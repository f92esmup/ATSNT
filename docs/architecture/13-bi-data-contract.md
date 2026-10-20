# BI data contract (`atsnt_bi`)

The `atsnt_bi.v_*` views are the public interface of ATSNT's analytics. Anything outside the
engine reads them and nothing else: the engine ships no dashboard (the Looker Studio report and the
in-process web workspace were retired). **This document is canonical.** Consumers pin a copy of the
columns they read and verify it at run time.

Contract version: **2** (2026-10-18; version 1 had 119 columns pinned by the first consumer, see
section 8 for what changed). Source of truth for the DDL:
`scripts/gcp/bigquery_schema.sql`. Consumer-requested ETL changes are listed in section 7.

## 1. Rules of change

1. Views `atsnt_bi.v_*` are the only public surface. Consumers never read the raw tables.
2. **Additive changes are free**: a new column or a new view does not break a consumer that selects
   its columns by name.
3. **Removing or renaming a column, or changing its type, is a breaking change.** A consumer
   then shows "contract not satisfied" instead of figures. Announce it, bump the contract version
   and update the pin in the same change.
4. Changing the *meaning* of a column (unit, scope, rounding, sentinel) is breaking even when
   the type is unchanged. Record it here.
5. Completed runs are immutable: ATSNT only appends. A consumer should warn when a completed run
   changes between two loads.

## 2. Conventions

| Topic | Convention |
|---|---|
| Time | `TIMESTAMP`, UTC. Consumers should request integer microseconds (`useInt64Timestamp`). |
| Money-like values | `NUMERIC` (9 decimals), in the run's quote currency. **The currency is not recorded** (gap G2); `BTCUSDT` implies USDT. Never summed across runs without normalising by `initial_capital`. |
| Percentages | Fractions: `0.154` is 15.4 %. Columns ending in `_pct`. |
| Nullability | **Every column is `NULLABLE` in BigQuery**, including keys. A consumer enforces *required* per column (section 4); a null in a required cell makes that row an issue shown on the page, never a zero. |
| Enumerations | `run_type`: `backtest`, `hpo`, `hpo_trial`, `monte_carlo`, `paper`, `live`. `status`: `running`, `completed`, `failed`, `stopped`. `side`: `Long`, `Short`. `event` (equity): `entry`, `trough`, `peak`, `exit`. `sample`: `is`, `oos`. An unknown value is an issue on that row. `exit_reason` is an open set and is shown as text; known values are `StopLoss`, `TakeProfit`, `TimeBarrier`, `Liquidation`, `MonthlyCircuitBreaker`, `EndOfSimulation`, and, for target-position strategies, `TargetExit` (the position returned to flat: a target of 0, a level that sizes to a quantity of 0, or a flip whose opening leg was not placed because it was dust, above `max_order_notional`, unmarginable or clipped to nothing) and `TargetFlip` (the campaign ended because the target flipped side and the opposite position opened; the next trade opens on the same fill time). A target-position trade is one campaign from flat to flat or to a flip: `entry_price` and `exit_price` are the VWAPs of its increasing and reducing fills and `quantity` is its peak absolute quantity. Its `slippage_paid` follows the entry convention: each increasing fill reports `open × slippage_pct` plus the adverse gap from the decision price the target was sized at, and each reducing fill reports `open × slippage_pct`; it is informative and already inside the fill prices, so `net_pnl` does not deduct it. |
| Location | Dataset `atsnt_bi` in `europe-southwest1`. Jobs must name the location. |

## 3. Views and semantics consumers rely on

