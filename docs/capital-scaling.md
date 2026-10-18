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
floor, and the holding horizon. The rolling window (100 to 1,600 bars, about 12 minutes to 3 hours)
sets the take-profit target, which is the rolling mean, so it spans the same intraday horizons:
windows of 14 to 40 bars (2 to 5 minutes) put the target closer than the take-profit floor and
almost never traded. The holding horizon `time_barrier_bars` (100 to 2,000 bars: about 12 minutes to 4 hours
at the 7 s per $1M bar measured on BTCUSDT 2026-01). The barrier counts bars, not time: a quiet
market stretches it, so it bounds the holding period on average, not as a clock limit. A space file written before `min_take_profit_pcts` existed keeps the
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
scripts/gcp/run_ephemeral_batch_hpo.sh --project <project> --year 2026 \
  --job-name hpo-btcusdt-2026-500eur-v2 \
  --machine-type n2-custom-30-30720 -- \
  --execution-context configs/execution_real_500eur.json \
  --param-space configs/param_space_daytrading_cusum.json \
  --holdout-from-month 9 --candidates 120
```

Use a new `--job-name` for every run: it names the uploaded winner
(`artifacts/hpo_btcusdt_2026-<job name>.json`), the upload never overwrites, and the script aborts
before building when that object already exists. It also aborts when `atsnt-batch` lacks
`bigquery.dataEditor` on the tables the pipeline writes (lost when the tables are recreated) and
prints the commands that re-grant it. A failed job is retried only after a Spot preemption.

`--holdout-from-month 9` keeps September and the current month out of the search: the HPO reads
January to August and the backtest of the winner reads only the held-out months, so that backtest is
an honest out-of-sample check. `--capital` still overrides the capital of the file.

Candidates: the space has 41,160 combinations and the HPO samples them at random. More candidates are
not better on their own: every extra trial raises the chance that the winner is luck, which the
Deflated Sharpe ratio penalizes. 120 candidates still try each value of each parameter dozens of
times. The cost grows linearly with candidates times bars.

### Reading the result

The search ranks by out-of-sample quality. See `docs/architecture/04-hpo-and-walk-forward.md`,
sections 2.2 to 2.4.

- **Eligible.** Only a candidate with at least 30 closed trades in every fold can win
  (`--min-trades-per-fold`). The job fails, and deploys nothing, when none reaches it.
- **Fitness.** Fitness is the mean out-of-sample Sortino per trade times the stability (the share
  of the edge that one-step neighbours keep). Per-trade Sortino values are small fractions: 0.1 is
  a good figure, not a poor one.
- **DSR p-value.** It says whether the winner beats the best result expected from luck among all
  candidates. A winner with p ≥ 0.05 is reported as not significant.
- **Deployment gate.** `paper_trading` and `live_trading` refuse a winner with fitness ≤ 0, a DSR
  p-value ≥ 0.05, or a scorecard without eligible folds (written before the minimum-trades rule,
  or by a search run with `--min-trades-per-fold 0`). Only `--allow-unproven-strategy` overrides them, for instance to test the
  mechanics on testnet. The pipeline still runs the holdout backtest of such a winner, so the
  evidence is complete.
- **Rejected signals** (`rejected_orders`). On a small account, `OrderSizeExceeded` means a stop
  so tight that the risk-based size exceeds the notional cap (section 2). Many of them mean that
  the account size, not the market, limits the strategy.

## 7. Checklist before raising the capital

1. At least 100 closed trades and 4 weeks at the current level, without `[CRITICAL]` halts.
2. Net PnL after fees is positive and the realized drawdown stays under two thirds of the daily
   and monthly limits.
3. Simulator latency and slippage calibrated from real fills (`--record-fills`, `parity_check`).
4. Raise the capital by at most 2x per step.
5. Update the artifact (`initial_capital`, caps, risk), re-run the comparison backtest, and only
   then fund the account.
6. Keep `--allow-foreign-collateral` off: it exists for the testnet only.
