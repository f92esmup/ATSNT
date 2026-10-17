# Capital scaling guide

How position sizing, the safety caps and the hyperparameter search depend on the account size,
and what to change when the capital grows. The reference account is **500 EUR (about 562 USDT)**.
The EUR/USDT rate moves: recompute the USDT figures before every deployment.

## 1. How an order is sized

Everything below lives in `RiskPolicy::size_order` (`crates/domain/src/risk.rs`) and runs on the
**current marked-to-market equity** at the moment of the signal, not on the initial capital.

1. `risk_usdt = equity * risk_per_trade_pct`
2. `stop_distance = |entry_price - stop_loss|` (the stop comes from the strategy)
3. `quantity = risk_usdt / stop_distance`
4. The quantity is **clamped** to `max_leverage` and floored to the lot step.
5. The order is **rejected** (never clamped) when it breaks `min_order_quantity`,
   `min_order_notional`, `max_order_notional` or `max_position_notional`.

Consequence: a tight stop produces a large notional (`notional = risk_usdt / stop_pct`). The
notional caps are the last line against that and against a balance-reading bug; they reject.

## 2. Parameters that depend on the capital

All of them live in the artifact `execution_context`.

| Field | Meaning | Rule of thumb (reference: 500 EUR) |
|---|---|---|
| `initial_capital` | Capital the backtest and the HPO start with | the real capital in USDT (562) |
| `risk_per_trade_pct` | Fraction of equity lost when the stop is hit | 0.005 (0.5%) |
| `max_daily_drawdown_pct` | Daily halt, from the day's marked-equity peak (resets 00:00 UTC) | 0.03 (about 6 consecutive stops) |
| `max_monthly_drawdown_pct` | Permanent halt versus the equity at the start of the UTC month | 0.15 (default) |
| `max_leverage` | Effective leverage ceiling (default 20) | 5 |
| `max_order_notional` | Largest single order, in USDT | about 3x equity, rounded |
| `max_position_notional` | Largest position, in USDT | about 3x equity, rounded |
| `min_order_notional` | Exchange `MIN_NOTIONAL` (BTCUSDT: 50; the code default is 5) | 50 |
| `min_order_quantity`, `quantity_step_size` | Exchange `LOT_SIZE` (BTCUSDT: 0.001) | 0.001 / 0.001 |

The production run refuses to start without explicit notional caps (`live_trading`). A ready
example for the reference account is `configs/execution_real_500eur.json` (guarded by
`crates/backtest/tests/execution_config_contract.rs`).

### The cap formula

```
cap  ~=  risk_usdt / smallest_stop_pct_you_accept
```

With 2.81 USDT of risk and a cap of 1,700 USDT the cap only fires for stops tighter than about
0.17%. Those trades cannot beat the fees anyway (section 4), so the cap rejects exactly what you
do not want to trade.

Do **not** size the caps for a future, larger account: on the real 562 USDT a cap built for 2,250
USDT (2x = 4,500) would allow a single order of about 8x the equity and would stop protecting.
Raise the caps in the artifact when the capital grows.

## 3. Scaling table

Assumptions: risk 0.5%, daily drawdown 3%, caps about 3x equity rounded to the nearest 100,
EUR/USDT 1.125 (2026-10-17).

| Capital (EUR) | Equity (USDT) | Risk per trade | Daily loss halt | Order and position cap |
|---|---|---|---|---|
| 500 | 562 | 2.81 | 16.9 | 1,700 |
| 1,000 | 1,125 | 5.63 | 33.8 | 3,400 |
| 2,000 | 2,250 | 11.25 | 67.5 | 6,800 |
| 5,000 | 5,625 | 28.1 | 168.8 | 16,900 |

Lot granularity: at BTC about 81,000 USDT one lot (0.001 BTC) is about 81 USDT, so a 562 USDT
account sizes in steps of 14% of its equity. The quantization error shrinks as the capital grows.

## 4. Why fees decide the strategy

Round-trip cost as a fraction of the notional: `c = 2 * taker_fee + 2 * slippage`. With the
reference values (0.05% and 0.05%) `c = 0.2%`.

- Cost measured in risk units: `c / stop_pct`. A 0.5% stop loses 40% of its risk to costs.
- Break-even hit rate: `p = (s + c) / (t + s)` with take-profit distance `t` and stop distance `s`.
  With `t = s = 0.5%` you need 70%; with `t = 1%, s = 0.5%` you need about 47%.

That is why `min_take_profit_pct` is a searched hyperparameter (`min_take_profit_pcts` in the
space file). The slippage value is a placeholder until it is calibrated with real fills
(`parity_check`, roadmap phase 4).

## 5. Hyperparameter search

The shipped space is `configs/param_space_daytrading_cusum.json` (guarded by
`crates/backtest/tests/param_space_contract.rs`, which must equal `DollarBarsCusumSpace::default()`).
It searches the window, the CUSUM multiplier, the entry and stop z-scores and the take-profit
floor. `time_barrier_bars` is fixed on purpose so the budget goes to the edge; widen it in the file
if you want to search it. A space file written before `min_take_profit_pcts` existed keeps the
single legacy value.

## 6. What to re-optimize when the capital grows

- Below roughly 1,000 EUR, lot quantization and the caps change the results: **re-run the HPO**
  with the new `initial_capital` and caps.
- Above that, the hyperparameters are mostly independent of the capital: update the
  `execution_context`, re-run a **backtest** (not the HPO) with the same hyperparameters and
  compare it with the previous one.
- This split is a guideline, not a measured threshold; confirm it with the comparison backtest.

### Running the HPO for a given account

The pipeline forwards `--execution-context` to `run_hpo`, so the search simulates every trial with
that account's risk, drawdown, leverage and notional caps; the winning artifact carries the same
context into the backtest, paper and live runs. The Batch image bakes `configs/` in at build time,
so the file must be committed before the job is submitted.

```
scripts/gcp/run_ephemeral_batch_hpo.sh --project <project> --year 2026 -- \
  --execution-context configs/execution_real_500eur.json \
  --param-space configs/param_space_daytrading_cusum.json \
  --candidates 1500
```

`--capital` still overrides the capital of the file. The pipeline default is 60 candidates, which is
a very small random sample of a space of 10,290 combinations: raise it (the cost grows linearly).

## 7. Checklist before raising the capital

1. At least 100 closed trades and 4 weeks at the current level, without `[CRITICAL]` halts.
2. Net PnL after fees is positive and the realized drawdown stays under two thirds of the daily
   and monthly limits.
3. Simulator latency and slippage calibrated from real fills (`--record-fills`, `parity_check`).
4. Raise the capital by at most 2x per step.
5. Update the artifact (`initial_capital`, caps, risk), re-run the comparison backtest, and only
   then fund the account.
6. Keep `--allow-foreign-collateral` off: it exists for the testnet only.