| View | Grain | Notes |
|---|---|---|
| `v_runs` | one row per run (HPO trials included) | Latest lifecycle row joined with the `full` metrics. Metrics are written **only when the run completes**: a running run has `total_trades` NULL and no metrics. Monte Carlo and HPO runs never have trade metrics. |
| `v_trades` | one row per closed trade | `trough_equity`/`peak_equity` are the equity extremes while the trade was open (MAE/MFE). |
| `v_equity` | one row per equity event | Four events per trade. `running_peak` starts at `initial_capital`. |
| `v_series` | one row per series point | Monte Carlo curves are keyed by **the Monte Carlo run's own `run_id`**, series `mc_equity`, `event` = curve name (`Realized`, `Quantile_0.05` ... `Worst_Stress_1`, `Best_Ceiling_1`), `x` = trade ordinal. |
| `v_hpo_trials` | one row per optimisation trial | `hpo_run_id` is the parent optimisation run; `rank` is 1-based. Eligible trials rank first. `eligible` is TRUE when every fold reached the minimum of closed trades; `min_fold_trades` is the fewest trades in any fold. `fitness` is the mean OOS Sortino times `stability_score` (0 for ineligible trials). `dsr_p_value` is the Deflated Sharpe p-value of the per-trade OOS Sharpe `oos_sharpe` (see docs/architecture/04-hpo-and-walk-forward.md). |
| `v_hpo_folds` | one row per trial, fold and sample | `sample` is `is` or `oos`. |
| `v_monte_carlo` | one row per Monte Carlo run | `backtest_run_id` links to the backtest it stresses. |

`v_latest_equity` is **not used**: its latest event is a `trough` or `peak` of the last trade
(ordering by `event DESC`), not the closing equity of the run.

Relations that must hold (checked by the first consumer on every run it shows):

1. `total_trades` = number of rows of `v_trades` for the run.
2. `winning_trades` = trades with `net_pnl > 0`.
3. `net_profit` = sum of the trades' `net_pnl`, within one unit of the ninth decimal per trade
   (observed: 1,145 trades differ by 4e-9).
4. `max_drawdown_pct` = max of the trades' `max_drawdown_pct` (declared in the DDL).
5. `exit_equity` of the last trade = `initial_capital + net_profit`.

Values that are not measurements (flagged by the first consumer, see gap G6): a drawdown above `1`
(100 %). Since version 2, `sortino_ratio` and `profit_factor` are NULL where they are undefined
instead of a `100` cap (G5); rows written before 2026-10-18 still carry the cap.

Formulas (version 2):
- `sortino_ratio` is per trade. The return of a trade is `r = net_pnl / equity at entry`, and
  `sortino_ratio = mean(r) / sqrt(sum(min(r, 0)^2) / N)` over all N trades of the scope. It is NULL
  without trades or without a losing trade. Typical values are fractions (about -0.5 to 0.5), not
  annualised figures.
- `profit_factor = gross_profit / gross_loss`; NULL without a losing trade.
- `rejected_orders` (JSON, `run_metrics`, through `v_runs` and `v_hpo_folds`) counts the signals the
  risk policy did not turn into orders, per reason (`OrderSizeExceeded`, `OrderBelowMinLotSize`,
  `DailyHalt`, ...). It is NULL when nothing was rejected. Target-position strategies add
  `RearmIgnored` (a target ignored after a forced exit on its side) and two **adjustment
  counters** that count orders that were reduced and still traded, not rejections:
  `PositionCapClip` (clipped at `max_position_notional`) and `StopBeyondLiquidation` (reduced
  so the stop trades before the liquidation price). A target delta below the minimum lot or
  notional is counted under `OrderBelowMinLotSize` / `OrderBelowMinNotional`, and one above
  `max_order_notional` under `OrderSizeExceeded`, as entries are.

## 4. Required columns

A row missing any of these is reported as an issue and excluded from figures.

| View | Required |
|---|---|
| `v_runs` | `run_id`, `run_type`, `status`, `recorded_at`, `started_at`, `strategy_id`, `symbol`, `market_type` |
| `v_trades` | all consumed columns |
| `v_equity` | `run_id`, `ts`, `event`, `equity`, `running_peak`, `drawdown_amount` |
| `v_series` | `run_id`, `series`, `event`, `value` |
| `v_hpo_trials` | `run_id`, `hpo_run_id`, `strategy_id`, `symbol`, `started_at`, `rank` |
| `v_hpo_folds` | `run_id`, `hpo_run_id`, `sample`, `fold_idx` |
| `v_monte_carlo` | `run_id`, `strategy_id`, `symbol`, `started_at` |

