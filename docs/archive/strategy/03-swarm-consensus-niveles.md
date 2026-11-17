# Swarm Consensus: the level machine and target-position execution (history)

> **Status: removed (2026-11).** This text was Sections 3.2, 3.3, 10, 11 and 18 of
> [`../../strategy/03-swarm-consensus.md`](../../strategy/03-swarm-consensus.md) while the swarm
> had two policy modes. The target-level policy (`classifier_mode: false`), `Decision::Target`,
> `TargetPosition`, scale-in and scale-out, flips, the re-arm rule, `emits_targets` and
> `bracket_priority` no longer exist in the code. The current specification describes the
> classifier only. Git history has the code.

### 3.2 One decision type

```rust
// crates/domain
pub enum Decision {
    /// Triple-barrier entry: accepted only when flat with nothing pending.
    Entry(OrderIntent),
    /// Signed exposure level: accepted at any time; replaces a pending target.
    Target(TargetPosition),
}

pub struct ProtectiveExits {
    pub stop: Decimal,                // always present (disaster or regular stop)
    pub take_profit: Option<Decimal>, // Some for Entry, None for Target
    pub max_bars: Option<usize>,      // Some for Entry, None for Target
}
```

Both variants become one `ExecutionPlan { target_quantity (signed), exits: ProtectiveExits }`
through one domain planner:

- **Entry(intent)**: quantity from `size_order`, which is unchanged. Sign from `intent.side`.
  Exits are `{intent.stop_loss, Some(intent.take_profit), Some(intent.max_bars_hold)}`.
- **Target(t)**: quantity from `size_target`. Sign from `t.level`. Exits are
  `{fill ∓ t.stop_distance, None, None}`.

The two kinds are not mixed in one position. A `Target` that arrives while a triple-barrier
`Entry` position is open or pending is ignored and counted `UnsupportedDecision`; an `Entry` that
arrives while a target is open or pending is ignored, as any entry is when not flat (Section 11.2).

`size_order` and `size_target` share one private quantity core. That core keeps the operation
order of today's `size_order` (`risk.rs:254-259`):

1. `(equity·risk) / stop_distance`;
2. the minimum with `(equity·max_leverage) / price`;
3. floor to the step.

So every existing sizing result stays identical.

### 3.3 One strategy contract

```rust
// crates/domain
pub enum MarketEvent<'a> { Trade(&'a Trade), DollarBar(&'a DollarBar) }
pub enum InputKind { DollarBars, Trades }

// crates/strategies
pub trait Strategy: Send {
    fn name(&self) -> &str;
    fn compatible_market_types(&self) -> &'static [MarketType];
    fn input_kind(&self) -> InputKind;
    fn on_event(&mut self, event: &MarketEvent) -> Option<Decision>;
    fn reset(&mut self);
    /// Capability used by the paper/live product gate (Section 17.1). Default: false.
    fn emits_targets(&self) -> bool { false }
    /// Strategy-specific audit written into reports as `strategy_audit`. Default: None.
    fn audit(&self) -> Option<serde_json::Value> { None }
}
```

- Runners prepare the input the strategy declares. `DollarBars` keeps today's
  `DollarBarAggregator` path, byte for byte. `Trades` passes raw trades.
- `Strategy::on_bar` stays, with a default that decides nothing. The default `on_event` sends a
  dollar bar to `on_bar` and wraps its intent in `Decision::Entry`, and ignores raw trades. The
  default `input_kind` is `DollarBars`.
- The removed `dollar_bars_cusum` and `order_flow_imbalance` kept their `on_bar` code
  **unedited**, as any bar-based strategy does under this contract. Only a strategy that consumes
  trades or emits targets overrides `input_kind` and `on_event`.
- Runners select behaviour by `input_kind()` and by the `Decision` variant, never by strategy name
  (`architecture_contract.rs` keeps enforcing this).


## 10. The Consensus Policy (Stage B)

### 10.1 Thresholds

Configured as fractions of `N` (the original used absolute votes out of 100; fractions keep the
meaning when `agents_per_feed` changes):

- `L1 = threshold_base`;
- `L2 = threshold_base + threshold_step`;
- `L3 = threshold_base + 2·threshold_step`;
- hysteresis `H`.

Validation: `0 < threshold_base`, `0 < threshold_step`, `L3 ≤ 1`, and `0 ≤ H`. `H > L1` is allowed,
as in the original: a level-1 position then survives until the net vote turns clearly against it.

### 10.2 One axis: the original level machine

