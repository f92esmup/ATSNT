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
│     - trades (Execution audit & realized PnL)          │  Streaming Asynchronous Inserts
│     - equity_snapshots (Account MTM & live drawdown)   │  (tabledata.insertAll REST API)
│     - hpo_evaluations (Parameter stability surface)    │
│     - monte_carlo_runs (Stress tests & ruin prob)      │
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
| **Google Cloud Storage (GCS)** | 5 GB Standard Storage / month | < 250 MB / month (compressed Parquet files) | **0,00 €** |
| **Looker Studio** | 100% Free Google SaaS product | Unlimited dashboards and scheduled reports | **0,00 €** |
| **Total Monthly Spend** | | | **0,00 €** |

---

## 4. BigQuery Data Warehouse Schemas (`atsnt_bi`)

The data warehouse contains four core tables partitioned by date to minimize query byte scans:

### 4.1 Table: `atsnt_bi.trades`
Records closed trades from both historical backtests and live/paper trading sessions:
```sql
CREATE TABLE IF NOT EXISTS `atsnt_bi.trades` (
    trade_id STRING NOT NULL,
    session_id STRING NOT NULL,
    strategy_id STRING NOT NULL,
    symbol STRING NOT NULL,
    side STRING NOT NULL,               -- 'Long' | 'Short' | 'ClosedPosition'
    entry_timestamp TIMESTAMP NOT NULL,
    exit_timestamp TIMESTAMP NOT NULL,
    entry_price NUMERIC NOT NULL,
    exit_price NUMERIC NOT NULL,
    quantity NUMERIC NOT NULL,
    gross_pnl NUMERIC NOT NULL,
    fees_paid NUMERIC NOT NULL,
    net_pnl NUMERIC NOT NULL,
    exit_reason STRING NOT NULL,        -- 'TakeProfit' | 'StopLoss' | 'TimeBarrier'
    holding_duration_seconds INT64
)
PARTITION BY DATE(exit_timestamp)
CLUSTER BY symbol, strategy_id;
```

### 4.2 Table: `atsnt_bi.equity_snapshots`
Records periodic mark-to-market valuations and live drawdown from running engines:
```sql
CREATE TABLE IF NOT EXISTS `atsnt_bi.equity_snapshots` (
    timestamp TIMESTAMP NOT NULL,
    session_id STRING NOT NULL,
    symbol STRING NOT NULL,
    cash_equity NUMERIC NOT NULL,
    unrealized_pnl NUMERIC NOT NULL,
    total_equity NUMERIC NOT NULL,
    drawdown_pct NUMERIC NOT NULL,
    active_position_side STRING,         -- 'Long' | 'Short' | NULL
    active_position_qty NUMERIC
)
PARTITION BY DATE(timestamp)
CLUSTER BY session_id;
```

### 4.3 Table: `atsnt_bi.hpo_evaluations`
Stores Walk-Forward Optimization parameter combinations and overfitting diagnostics:
```sql
CREATE TABLE IF NOT EXISTS `atsnt_bi.hpo_evaluations` (
    run_id STRING NOT NULL,
    timestamp TIMESTAMP NOT NULL,
    strategy_id STRING NOT NULL,
    parameter_space STRING NOT NULL,     -- JSON: {"dollar_threshold": 1000000, "z_entry": 1.8...}
    is_sharpe NUMERIC NOT NULL,
    oos_sharpe NUMERIC NOT NULL,
    deflated_sharpe_ratio NUMERIC NOT NULL,
    parameter_stability_score NUMERIC NOT NULL,
    total_trades INT64 NOT NULL,
    win_rate NUMERIC NOT NULL,
    max_drawdown_pct NUMERIC NOT NULL
)
PARTITION BY DATE(timestamp)
CLUSTER BY strategy_id;
```

### 4.4 Table: `atsnt_bi.monte_carlo_runs`
Stores aggregate stress-testing statistics from Circular Block Bootstrapping:
```sql
CREATE TABLE IF NOT EXISTS `atsnt_bi.monte_carlo_runs` (
    run_id STRING NOT NULL,
    timestamp TIMESTAMP NOT NULL,
    strategy_id STRING NOT NULL,
    iterations INT64 NOT NULL,
    resample_method STRING NOT NULL,     -- 'CircularBlockBootstrap' | 'IID'
    historical_max_drawdown NUMERIC NOT NULL,
    p50_max_drawdown NUMERIC NOT NULL,
    p95_max_drawdown NUMERIC NOT NULL,
    p99_max_drawdown NUMERIC NOT NULL,
    probability_of_ruin_pct NUMERIC NOT NULL
)
PARTITION BY DATE(timestamp)
CLUSTER BY strategy_id;
```