The consumed columns and their types are the `PINNED` list in
the consumer's own pinned list (a unit test in the consumer keeps it identical to its models).

## 5. How a consumer can enforce it

The first consumer, the `/trading` module of Econoweb (a separate repository), keeps a pinned copy
of the columns and types it reads and enforces this contract itself:

* **At run time**: one `INFORMATION_SCHEMA.COLUMNS` read per refresh compares the live views with
  its pin. A missing column or a changed type marks its module "contract not satisfied" instead of
  showing figures.
* **Per read**: strict parsing (section 2) and statements that select explicit columns, never `*`.
* **Per run**: reconciliation of the five relations in section 3.
* **In CI**: recorded, scrubbed responses of every view as fixtures.

ATSNT should run the same comparison before changing a view: if a pinned column would disappear or
change type, that is a breaking change (section 1).

## 6. Access

* Identity: Cloud Run service account (metadata server), or `gcloud` / `GCP_AUTH_TOKEN` locally.
* Roles: `roles/bigquery.jobUser` on the billing project and `roles/bigquery.dataViewer` on the
  dataset `atsnt_bi` only (not on the whole project).
* Cost: a full scan of any view is under 2 MB; BigQuery bills the 10 MiB minimum per table
  touched. Nothing queries in the background: reads happen when a page needs data that is
  missing or older than the cache TTL, or on "Actualizar datos".

## 7. ETL backlog requested by consumers

What the views lack, found by the first consumer (reviewed against the real dataset on 2026-10-16:
4 backtests, 2 optimisations with 180 trials, 3 Monte Carlo runs, 1 running paper run, no live run).
Priority: **High** blocks a decision the consumer is meant to support, **Medium** weakens it,
**Low** is a convenience. Status is kept here.