`axis_update(current ∈ {0,1,2,3}, net) -> {0,1,2,3}`, reproduced from `CRiskManager::GetTradeParams`:

```text
t = current
if net >= L3                 { t = 3 }
else if net >= L2 && t < 2   { t = 2 }
else if net >= L1 && t < 1   { t = 1 }
if t == 3 && net < L3 - H    { t = 2 }
if t == 2 && net < L2 - H    { t = 1 }
if t == 1 && net < L1 - H    { t = 0 }
return t
```

Moving up is immediate, and a jump from 0 to 3 is allowed. Moving down needs `H` extra votes
against, and may cascade over several levels in one update. This asymmetry limits overtrading.

### 10.3 Two axes and the signed target

- `long_level = axis_update(long_level, long_net)`.
- `short_level = axis_update(short_level, short_net)` if `allow_short`, otherwise 0.
- Target level `k`:
  - if `long_level > 0` and `short_level > 0`, then `k = 0` (the swarm contradicts itself: stand
    aside, consistent with "conflict means wait");
  - otherwise `k = long_level - short_level`.

Both axes keep their own state across samples, including during a conflict.

### 10.4 Risk per level

`risk_fraction(|k|) = base_risk + (|k| - 1) · risk_increment` for `|k| ≥ 1`, and `0` for `k = 0`.
This is the original rule. With the thesis-selected 1% and 2%, level 3 risks 5% of equity, so the
search space bounds keep the user's account policy in mind (Section 14.2).

### 10.5 Protective exits: Stop loss, Take Profit, and Temporal Decay

- **Stop distance**: `stop_distance = stop_atr_multiplier · atr_stop_reference`. In HPO, `stop_atr_multiplier` is searched in `[1.5, 2.0, 2.5, 3.0, 4.0]` (anchored on feed 1 H1 ATR).
- **Take-Profit (TP)**: `take_profit_distance = take_profit_atr_multiplier · atr_stop_reference` (optional). When configured (e.g. `[None, 2.0, 3.0, 4.0, 5.0]`), the engine arms a protective take-profit barrier on the winning side (`fill + tp` for long, `fill - tp` for short).
- **Anti-whipsaw protection (`rearm_blocked`)**: When a target position exits on `TakeProfit` (or `StopLoss`), the execution core blocks re-entering on that same side until the consensus returns to flat (`k = 0`) or flips. This prevents buying at the absolute top immediately after collecting profits while the swarm vote remains momentarily high.
- **Temporal decay (`decay_horizon_seconds`)**: If configured, once a level has been held past `decay_horizon_seconds`, the policy forces a flat target exit (`k = 0`) to prevent holding stagnant positions across market regimes.

### 10.6 Emission

The policy emits `Decision::Target(TargetPosition::with_exits(timestamp, level: k, risk_fraction, stop_distance, take_profit_distance))` **only when `k` changes**, including a return to 0. The original recomputed the target lot on every
update, so changes in balance or ATR alone triggered small rebalancing orders that paid fees for
nothing (correction C6). ATSNT sizes once per level change.

---

## 11. Execution of a Target Decision

These rules are part of the single execution model (Section 3). They are written for
`ExecutionCore`, and `TradingSession` applies the same rules through the venue ports
(Section 11.10). Nothing here changes how `Decision::Entry` is executed.

### 11.1 Price events

The core consumes **price events**: OHLC buckets of trades, with the timestamps of their first
(`start_ms`) and last (`end_ms`) trade; a dollar bar maps to its own start and end times.

| Run | Price event | Source |
| :--- | :--- | :--- |
| Backtest, `--fill-model bar` | one trade (trade resolution) for `Trades` strategies; one dollar bar for `DollarBars` strategies | the runner |
| HPO | a `price_bucket_ms` bucket (default 1,000 ms) for `Trades` strategies; one dollar bar for `DollarBars` strategies | the tape (Section 15) |
| Backtest, `--fill-model tick` | none: the `SimulatedVenue` matches orders against the tape | the venue |

### 11.2 Ordering per price event

The order is that of `BacktestEngine::process_bar` (`engine.rs:228-351`). It is unchanged and shared
by both decision kinds:

1. Day and month rollover.
2. Funding settlement.
3. Fill the pending order if it is due (`due_ms ≤ event start`).
4. Liquidation, then protective exits (stop, then take profit and time limit when present).
5. Monthly kill switch.
6. The strategy step: the strategy sees the event (trade or bar) and may return a `Decision`.
7. Mark to market, peak and drawdown, daily breaker.
8. Excursion.

A decision is **scheduled**, never filled on the event that produced it:

- `due_ms = decision timestamp + latency_ms`.
- `latency_ms` is a new optional field of `BacktestConfig`. It defaults to 0, so existing
  execution contexts keep filling at the next bar open as today. The swarm's execution context
  sets 100, the paper-session default (`paper.rs:147`).
- A newer `Target` replaces a pending `Target`.
- An `Entry` is still ignored unless the strategy is flat with nothing pending (today's rule at
  `engine.rs:302-306`).
- A `Target` that arrives while a triple-barrier `Entry` position is open or pending is ignored
  and counted `UnsupportedDecision` in `rejected_orders`: mixing decision kinds in one position is
  not supported.

### 11.3 Sizing at decision time

Both kinds are sized **when the decision is made**, as the engine does for entries today: the
reference price is the decision price, and the equity is `total_equity(decision price)`, which is
what `submit_entry` uses at `engine.rs:516`. For a `Target`:

- `q_raw = equity · risk_fraction / stop_distance`;
- the leverage cap is `equity · max_leverage / price`;
- the result is floored to `quantity_step_size`. All three steps share the core of `size_order`.
- `max_position_notional`, when set, **clips** `q_raw` instead of rejecting it, and counts a
  `position_cap_clip`.
- The signed target is `q* = sign(k) · q_raw`. The order is `Δ = q* - q_current`, evaluated when the
  order is filled, so earlier fills are taken into account.

### 11.4 Orders and fills

- All orders are market orders (taker), as in the original.
- Fill price: event open `± open · slippage_pct`, against the order side. The operation shapes are
  the engine's (`open + open·pct` on a buy, `ref - ref·pct` on a sell), so the shared code gives
  identical numbers on entries.
- Fee: `(fill_px · |q|) · taker_fee_pct`.
- Reported slippage (`slippage_paid`, informative only: the fill price already includes it, so
  cash and PnL never deduct it again) uses the entry convention. An increasing fill reports
  `open · slippage_pct` plus the adverse part of the gap from the decision price (the deciding
  event's close the target was sized at), per unit; a gap in favour does not cancel the slippage.
  A reducing fill, the close of a flip, and a flat target are market exits and report
  `open · slippage_pct` per unit, as an entry's market exit does.
- **Dust**: if `|Δ| < min_order_quantity` or `|Δ| · price < min_order_notional`, no order is sent
  and the decision is counted in `rejected_orders` under the existing `RiskError` kinds. A target of
  0 always closes the whole position, whatever its size.
- **Flip** (`q*` and `q_current` non-zero with opposite signs): two fills on the same event. The
  first is a close of `q_current`, which ends the campaign. The second opens `q*`, which starts a
  new one. Each pays its own fee and slippage.
- **Scale-in** and **scale-out** go through `domain::Position::apply_fill`. Scale-in averages the
  entry price; scale-out realises PnL on the reduced quantity. A full close uses the engine's
  existing close arithmetic, so entry-and-exit trades stay identical.
- Margin is recomputed after every fill with the engine's rule (`minimum_leverage` on the new
  notional; margin `= notional / leverage`).

### 11.5 Protective exits for targets

- `ProtectiveExits { stop, take_profit: None, max_bars: None }`. There is one stop for the whole
  position (correction C7).
- After **every** fill that leaves a position open, the stop is recomputed:
  `stop = last_fill_price ∓ stop_distance`. The `stop_distance` is the one carried by the target
  being executed.
- Sizing (11.3) and the stop use the same distance. A stop-out therefore loses about
  `risk_fraction` of the equity at decision time, plus the unrealised PnL carried from earlier
  fills, plus costs.

### 11.6 Stop and liquidation execution

These are the engine's existing exit evaluation (`engine.rs:439-501`), extracted into one pure
function with optional take profit and time limit:

- Long: if `event.low ≤ stop`, the fill is `min(event.open, stop)` minus slippage. Short: if
  `event.high ≥ stop`, the fill is `max(event.open, stop)` plus slippage. The `open` term models a
  gap through the stop.
- Liquidation comes first. It is evaluated with the existing rule (`liquidate_if_due`,
  `engine.rs:396-437`), whose stop-before-liquidation check now reads `exits.stop` instead of
  `intent.stop_loss`. The value is the same for entries.
- If an increasing target fill would leave the liquidation price between the entry and the stop,
  the order is reduced, in whole lot steps, to the largest quantity whose stop trades first, and a
  `stop_beyond_liquidation` clip is counted. The clip is applied **before** the fill: the reduced
  order is the one that fills, so no oversized position ever exists. A clip to less than one lot
  places nothing.
