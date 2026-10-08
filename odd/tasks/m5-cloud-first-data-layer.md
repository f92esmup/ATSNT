# Milestone 5 — Web Decommission & Decoupled Cloud-First Data Layer

Branch: `main` | Status: Complete (`[x]`)
Commit: [`1bf9cb6`](https://github.com/f92esmup/ATSNT/commit/1bf9cb6)
Document Version: 1.0.0
Authoritative Architecture: [`docs/architecture/07-web-telemetry-dashboard.md`](../../docs/architecture/07-web-telemetry-dashboard.md)

---

## 1. Goal & Architectural Purpose
The goal of Milestone 5 is to decouple visualization and monitoring entirely from the high-performance trading engine, eliminating local web servers, open inbound network ports, and custom frontend code maintenance.

The architecture was transformed into a **Cloud-First** reporting ecosystem:
1. **Google Cloud BigQuery (`atsnt_bi`)**: Direct streaming insert of execution trades, periodic account equity snapshots, hyperparameter optimization (HPO) parameter spaces, and Monte Carlo risk summaries.
2. **Google Cloud Storage (`gs://atsnt-lake/simulations/`)**: High-efficiency columnar Apache Parquet export of massive Monte Carlo simulation trajectories.
3. **Looker Studio**: Zero-code Business Intelligence dashboards connected via native 1-click SQL queries to BigQuery.
4. **Telegram Bot Webhook**: Real-time push notifications (<200ms latency) to operators' mobile devices for position entries, exits, stop losses, and circuit breakers.

---

## 2. Implementation Summary

### Phase 1: Web Decommission & Modular Quantitative Refactor
- Completely deleted `crates/web` (Axum HTTP server, WebSocket endpoints, static SPA assets, and API test suite).
- Purged web dependencies (`axum`, `tower-http`, `tokio-stream`) from workspace `Cargo.toml`.
- Rescued and migrated asynchronous paper trading runtime (`PaperSessionRuntime` and `PaperSessionOwner`) natively into `crates/backtest/src/paper.rs`.
- Reorganized mathematical sampling and filtering into pure, modular submodules in `crates/domain/src/analytics/` (`sampling.rs` [DollarBarAggregator], `filters.rs` [CusumFilter], `stats.rs` [RollingZScore]) adhering to Marcos López de Prado's *Advances in Financial Machine Learning* (AFML).

### Phase 2: GCP Cloud-First Data Layer & Telegram Alerting
- Defined `AlertNotifier` async trait in `crates/adapters/src/traits.rs`.
- Implemented `crates/adapters/src/gcp/`:
  - `telegram.rs`: `TelegramNotifier` with Markdown formatting for order fills, stops, and risk trips.
  - `bigquery.rs`: `BigQuerySink` streaming to tables `trades`, `equity_snapshots`, `hpo_evaluations`, and `monte_carlo_runs`.
  - `gcs.rs`: `GcsParquetSink` using Apache Arrow/Parquet for in-memory trajectory batch serialization.
  - `mod.rs`: Module exports and RFC 3339 timestamp helpers.
- Connected CLI flags across `crates/backtest`:
  - `paper_trading`: Added `--gcp-bigquery` and `--telegram-alerts`.
  - `run_hpo`: Added `--gcp-bigquery`.
  - `backtest`: Added `--gcp-bigquery` and `--gcs-parquet`.
- Enforced fail-open / offline fallback: if GCP/Telegram credentials are not set in the environment, sinks log informative messages without erroring or blocking execution.

---

## 3. Verification & Acceptance Evidence

All verification commands executed cleanly:
- `cargo check --workspace --all-targets`: 0 errors.
- `cargo test --workspace`: 69 passed, 0 failed.
- `cargo clippy --workspace --all-targets -- -D warnings`: 0 warnings.
- `cargo fmt --check`: 100% compliant.
- Delivered and pushed to `origin/main` in commit `1bf9cb6`.
