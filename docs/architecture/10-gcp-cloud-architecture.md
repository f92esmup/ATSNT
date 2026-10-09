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

## 6. Authentication Architecture (Cascade Resolver)

To prevent credential leaks, expired token errors, or downtime, the Rust adapter uses a 3-tier cascade resolution strategy:

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

1. **`v_daily_pnl`**:
   Aggregates net PnL, trade volume, and win rate by day for performance charting:
   ```sql
   SELECT
       DATE(exit_timestamp) AS trade_date,
       symbol,
       strategy_id,
       COUNT(1) AS total_trades,
       COUNTIF(net_pnl > 0) AS winning_trades,
       SAFE_DIVIDE(COUNTIF(net_pnl > 0), COUNT(1)) AS win_rate,
       SUM(gross_pnl) AS daily_gross_pnl,
       SUM(fees_paid) AS daily_fees,
       SUM(net_pnl) AS daily_net_pnl
   FROM `atsnt_bi.trades`
   GROUP BY trade_date, symbol, strategy_id
   ORDER BY trade_date DESC;
   ```

2. **`v_live_paper_monitor`**:
   Returns the latest state of each active paper/live trading session:
   ```sql
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

3. **`v_strategy_performance`**:
   Summarizes high-level strategy metrics (Profit Factor, Total Return):
   ```sql
   SELECT
       strategy_id,
       symbol,
       COUNT(1) AS total_trades,
       ROUND(SAFE_DIVIDE(COUNTIF(net_pnl > 0), COUNT(1)) * 100, 2) AS win_rate_pct,
       ROUND(SUM(net_pnl), 2) AS net_profit_usdt,
       ROUND(SAFE_DIVIDE(SUM(IF(net_pnl > 0, net_pnl, 0)), ABS(SUM(IF(net_pnl < 0, net_pnl, 0)))), 2) AS profit_factor,
       ROUND(AVG(holding_duration_seconds) / 60, 1) AS avg_holding_minutes
   FROM `atsnt_bi.trades`
   GROUP BY strategy_id, symbol;
   ```

---

## 8. Operational Quickstart

### Step 1: Provision GCP Resources (Once)
```bash
export GCP_PROJECT_ID="your-project-id"
export GCP_REGION="EU" # o europe-west1, europe-southwest1, etc.

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

## 9. Observability & Cloud Upload Traceability

To guarantee full operator confidence and operational transparency, all GCP cloud export actions emit explicit console banners and status logs:

### 9.1 BigQuery Streaming Insert Banners
Whenever trades, HPO evaluations, or Monte Carlo statistics are sent to BigQuery:
```text
############################################################
# [GCP :: BigQuery] STREAMING INSERT SUCCESSFUL            #
# Service: Google Cloud BigQuery                          #
# Table:   mi-facturador-bot-01:atsnt_bi.trades           #
# Rows:    12 row(s) streamed successfully                 #
############################################################
```
Periodic mark-to-market equity snapshots emit a concise 1-line stream marker:
```text
# [GCP :: BigQuery] Telemetry snapshot streamed -> mi-facturador-bot-01:atsnt_bi.equity_snapshots (1 row)
```

### 9.2 Cloud Storage Upload Banners
Whenever files (simulation reports, JSON configs, Parquet trajectories) are uploaded to GCS:
```text
############################################################
# [GCP :: Cloud Storage] UPLOAD COMPLETE                   #
# Service: Google Cloud Storage (GCS)                     #
# Target:  gs://atsnt-lake-mi-facturador-bot-01/simulations/monte_carlo_1791544244.parquet
# Size:    45230 bytes (44.17 KB)                         #
# Type:    application/octet-stream                       #
############################################################
```

