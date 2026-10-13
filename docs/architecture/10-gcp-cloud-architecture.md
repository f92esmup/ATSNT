# GCP Cloud-First Architecture & BI Analytics Specification

> [!NOTE]
> **Document Status**: Production Specification (Milestone 5 Phase 2)  
> **Target Cloud Provider**: Google Cloud Platform (GCP)  
> **BI Tool**: Google Looker Studio (formerly Data Studio)  
> **Hosting & Analytics Cost**: **0,00 € / mes** (100% Always Free Tier compliant)

---

## 1. Executive Summary & Architectural Philosophy

The ATSNT quantitative trading engine is designed as an **outbound-only, air-gapped system**. It decouples live execution from heavy quantitative research and business intelligence reporting:

1. **Zero Open Inbound Ports**: The engine does not run an HTTP or WebSocket listener in production. All cloud interactions are strictly outbound HTTPS API requests to serverless GCP endpoints (BigQuery Streaming Ingestion, Cloud Storage, and Telegram Bot API).
2. **Zero Database Maintenance**: No self-hosted PostgreSQL, MySQL, or ClickHouse instances. We leverage Google BigQuery's managed serverless columnar architecture and Google Cloud Storage (GCS) as an immutable data lake.
3. **Decoupled Workload Topology**:
   - **Local Workstation / Spot Heavy Compute**: Caches historical market data, runs parallel Walk-Forward Optimization (HPO) with `rayon`, and simulates 10,000 Monte Carlo trajectories.
   - **GCP Compute Engine (`e2-micro`)**: Runs the long-lived, 24/7 event loop for `paper_trading` or `live_gateway`, consuming between 15 MB and 40 MB of RAM.
   - **GCP Analytics & BI**: BigQuery and GCS ingest telemetry, and Looker Studio displays dashboards with zero frontend code.

---

## 2. End-to-End System Topology

```
┌─────────────────────────────────────────────────────────────────┐
│                    1. LOCAL WORKSTATION / CI-CD                 │
│  - cargo build --release (produces lightweight static binary)   │
│  - fetch_data (Binance aggTrades CSV -> Apache Parquet)         │
│  - backtest & monte_carlo (Circular Block Bootstrap)            │
│  - run_hpo (Walk-Forward Optimization with Rayon multithreading)│
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                │ Outbound HTTPS: Results & Trajectories
                                ▼
┌────────────────────────────────────────────────────────┐       ┌──────────────────────────────────────┐
│        3. GCP DATA WAREHOUSE & DATA LAKE               │       │       2. GCP COMPUTE (e2-micro)      │
│                                                        │       │  - paper_trading / live_gateway      │
│  A. Google Cloud Storage (Bucket: gs://atsnt-lake)     │       │  - Continuous 24/7 Tokio runtime     │
│     - /simulations/monte_carlo_<run_id>.parquet        │       │  - Resident Memory: 15 MB - 40 MB    │
│     - /hpo/wfo_results_<run_id>.json                   │       │  - Zero inbound listening ports      │
│     - /market_data/<symbol>/...                        │       └──────────────────┬───────────────────┘
│                                                        │                          │
│  B. Google BigQuery (Dataset: atsnt_bi)                │◄─────────────────────────┘
│     - runs (spec, lineage, lifecycle)                  │  Streaming Asynchronous Inserts
│     - run_metrics (x-ray per run and scope)            │  (tabledata.insertAll REST API)
│     - trades (closed trades with MAE/MFE)              │
│     - run_series (event-based equity, MC curves)       │
└───────────────────────────────┬────────────────────────┘
                                │
                                │ Native BigQuery Connector (Zero ETL)
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                  4. LOOKER STUDIO DASHBOARDS                    │
│  - Real-time Equity Curves, Drawdowns & Active Positions        │
│  - Cumulative PnL, Win Rate, Profit Factor & Payoff Ratio       │
│  - Walk-Forward In-Sample vs Out-of-Sample Parameter Heatmaps   │
│  - Monte Carlo Fan-Chart Ensembles & Ruin Distributions         │
└─────────────────────────────────────────────────────────────────┘
```

---

## 3. Financial Budget & Always Free Tier Commitment

The architecture is deliberately dimensioned to run at **0,00 € / mes** indefinitely by staying within Google Cloud's *Always Free* quotas:

| Component | Free Monthly Quota | ATSNT Estimated Consumption | Cost |
| :--- | :--- | :--- | :--- |
| **Compute Engine** | 1 `e2-micro` instance (in `us-central1`, `us-east1`, or `us-west1`) | 1 instance running 24/7 (paper or live engine) | **0,00 €** |
| **Standard Persistent Disk** | 30 GB HDD / month | 10 - 20 GB operating system boot disk | **0,00 €** |
| **BigQuery Ingestion & Storage** | 10 GB storage free / month | < 50 MB / month (tabular logs and snapshots) | **0,00 €** |
| **BigQuery Query Engine** | 1 TB analytical queries free / month | < 2 GB / month (Looker Studio dashboard caching) | **0,00 €** |
| **Google Cloud Storage (GCS)** | 5 GB Standard Storage / month (US regions) | < 250 MB / month (compressed Parquet files) | **0,00 € - 0,01 €** |
| **Looker Studio** | 100% Free Google SaaS product | Unlimited dashboards and scheduled reports | **0,00 €** |
| **Total Monthly Spend** | | | **0,00 €** |

> [!NOTE]
> **Regional Placement (Madrid `europe-southwest1`)**:
> By default, the repository provisions BigQuery (`atsnt_bi`) and GCS (`atsnt-lake-${PROJECT_ID}`) in single-region **Madrid (`europe-southwest1`)** for minimal latency, data residency, and 1:1 synchronization with Madrid time (`20:00 Madrid daily reports`). In European single-regions, GCS standard storage is billed at ~0,02 €/GB/month (fractions of a cent for ATSNT's megabyte-sized audit files). If strict zero-cent tier is desired for GCS, the bucket location can be set to `us-central1`.

---

## 4. BigQuery Data Warehouse Schemas (`atsnt_bi`)

The canonical DDL is [`scripts/gcp/bigquery_schema.sql`](../../scripts/gcp/bigquery_schema.sql). Datasets created with the previous model must first run [`scripts/gcp/bigquery_drop_legacy.sql`](../../scripts/gcp/bigquery_drop_legacy.sql) (destructive).

Every execution mode records the same run-centric contract through the `RunRecorder` port (`crates/adapters/src/analytics.rs`), keyed by `run_id` (UUID v7):

| Table | Grain | Written by |
|-------|-------|------------|
| `runs` | One row per run and status change (`running` → `completed` / `stopped` / `failed`); full spec: strategy params, sampling, execution context (fees, slippage, capital), data window, git sha, `parent_run_id` | all run types |
| `run_metrics` | One row per run and scope (`full`, `is_fold_<n>`, `oos_fold_<n>`); trade metrics plus `extras` JSON (DSR, stability, ruin probability, MC percentiles) | all run types |
| `trades` | One row per closed trade with MAE/MFE equity points and the deepest drawdown while open | `backtest`, `paper`, `live` |
| `run_series` | Event points, never time-sampled: `equity` (entry/trough/peak/exit per trade) and `mc_equity` (8 representative Monte Carlo curves) | `backtest`, `monte_carlo`, `paper`, `live` |

Run types: `backtest`, `monte_carlo` (child of its backtest), `hpo`, `hpo_trial` (child of its sweep, metrics only), `paper`, `live`. Strategy parameters are stored as JSON, so adding a strategy needs no schema change.

### 4.1 Idempotency & Data Contract Principles

1. **Append-only**: streaming inserts cannot be updated while in the streaming buffer, so lifecycle changes append rows and views keep the latest row per key.
2. **Deduplicated reads**: `insertId` keys (`run_id+status`, `run_id+scope`, `trade_id`, series point key) plus `ROW_NUMBER()`/`DISTINCT` views make retries harmless.
3. **Exact drawdown**: `MAX(trades.max_drawdown_pct)` per run equals the engine's mark-to-market max drawdown, without storing every bar.
4. **Fixed-point values**: all monetary fields are `NUMERIC` serialized from `rust_decimal::Decimal`.

---

## 5. Google Cloud Storage (GCS) Hierarchy

GCS only stores the strategy deployment artifact uploaded by `run_hpo --gcs-upload`; analytics live in BigQuery.

```text
gs://atsnt-lake-${PROJECT_ID}/
└── artifacts/
    └── <output-config file name>.json     # StrategyDeploymentArtifact (RunSpec + HPO scorecard)
```

---

## 6. Authentication & Secrets Architecture (GCP Secret Manager + Cascade Resolver)

To eliminate plaintext secrets and `.env` files from production disks, the ATSNT trading engine integrates directly with **Google Cloud Secret Manager** (`secretmanager.googleapis.com`):

### 6.1 Enterprise Secret Resolution
Exchange credentials (`BINANCE_API_KEY`, `BINANCE_SECRET_KEY`) are dynamically resolved at runtime via [`GcpSecretManager`]:

1. **Tier 1 (Production)**: **Google Cloud Secret Manager REST API** (`/v1/projects/{project}/secrets/{id}/versions/latest:access`). Decodes base64 secret payload directly into memory with zero disk persistence.
2. **Tier 2 (Fallback)**: Local environment variables / `.env` file (strictly for local offline development).
3. **Tier 3 (Manual CLI)**: Explicit `--api-key` and `--secret-key` flags.

```text
┌─────────────────────────────────────────────────────────────┐
│                 GcpSecretManager Resolution                 │
├─────────────────────────────────────────────────────────────┤
│ 1. GCP Secret Manager: BINANCE_API_KEY / BINANCE_SECRET_KEY │
│    (Enterprise production, zero credentials on disk)        │
│                                                             │
│ 2. Environment / .env Fallback                              │
│    (Local developer convenience)                            │
└─────────────────────────────────────────────────────────────┘
```

### 6.2 Token & Project ID Cascade Resolver (`GcpAuthResolver`)
To authenticate with Secret Manager, BigQuery, and GCS without hardcoded credentials, the Rust adapter uses a 3-tier cascade resolution strategy:

```text
┌─────────────────────────────────────────────────────────────┐
│                 GcpAuthResolver (Cascade)                   │
├─────────────────────────────────────────────────────────────┤
│ 1. Static Token: GCP_AUTH_TOKEN                             │
│    (Ideal for CI/CD, unit testing, and short manual runs)   │
│                                                             │
│ 2. GCE Metadata Server (http://metadata.google.internal)    │
│    (Automatic 24/7 token renewal when running on GCP VM;   │
│     zero service-account files stored on the filesystem)    │
│                                                             │
│ 3. Local CLI Fallback (gcloud auth print-access-token)      │
│    (Seamless local workstation developer experience)        │
└─────────────────────────────────────────────────────────────┘
```

If none of the above are configured or network access fails, the engine operates in **Fail-Open Mode**: it emits a diagnostic `tracing::info` log and proceeds without interrupting trading execution.

---

## 7. Looker Studio BI Dashboard Specification

Looker Studio reads only the views defined in `bigquery_schema.sql`: `v_runs` (latest run row + `full` metrics + flattened spec fields), `v_run_metrics`, `v_trades`, `v_series`, `v_equity` (event-based equity with running peak and drawdown), `v_latest_equity` (paper/live monitor), and the flattened `v_hpo_trials`, `v_hpo_folds` and `v_monte_carlo`. The report layout is specified in [11-looker-studio-bi-spec.md](11-looker-studio-bi-spec.md).

---

## 8. Operational Quickstart

### Step 1: Provision GCP Resources (Once)
```bash
export GCP_PROJECT_ID="your-project-id"
export GCP_REGION="europe-southwest1" # Madrid

./scripts/gcp/setup_gcp_resources.sh
```

### Step 2: Run Local Simulations with Cloud Export
```bash
# 1. Backtest & Monte Carlo recorded as runs (Monte Carlo is a child run):
cargo run -p backtest -- \
  --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet \
  --monte-carlo \
  --gcp-bigquery

# 2. Walk-Forward HPO recorded as an hpo run with one hpo_trial run per candidate:
cargo run -p backtest --bin run_hpo -- \
  --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet \
  --gcp-bigquery
```

### Step 3: Run Paper Trading on GCP `e2-micro` VM
```bash
# Executed inside the VM (zero credentials needed, authenticated via GCE metadata server):
./paper_trading \
  --symbol btcusdt \
  --gcp-bigquery \
  --telegram-alerts
```

---

## 9. Professional Observability Architecture

To ensure high reliability, zero terminal lockup on constrained cloud virtual machines (e.g., `e2-micro`), and zero disk saturation, ATSNT enforces a clean multi-tier observability architecture:

### 9.1 Multi-Tier Observability Matrix

| Channel | Data Type | Frequency | Destination | Example |
| :--- | :--- | :--- | :--- | :--- |
| **Operational Push Alerts** | Key business execution events | Very low (minutes/hours) | Telegram Bot API | Position Opened, Stop Loss hit, Trade Closed summary, Daily 20:00 Madrid report |
| **Structured System Logs** | Engine lifecycle & audit | Medium (seconds/minutes) | `journald` (100 MB rotating disk) | Bar finalized, order executed, socket reconnect, network error |
| **Hot Path Market Ticks** | High-throughput trade ticks | High (10-100s per second) | `tracing::trace!` (stdout only if `--show-ticks`) | `[TICK #1042] ts: ... price: 65420.50` (silent in production) |
| **Analytical BI Telemetry** | Aggregated equity curves & PnL | Periodic (1s / 5s) | BigQuery (`atsnt_bi`) | Streaming inserts for real-time Looker Studio dashboards |

### 9.2 Hot-Path Tick Telemetry (`--show-ticks`)
In production, streaming thousands of individual trade ticks to standard output saturates CPU, network, and terminal buffers.
- By default, incoming trade ticks are evaluated in-memory and logged at `tracing::trace!` level without console output.
- Pass `--show-ticks` only when debugging interactively in local terminal environments.

### 9.3 Clean Single-Line Cloud Telemetry Markers
Multi-line ASCII boxes have been eliminated in favor of concise, single-line cloud stream notifications and structured tracing events:
```text
# [GCP :: BigQuery] Streamed 1 row(s) -> mi-facturador-bot-01:atsnt_bi.trades
# [GCP :: Secret Manager] Secret 'BOT_TOKEN' resolved from project 'mi-facturador-bot-01'
# [GCP :: Cloud Storage] File uploaded -> gs://atsnt-lake/artifacts/hpo_btcusdt_2026.json
```

### 9.4 Real-Time & Executive Telegram Notifications

ATSNT connects directly to Telegram using bot credentials resolved from Google Cloud Secret Manager (`BOT_TOKEN` / `TELEGRAM_BOT_TOKEN`, `CHAT_ID` / `TELEGRAM_CHAT_ID`) or local `.env`:

1. **Position Opened Alert**:
   Emits instrument, direction, execution entry price, quantity, stop loss, take profit, and current cash equity upon fill.
2. **Comprehensive Trade Closed Summary**:
   Emits exit reason (`TakeProfit`, `StopLoss`, `TimeBarrier`), holding duration (`Xm Ys`), entry/exit prices, gross PnL, fees and slippage friction, net realized PnL in USD, percentage return, and total equity.
3. **Daily Executive Performance Summary (20:00 Europe/Madrid)**:
   An automated daily scheduler triggers at **20:00 Madrid CET/CEST** every day, calculating:
   - Date and execution mode (Paper / Live)
   - Total trades today, winning trades count, losing trades count, win rate percentage
   - Gross profit, total fees/friction drag, net realized PnL
   - Cash equity, mark-to-market portfolio equity, session drawdown percentage
   - Current open position status (or Flat)
   - Engine telemetry: total ticks processed and dollar bars aggregated today.

---

## 10. Production Deployment: Systemd & Journald Log Rotation

To guarantee 24/7 uptime and prevent disk exhaustion on GCP `e2-micro` instances (30 GB standard disk):

### 10.1 Systemd Service Unit (`atsnt.service`)
Reference unit installed in `/etc/systemd/system/atsnt.service`:

```ini
[Unit]
Description=ATSNT Algorithmic Trading Engine
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
User=f92esmup
WorkingDirectory=/home/f92esmup/Projects/ATSNT
Environment="RUST_LOG=info,adapters=info"
ExecStart=/home/f92esmup/Projects/ATSNT/target/release/paper_trading --symbol btcusdt --telegram-alerts --gcp-bigquery
Restart=always
RestartSec=5s
KillSignal=SIGINT
TimeoutStopSec=30s
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

Enable and start the service:
```bash
sudo systemctl daemon-reload
sudo systemctl enable atsnt.service
sudo systemctl start atsnt.service
```

### 10.2 Journald 100 MB Disk Quota (`journald.conf`)
Configure `/etc/systemd/journald.conf.d/atsnt-quota.conf` (or edit `/etc/systemd/journald.conf`):

```ini
[Journal]
Storage=persistent
Compress=yes
SystemMaxUse=100M
SystemKeepFree=500M
SystemMaxFileSize=10M
MaxRetentionSec=1month
```

Restart `systemd-journald`:
```bash
sudo systemctl restart systemd-journald
```

### 10.3 Inspecting Engine Telemetry
```bash
# Follow live structured logs:
journalctl -u atsnt.service -f

# View last 100 log lines:
journalctl -u atsnt.service -n 100 --no-pager
```


