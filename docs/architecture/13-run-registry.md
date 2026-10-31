# Run registry

The local store of run analytics. `backtest` and `run_hpo` write it when given `--record <PATH>`,
creating the file if needed; without the flag nothing is written. The implementation is
`crates/adapters/src/storage/sqlite_runs.rs`, behind the `ports::RunRecorder` port.

## Tables

- **`runs`**: lifecycle rows, append-only. The latest row of a `run_id` is the run's current state.
  Columns: `run_id`, `parent_run_id`, `baseline_run_id`, `run_type`, `status`, `recorded_at`,
  `started_at`, `finished_at`, `strategy_id`, `symbol`, `market_type`, `sampling`, `params` (JSON),
  `execution_context` (JSON), `fill_model`, `data_source`, `data_start`, `data_end`, `git_sha`.
- **`run_metrics`**: one row per run and scope (`full`, `is`, `oos`, `fold_<n>`). Columns: the
  trade counts, `win_rate`, the profit and loss totals, `total_fees`, `total_slippage`,
  `profit_factor`, `payoff_ratio`, `mathematical_expectancy`, the drawdown, `sortino_ratio`,
  `fee_drag_pct`, the holding and excursion averages, `exit_reason_counts` (JSON), `halted`,
  `rejected_orders` (JSON) and `extras` (JSON).
- **`trades`**: closed trades with their equity excursion. Columns: `trade_id`, `run_id`, `side`,
  entry and exit timestamps and prices, `quantity`, the gross, fee, funding, slippage and net PnL,
  `return_pct`, `exit_reason`, the holding time, the entry, trough, peak and exit equity with their
  timestamps, and the maximum drawdown.
- **`run_series`**: event-based points, such as the equity at each trade event or the Monte Carlo
  fan chart. Never time-sampled. Columns: `series`, `event`, `ts`, `x`, `value` and `trade_id`.

Every table has a `seq` column, an integer key that orders its rows in the order they were
written.

## Conventions

- Decimals are stored as text with at most nine fractional digits and trailing zeros removed. The
  text is exact; no float ever holds a value.
- Timestamps are RFC 3339 strings in UTC.
- Run ids are UUIDs (see `RunId`).
- Each call of the recorder writes one batch in one transaction, and returns only after the commit.

## Schema version

- `PRAGMA user_version` is 1. A store written with a higher version is refused, not misread.
- Changing or removing a column needs a new version and a migration. Adding a column is a new
  version too; readers select columns by name.

## Example

```sql
SELECT net_profit, total_fees, total_trades FROM run_metrics WHERE scope = 'full' ORDER BY seq DESC LIMIT 5;
```
