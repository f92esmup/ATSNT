# Data Model Refactor — Unified Run Contract for Analytics

Objective: replace the per-binary, ad-hoc persistence (local JSON reports, GCS copies, MC Parquet, polled equity snapshots, mislabeled HPO rows) with one run-centric BigQuery model that new strategies plug into without schema changes.
Branch: `feat/data-model-refactor` | Delivery: ask-on-risk (forecast > 400 lines → chained PRs) | Test runner: `cargo test --workspace`, plus `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.

## Specs

- **S1 — No useless artifacts.** User: "si no les voy a dar uso, no veo el sentido de guardarlos en gcloud storage". Remove local `storage/reports/*.json` writes, GCS uploads of those reports, and the Monte Carlo trajectory Parquet export. Keep `StrategyDeploymentArtifact` (consumed by deploy, paper, and live) and its GCS upload.
- **S2 — Common data contract.** User: "que contrato de datos establecemos como comun para todos? Porque al final buscamos una estructura capaz de que se añadan nuevas estrategias sin fricción [...] hiperparámetros optimizables. fees y etc. para que no se decida hardcodeado". Every run records a `RunSpec` (strategy_id, symbol, market_type, sampling, strategy params as JSON, execution_context with fees/slippage/risk) plus run metadata (run_id, parent_run_id, run_type, data window, git sha). The deployment artifact is `RunSpec + HpoScorecard` and existing artifact JSON keeps loading. No hardcoded fees, resample methods, or placeholder metric values.
- **S3 — BigQuery model.** Tables `runs`, `run_metrics` (scope full|is_fold_N|oos_fold_N + extras JSON), `trades` (with run_id, run_type, MAE/MFE), `run_series` (event points). Views `v_runs`, `v_trades`, `v_equity` (drawdown derived via window function), `v_latest_equity`. Tables are append-only; views select the latest row per key (streaming buffer forbids UPDATE).
- **S4 — Event-based equity, never time-based.** User: "mi cuenta solo va a cambiar cuando se opera. a si que muestreos por tiempo no tienen sentido. pero guardar el estado exacto que se realiza cuando hay flujo de dinero, en cada trade en entrada y salida". Per trade persist four equity points: entry, trough (MAE), peak (MFE), exit, each with timestamp. The max drawdown reconstructed from these points must equal the engine's mark-to-market `max_drawdown_pct`. Remove the 1 s / 5 s MTM snapshots and the per-bar snapshot.
- **S5 — What each run type stores.** User: "en backtest y hpo y montecarlo. ahi tampoco tiene sentido guardar cada trade exacto". Backtest: run, metrics, trades, equity events. HPO: parent run + one `hpo_trial` child run per candidate with real IS/OOS metrics only (no trades, no series). Monte Carlo: child run of its backtest with percentiles in metrics extras and fan chart in `run_series` (no trades). Paper/live: run, trades, equity events; live must stop hardcoding `unrealized_pnl = 0` and position `None`.
- **S6 — Old BigQuery objects are dropped.** User: "las tablas viejas se pueden eliminar". Drop `equity_snapshots`, `hpo_evaluations`, `monte_carlo_runs`, `v_live_paper_monitor`, and replace the old `trades` schema.
- **S7 — New dashboard.** User: "tendriamos que diseñar un dashboard nuevo que sea mas flexible". Rewrite the Looker Studio spec around questions, not tables: run explorer, run x-ray, comparator (HPO surface included), robustness (IS vs OOS, DSR, MC fan), live/paper monitor; one `run_id` control filters every page.

## Tasks

| ID | Specs | Route | Status | Evidence |
|----|-------|-------|--------|----------|
| T1 | S2 | inline | done | bea2ccb |
| T2 | S4 | inline | done | b5c33b7 |
| T3 | S2 | inline | done | 79f8ddb |
| T4 | S2, S3 | inline | done | c51a764 |
| T5 | S3, S6 | inline | done | 332961a (SQL not validated against BigQuery; pending T12) |
| T6 | S1, S5 | inline | done | 52a0ef5 |
| T7 | S1, S5 | inline | done | 241b4dd |
| T8 | S4, S5 | inline | done | 75a3fa7 |
| T9 | S4, S5 | inline | done | fe3b3fb |
| T10 | S1 | inline | done | 9f6c8d3 |
| T11 | S7 | inline | done | 28ab110 |
| T12 | S6 | inline | done | executed 2026-10-13 on mi-facturador-bot-01.atsnt_bi (user-authorized) |

- T1: extract `RunSpec`, `RunType`, `RunId` (UUID v7); artifact = spec + scorecard with backward-compatible JSON.
- T2: MAE/MFE equity points on `ClosedTrade`, tracked by the engine; reconstruction test vs engine max DD.
- T3: complete metrics (fee drag, exits by reason, avg holding, avg MAE/MFE).
- T4: `RunRecorder` port + BigQuery adapter rows; delete old row types.
- T5: new DDL and views; drop statements for old objects.
- T6: backtest + Monte Carlo binary writes the new model; remove report JSON/GCS/Parquet paths.
- T7: HPO parent/trial runs; keep deployment artifact write and upload.
- T8: paper trading run lifecycle + equity events; remove polled snapshots.
- T9: live trading same as paper; fix hardcoded values.
- T10: cleanup (`GcsArtifactSink`, pipeline script, docs 05/10).
- T11: dashboard spec rewrite (doc 11).
- T12: execute the drop of old BigQuery objects (requires GCP access; user-run if unavailable).

Routes are inline: no parallel units with disjoint surfaces and no context backstop triggered.

## Technical debt

- Funding-rate payments for perpetual futures are not modeled by the engine; once modeled, each payment becomes a cash-flow event in `run_series`.

## Log

- L1 (2026-10-13), original request: "Lo que estoy buscando es estructurarar bien mi arquitectura de datos porqeu creo qeu ahora es deficiente en todos los sentidos, mi objetivo final es poder analizar todos lso parámetros que importan de la estrategia. backtests, hpos montecarlos... [...] El punto es que en general creo que guardo los datos en el formato incorrecto. los debería guardar todos en el mismo formato, bq? como los configuro para que no se guarden por ejemplo 6000 snapshots al día si a mi me importa solo el último? o si quiero una evolución.. no se. necesito que analices el repo y busques las discrepancias en este sentido."
- L2 (2026-10-13): "el funding de eventos no se lo que es. y no lo modela. se puede poner como deuda técnica. las tablas viejas se pueden eliminar. si comienza a ejeuctar el plan."
- L3: analysis evidence — HPO rows hardcode `total_trades/win_rate/max_drawdown_pct = 0` and store Sortino in `*_sharpe` columns (`run_hpo.rs:345-356`); paper writes snapshots per bar and every 1 s (`paper_trading.rs:433,593`); live hardcodes unrealized PnL and position (`live_trading.rs:623-632`); no code reads `storage/reports/*.json`. 2026 BTCUSDT at $1M threshold ≈ 11.5k bars/day, so per-bar series are rejected (HPO 720 trials ≈ 190 GB).
- L4 (2026-10-13): correction to S4 — four points per trade (entry/trough/peak/exit) cannot always reproduce the engine max drawdown (an intra-trade high before the trough is lost). Each trade therefore also stores the deepest session drawdown observed while open (`max_drawdown_amount/pct`); the max across trades equals the engine metric exactly (test `per_trade_drawdowns_reproduce_engine_max_drawdown_exactly`). The four points remain the plotted curve.
- L5: old row types (`EquitySnapshotRow`, `HpoEvaluationRow`, `MonteCarloRow`, old `TradeRow`) are deleted when their last caller migrates (T9/T10) instead of in T4, to keep every commit building.
- L6: T7 replaced the hardcoded HPO fee schedule with `--execution-context <json>` (default `BacktestConfig::default()`), per S2; HPO trial metrics are per-fold scopes `is_fold_<n>` / `oos_fold_<n>` plus a `full` extras row with ranking scores. Monte Carlo stores 8 representative curves (series `mc_equity`).
- L7: T10 found that tests using a "disabled" BigQuerySink fell back to the local gcloud CLI and streamed into the real dataset (legacy `disabled_bigquery_sink_succeeds_without_panic` inserted `trade_1` into `atsnt_bi.trades`; the new T4 test hit 404 on non-existent tables). Fixed with `BigQuerySink::offline` / `GcsArtifactSink::offline`; `cargo test --workspace -- --nocapture` now shows only "Insert skipped" lines.
- L8: T11 added `v_hpo_trials`, `v_hpo_folds`, `v_monte_carlo` views; doc 11 kept in Spanish to match its existing language.
- L9: verification state — `cargo test --workspace` 168 passed, clippy and fmt clean; backtest and HPO binaries smoke-run offline on real 2024-01 / 2026-10-01 data; paper/live not run live (require WebSocket/Binance access). SQL never executed against BigQuery. Native assess: risk high, review_due (4351 changed lines on the branch); independent verifier and native review not run yet. Next: T12 after authorization, then verification/review, then delivery strategy (branch exceeds 400 lines).
- L10 (2026-10-13): "entonces no vamos a revisar esos cambios. lo damos como bueno. y si, te autorizo para esa tarea". Native review START had failed with `lens_context_budget_exceeded` (4354 lines); user declined splitting and accepted the branch without review or independent verifier.
- L11: T12 executed. Before drop: trades 18 rows, equity_snapshots 540,044 rows, hpo_evaluations 0, monte_carlo_runs 0, views v_trades and v_live_paper_monitor. After: tables runs, run_metrics, trades, run_series and 9 views created; every view queries successfully (0 rows). No end-to-end insert through `--gcp-bigquery` has been run yet (JSON columns via insertAll unverified).
- L12 (2026-10-13): end-to-end `backtest --gcp-bigquery` exposed BigQuery NUMERIC scale rejection (>9 decimals); fixed in 917cfda and verified: v_runs/v_trades/v_equity/v_monte_carlo populated, MAX(trades.max_drawdown_pct) = engine max DD (0.002133156). VM atsnt-engine redeployed with the new paper_trading (run 01a134de-8859-7cf4-b97a-cbdb749dfe8c recording). GCS cleaned to `artifacts/hpo_btcusdt_2026.json` only. Orphan runs from the failed first attempt deleted.
- L13 (2026-10-13): "adelante, hazlo y cierra toda la refactorización. actualiza documentación y haz push a la rama main y elimina las auxiliares." Feature closed; merged to main.