| # | Priority | Gap | Why it matters | Proposed change | Status |
|---|---|---|---|---|---|
| G1 | High | A paper or live run does not record which backtest justified it. `parent_run_id` only links `hpo -> hpo_trial` and `backtest -> monte_carlo`. | The most valuable check is "is the live run behaving inside the Monte Carlo band of its baseline?". Guessing the baseline from parameters could silently pair the wrong runs. | Add `baseline_run_id STRING` to `runs` and expose it in `v_runs`; `paper_trading` and `live_trading` take `--baseline-run-id <backtest run id>` (validated as a UUID v7). The Monte Carlo run of that backtest is the one whose `parent_run_id` is the baseline. Rows written without the flag leave the column NULL. | Closed in the engine; deployed datasets need `bigquery_schema.sql` re-run (it adds the column and refreshes `v_runs`). |
| G2 | High | The currency of `initial_capital`, `net_profit`, fees and equity is not recorded (only implied by `symbol`, e.g. `BTCUSDT`). | Without it a figure cannot be shown with its unit, and runs of different quote assets cannot be told apart. | Add `quote_currency STRING` (`USDT`, ...) to `runs`/`v_runs`. | Open |
| G3 | Medium | The deployed `v_runs` has no `halted` column, although the DDL declares it (`ALTER TABLE ... ADD COLUMN IF NOT EXISTS halted`). | The monthly kill switch is key to judge a run: a run stopped by the kill switch is not comparable to one that ran to the end. | Re-run the DDL setup so the deployed dataset matches the DDL; Consumers then add `halted` to their pin. | Closed: the DDL was re-run on the deployed dataset on 2026-10-18. |
| G4 | Medium | Metrics are written only when a run completes, so a running run (the paper run) has no `total_trades`, `net_profit`, Sortino... | The runs that matter most for ongoing decisions (paper/live) have no scorecards until they end. | Write interim `full` metrics periodically for running runs. Until then consumers derive what they can from `v_trades`. | Open |
| G5 | Medium | `sortino_ratio` and `profit_factor` use `100` as a cap (5-trade backtest: both exactly `100`); `sortino_ratio` also reaches `-345.83` on the ruined run. | A cap looks like an excellent measurement. | Return NULL (or add `sortino_saturated BOOL`) when the ratio is undefined; document the formula and its bounds. | Closed in version 2: NULL when undefined; formulas in section 3. |
| G6 | Medium | Monte Carlo reports `p95_max_drawdown_pct = 1.2636` (126 %) for the ruined backtest. | A drawdown above 100 % of equity is not a physical quantity for a long-only account. | Clarify the simulation (leverage? negative equity?) or clamp at 1 and report ruin separately. | Open |
| G7 | Medium | No benchmark series (buy and hold of the symbol over the run period). | "Does the strategy beat holding?" cannot be answered. | A view `v_benchmark(run_id, ts, value)` or the price series of the run period. | Open |
| G8 | Medium | Walk-forward detail is thin: only Sortino-level metrics per fold; no fold date windows; no per-trial out-of-sample returns. | The probability of backtest overfitting (PBO/CSCV) needs per-trial return series; fold windows are needed to read degradation over time. | Expose `train_start/train_end/test_start/test_end` per fold, and OOS returns (or per-fold trade returns) per trial. | Open |
| G9 | Medium | No link from an optimisation to the backtest that realised its rank-1 parameters (the winner travels as a GCS JSON). | The run x-ray cannot show "this backtest came from trial #1 of optimisation X, which ranked among N trials". | Add `source_hpo_run_id` / `source_trial_run_id` to the backtest's `runs` row. | Open |
| G10 | Medium | Deposits and withdrawals of the trading account are not recorded; equity exists only at trade events (no mark-to-market series, no open P&L). | A live run's equity cannot be reconciled with the exchange balance, so it cannot be trusted as the account's value. | Record equity snapshots (`ts`, equity, open P&L) and account flows for paper/live runs. | Open |
| G11 | Low | No consultable contract version. | A consumer checks columns and types, not the intent of a change. | A one-row view `v_contract_version(version, changed_at)` maintained with this contract. | Open |
| G12 | Low | `net_profit` and `total_slippage` differ from the sums of the trades by a few units of the ninth decimal (1,145 trades: 4e-9 on net profit; 3 trades: 1e-9 on slippage). | Information only; the first consumer tolerates one unit per trade and shows the difference. | Document which side is computed in which precision. | Open |
| G13 | Low | `execution_context` JSON carries `risk_per_trade_pct` and `max_daily_drawdown_pct`; they are not flattened in `v_runs`. | None: the first consumer parses the JSON. | No action. | Closed |
| G14 | Medium | Units are inconsistent: `probability_of_ruin_pct` is 0-100 (a certain ruin stores `100`) while every other `*_pct` column is a fraction (`0.3` is 30 %, `ruin_threshold_pct` included). | A reader of the raw views, or a future consumer, can mistake 100 for 10,000 %. The first consumer special-cases it. | Store `probability_of_ruin_pct` as a fraction like the rest, or rename it to say its unit. | Open |
| G15 | Medium | HPO trial and fold data may be degenerate without saying so: an optimisation with no trades (60 trials, all `0`, DSR p = 1) looks like any other. | A consumer would present zeros as results: it can read "0 of N viable" but cannot tell "no trades" from "all lost". | Record the number of trades per trial and a status (`no_trades`, `completed`) on the optimisation run. | Closed: `v_hpo_trials` has `total_oos_trades`, `min_fold_trades` and `eligible`; an optimisation without an eligible trial is recorded as `completed` with every trial `eligible = FALSE`, and the pipeline deploys no winner. |

## 8. Version history

| Version | Date | Change | Breaking for |
|---|---|---|---|
| 2 | 2026-10-18 | `sortino_ratio` is per trade on entry equity and NULL when undefined; `profit_factor` is NULL without losses (both were capped at `100`). HPO `fitness` uses the OOS Sortino and the plateau stability; `dsr_p_value` is a real Deflated Sharpe p-value. New columns: `run_metrics.rejected_orders` (JSON), `v_hpo_trials.min_fold_trades`, `eligible`, `oos_sharpe`. | Consumers that read `sortino_ratio`, `profit_factor`, `fitness`, `stability_score` or `dsr_p_value` as comparable to version 1 rows: compare only rows recorded since 2026-10-18. |
| 1 | 2026-10 | First pinned contract. | - |
