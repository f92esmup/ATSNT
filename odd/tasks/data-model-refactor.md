# Data Model Refactor — Unified Run Contract for Analytics

Objective: replace the per-binary, ad-hoc persistence (local JSON reports, GCS copies, MC Parquet, polled equity snapshots, mislabeled HPO rows) with one run-centric BigQuery model that new strategies plug into without schema changes.
Branch: `feat/data-model-refactor` | Delivery: ask-on-risk (forecast > 400 lines → chained PRs) | Test runner: `cargo test --workspace`, plus `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.

## Specs

- **S1 — No useless artifacts.** User: "si no les voy a dar uso, no veo el sentido de guardarlos en gcloud storage". Remove local `storage/reports/*.json` writes, GCS uploads of those reports, and the Monte Carlo trajectory Parquet export. Keep `StrategyDeploymentArtifact` (consumed by deploy, paper, and live) and its GCS upload.
- **S2 — Common data contract.** User: "que contrato de datos establecemos como comun para todos? Porque al final buscamos una estructura capaz de que se añadan nuevas estrategias sin fricción [...] hiperparámetros optimizables. fees y etc. para que no se decida hardcodeado". Every run records a `RunSpec` (strategy_id, symbol, market_type, sampling, strategy params as JSON, execution_context with fees/slippage/risk) plus run metadata (run_id, parent_run_id, run_type, data window, git sha). The deployment artifact is `RunSpec + HpoScorecard` and existing artifact JSON keeps loading. No hardcoded fees, resample methods, or placeholder metric values.
- **S3 — BigQuery model.** Tables `runs`, `run_metrics` (scope full|is|oos|fold_N + extras JSON), `trades` (with run_id, run_type, MAE/MFE), `run_series` (event points). Views `v_runs`, `v_trades`, `v_equity` (drawdown derived via window function), `v_latest_equity`. Tables are append-only; views select the latest row per key (streaming buffer forbids UPDATE).
- **S4 — Event-based equity, never time-based.** User: "mi cuenta solo va a cambiar cuando se opera. a si que muestreos por tiempo no tienen sentido. pero guardar el estado exacto que se realiza cuando hay flujo de dinero, en cada trade en entrada y salida". Per trade persist four equity points: entry, trough (MAE), peak (MFE), exit, each with timestamp. The max drawdown reconstructed from these points must equal the engine's mark-to-market `max_drawdown_pct`. Remove the 1 s / 5 s MTM snapshots and the per-bar snapshot.
- **S5 — What each run type stores.** User: "en backtest y hpo y montecarlo. ahi tampoco tiene sentido guardar cada trade exacto". Backtest: run, metrics, trades, equity events. HPO: parent run + one `hpo_trial` child run per candidate with real IS/OOS metrics only (no trades, no series). Monte Carlo: child run of its backtest with percentiles in metrics extras and fan chart in `run_series` (no trades). Paper/live: run, trades, equity events; live must stop hardcoding `unrealized_pnl = 0` and position `None`.
- **S6 — Old BigQuery objects are dropped.** User: "las tablas viejas se pueden eliminar". Drop `equity_snapshots`, `hpo_evaluations`, `monte_carlo_runs`, `v_live_paper_monitor`, and replace the old `trades` schema.
- **S7 — New dashboard.** User: "tendriamos que diseñar un dashboard nuevo que sea mas flexible". Rewrite the Looker Studio spec around questions, not tables: run explorer, run x-ray, comparator (HPO surface included), robustness (IS vs OOS, DSR, MC fan), live/paper monitor; one `run_id` control filters every page.

## Tasks

| ID | Specs | Route | Status | Evidence |
|----|-------|-------|--------|----------|
| T1 | S2 | inline | pending | |
| T2 | S4 | inline | pending | |
| T3 | S2 | inline | pending | |
| T4 | S2, S3 | inline | pending | |
| T5 | S3, S6 | inline | pending | |
| T6 | S1, S5 | inline | pending | |
| T7 | S1, S5 | inline | pending | |
| T8 | S4, S5 | inline | pending | |
| T9 | S4, S5 | inline | pending | |
| T10 | S1 | inline | pending | |
| T11 | S7 | inline | pending | |
| T12 | S6 | inline | pending | |

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