### 4.5 Idempotency & Data Contract Principles
To guarantee deterministic reproducibility, zero duplicate rows, and seamless BI filtering:
1. **Streaming Deduplication (`insertId`)**: All BigQuery streaming inserts via `BigQuerySink::insert_rows` attach a unique `insertId` envelope for each row. BigQuery uses this key for automatic 1-minute deduplication on network retries.
2. **Deterministic ID Taxonomy**:
   - `session_id`:
     - Backtest: `bt_<symbol>_<timestamp>`
     - Paper Trading: `paper_<symbol>_<session_start_timestamp>`
     - Live Gateway: `live_<symbol>_<session_start_timestamp>`
   - `trade_id`:
     - Backtest & Paper: `{session_id}_t{idx:05}` (e.g. `paper_btcusdt_1791544244_t00001`)
     - Live Gateway: `{symbol}_{binance_order_id}_{trade_id}`
3. **Analytical View Layer Idempotency**: All analytical reporting queries read through deduplicating views (`v_trades`, `v_live_paper_monitor`) using windowed `ROW_NUMBER() OVER (...) = 1` to guarantee absolute data consistency even across manual table reloads.

---

## 5. Google Cloud Storage (GCS) Hierarchy

Heavy simulation curves and columnar market data are stored as Snappy-compressed Apache Parquet in the bucket `gs://atsnt-lake-${PROJECT_ID}`:

```text
gs://atsnt-lake-${PROJECT_ID}/
├── simulations/
│   └── monte_carlo_<run_id>.parquet       # Trajectories: run_id, label, step_index, equity
├── hpo/
│   └── hpo_results_<run_id>.json          # Full serialized winning parameter manifests
├── backtests/
│   └── backtest_<timestamp>.json          # Detailed backtest execution reports
└── market_data/
    └── <symbol>/
        └── <symbol>-aggTrades-<YYYY-MM>.parquet
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

Looker Studio connects directly to BigQuery using the standard Google connector.

### Analytical SQL Views for Dashboards:

1. **`v_trades`**:
   Idempotent, deduplicated closed trade records with full price and duration metrics:
   ```sql
   CREATE OR REPLACE VIEW `atsnt_bi.v_trades` AS
   SELECT * EXCEPT(row_num)
   FROM (
       SELECT
           *,
           ROW_NUMBER() OVER(PARTITION BY trade_id ORDER BY exit_timestamp DESC) AS row_num
       FROM `atsnt_bi.trades`
   )
   WHERE row_num = 1;
   ```

2. **`v_live_paper_monitor`**:
   Returns the latest mark-to-market state of each active paper/live trading session:
   ```sql
   CREATE OR REPLACE VIEW `atsnt_bi.v_live_paper_monitor` AS
   SELECT * EXCEPT(row_num)
   FROM (
       SELECT
           timestamp,
           session_id,
           symbol,
           cash_equity,
           unrealized_pnl,
           total_equity,
           drawdown_pct,
           active_position_side,
           active_position_qty,
           ROW_NUMBER() OVER(PARTITION BY session_id ORDER BY timestamp DESC) AS row_num
       FROM `atsnt_bi.equity_snapshots`
   )
   WHERE row_num = 1;
   ```

### 7.1 Multi-Dimensional Filtering in Looker Studio
Every dashboard report in Looker Studio can incorporate interactive control dropdowns:
- **Strategy Selector**: Dropdown on `strategy_id` (e.g. `DollarBarsCusum_v1`, `TrendFollowing_v1`).
- **Session Filter**: Dropdown on `session_id` to compare individual live runs against historical backtests (`bt_...`, `paper_...`, `live_...`).
- **Instrument Selector**: Dropdown on `symbol` (`BTCUSDT`, `ETHUSDT`).
- **Date Range Picker**: Native calendar control bound to `exit_timestamp`.
- **Optimization Surface Explorer**: Multi-metric scatter plot on `hpo_evaluations` filtered by `strategy_id` and sorted by `deflated_sharpe_ratio`.

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
# 1. Backtest & Monte Carlo with BigQuery & GCS export:
cargo run -p backtest -- \
  --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet \
  --monte-carlo \
  --gcp-bigquery \
  --gcs-parquet

# 2. Walk-Forward HPO with parameter surface streaming:
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
# [GCP :: Cloud Storage] File uploaded -> gs://atsnt-lake/simulations/monte_carlo_1791544244.parquet
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