- Exit reason strings reuse the existing ones: `StopLoss`, `Liquidation`, `MonthlyCircuitBreaker`,
  `EndOfSimulation`. Two strings are added (Section 11.9).

### 11.7 Re-arm after a forced exit

After a stop, a liquidation, or a kill-switch flatten on side `s`, the policy still holds its
level. Without a rule, the next same-sign target would re-enter at once.

**Rule**: after a forced exit, every target with the sign of `s` is ignored, and counted, until a
target of 0 or of the opposite sign arrives (correction C12). With hysteresis, this means the net
vote has to fall below `L1 - H` on that axis first.

### 11.8 Breakers

The existing breakers apply unchanged. Only their effect on a target is defined:

- **Daily breaker** (the gate today at `engine.rs:518`): while it is tripped, a target may only
  reduce `|q|`. An increase is counted under `DAILY_HALT_REJECTION`, as an entry is today.
- **Monthly kill switch** (`engine.rs:271-289`): the existing flatten and halt.

### 11.9 Campaigns and the trade ledger

The metrics pipeline (Sortino, profit factor, DSR, minimum trades per fold) works on
`ClosedTrade`s (`metrics.rs:10`). The `TradeLedger` builds them, for both decision kinds:

- A **campaign** opens when the position leaves 0 and closes when it returns to 0 or flips. For an
  `Entry`, the campaign is exactly today's trade, with one opening and one closing fill.
- `entry_price`: VWAP of the increasing fills. `exit_price`: VWAP of the reducing fills.
- `quantity`: the peak absolute quantity.
- `pnl_gross`: the sum of realised PnL. Fees, slippage, and funding: the campaign sums.
- `return_pct`, holding time, and equity fields: today's formulas.
- New `exit_reason` strings:
  - `TargetExit`: the position returned to 0. This includes a target whose level sizes to a
    quantity of 0 (it closes the position), and a flip whose opening leg is not placed (dust,
    `OrderSizeExceeded`, unmarginable, or clipped to nothing): the position ended flat;
  - `TargetFlip`: the campaign ended in a flip whose opening leg filled on the same event.
  Both are additive to the run registry's string domain and are documented in
  `docs/architecture/13-run-registry.md`.
- A level change inside a campaign is **not** a trade. Counting each one would multiply the trade
  count and inflate significance. The fill count and the peak level per campaign go to the
  strategy audit (Section 16), not to `ClosedTrade`, whose schema does not change.

### 11.10 The same rules on the session path

`TradingSession` (tick simulation, and later paper and live) executes a `Target` like this:

1. Size it with the same planner. Equity is the venue wallet, as for entries today.
2. Send `Δ` with `submit_order`. A reduction uses `reduce_only = true`. A flip sends the reduce-only
   close first, then the opening order once the close is filled.
3. After each fill, re-place the stop with `cancel_orders` + `place_exits(symbol, side, stop, None)`.
4. Feed every fill to the shared `TradeLedger`.

Reports carry no order id. The session attributes each fill by its phase, as it does today. A new
phase, `Adjusting { delta }`, covers an open position with an order in flight. A fill on the
position side is an increase, and a fill on the opposite side is a reduction.

---


## 18. Worked Example of the Level Machine

Settings: `L1 = 0.50`, `L2 = 0.55`, `L3 = 0.60`, `H = 0.25`, `allow_short = true`.

| Sample | long_net | short_net | long_level | short_level | k | Emitted |
| :-: | :-: | :-: | :-: | :-: | :-: | :--- |
| 1 | 0.20 | 0.00 | 0 | 0 | 0 | none (unchanged) |
| 2 | 0.52 | 0.00 | 1 | 0 | +1 | target +1 |
| 3 | 0.61 | 0.00 | 3 | 0 | +3 | target +3 (jump) |
| 4 | 0.40 | 0.00 | 3 | 0 | +3 | none: 0.40 is not below `L3 - H` = 0.35 |
| 5 | 0.20 | 0.10 | 0 | 0 | 0 | target 0: cascade 3→2→1→0 (0.20 < 0.35, < 0.30, < 0.25) |
| 6 | −0.10 | 0.52 | 0 | 1 | −1 | target −1 |
| 7 | 0.55 | 0.55 | 2 | 2 | 0 | target 0 (contradiction) |

Sample 4 shows the hysteresis: the long net vote fell from 0.61 to 0.40, but the level stays at 3
until it falls below 0.35. Sample 5 drops all three levels in one update.
