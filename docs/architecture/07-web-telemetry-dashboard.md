# Cloud-First BI Telemetry & Real-Time Alerting Specification

> [!NOTE]
> **Evolution Note (Milestone 5 Transition):**
> This specification supersedes the former in-process web presentation crate (`crates/web`). Under Milestone 5, the in-process Axum web server and static SPA frontend were **completely decommissioned** to achieve true air-gapped isolation, eliminate open inbound network ports, and replace custom UI maintenance with institutional Business Intelligence (**Google Cloud BigQuery + Looker Studio**) and instant mobile push notifications (**Telegram Bot Webhooks**).

---

## 1. Architectural Philosophy: Air-Gapped Cloud-First Analytics

The trading engine operates as a strictly headless, outbound-only system. It exposes **zero open ports** (no HTTP, no WebSockets), preventing network-based lateral movements or unauthorized web access.

```text
┌────────────────────────────────────────────────────────────────────────┐
│               ATSNT Headless Trading Engine (100% Rust)                │
│                                                                        │
│   ┌─────────────────────┐  ┌────────────────┐  ┌───────────────────┐   │
│   │    paper_trading    │  │    run_hpo     │  │     backtest      │   │
│   └──────────┬──────────┘  └───────┬────────┘  └─────────┬─────────┘   │
│              │                     │                     │             │
│              └───────────────┬─────┴─────────────────────┘             │
│                              ▼                                         │
│                    crates/adapters::gcp                                │
│       ┌──────────────────────┼──────────────────────┐                  │
│       │                      │                      │                  │
│       ▼                      ▼                      ▼                  │
│  BigQuerySink          GcsParquetSink        TelegramNotifier          │
└───────┼──────────────────────┼──────────────────────┼──────────────────┘
        │ Outbound HTTPS       │ Outbound HTTPS       │ Outbound HTTPS
        ▼                      ▼                      ▼
┌──────────────────┐  ┌──────────────────┐  ┌────────────────────────────┐
│ Google BigQuery  │  │  Google Storage  │  │    Telegram Bot API        │
│ (atsnt_bi)       │  │  (GCS Parquet)   │  │    (@BotFather webhook)    │
└────────┬─────────┘  └──────────────────┘  └──────────────┬─────────────┘
         │ Direct 1-Click Connection                       │ Push Notification
         ▼                                                 ▼
┌────────────────────────────────────────┐  ┌────────────────────────────┐
│      Looker Studio (Google BI)         │  │    Mobile Push Alerts      │
│  - Real-time Equity & Drawdown Curves  │  │  - Position Opened/Closed  │
│  - Post-Trade Performance Attribution  │  │  - Stop Loss & Take Profit │
│  - Monte Carlo Probability of Ruin     │  │  - Circuit Breaker Trips   │
│  - Walk-Forward HPO Parameter Surfaces │  │  - Sub-200ms Latency       │
│  - 100% Read-Only, Zero Frontend Code  │  │                            │
└────────────────────────────────────────┘  └────────────────────────────┘
```

### Key Principles

1. **True Air-Gapped Headless Execution (Zero Inbound Ports)**:
   - The engine does not bind to any network interface.
   - All external interactions with the monitoring layer are strictly **outbound HTTPS POST requests** (to BigQuery streaming insert API, GCS upload API, and Telegram webhook).
2. **Zero Maintenance BI Dashboards (Looker Studio)**:
   - Data scientists and operators consume normalized tabular SQL data directly in Looker Studio.
   - Zero JavaScript/CSS/HTML maintenance; zero client-side WebSocket synchronization bugs.
3. **Instant Mobile Situational Awareness (Telegram)**:
   - Critical operational events (position entries, exits, risk threshold violations) are pushed directly to mobile devices in under 200 milliseconds.
4. **Massive Scale at Minimal Cost**:
   - Heavy simulation curves (e.g. 10,000 Monte Carlo trajectories) are compressed into columnar Apache Parquet files and archived in GCS, readable on demand.

---

## 2. Ports & Adapters Architecture (`crates/adapters`)

In accordance with Hexagonal Architecture:

```text
crates/adapters/src/
├── traits.rs           # AlertNotifier trait definition
├── gcp/
│   ├── mod.rs          # Module declarations and RFC 3339 timestamp helpers
│   ├── bigquery.rs     # BigQuerySink & structured DTO rows (TradeRow, EquitySnapshotRow, etc.)
│   ├── gcs.rs          # GcsParquetSink (Arrow schema definition & Parquet serializer)
│   └── telegram.rs     # TelegramNotifier (Markdown alert generator & HTTP dispatcher)
```

### 2.1 The Alerting Port (`crates/adapters/src/traits.rs`)

```rust
#[async_trait]
pub trait AlertNotifier: Send + Sync {
    async fn notify(&self, message: &str) -> Result<(), AdapterError>;
}
```

### 2.2 Telegram Mobile Alerting (`TelegramNotifier`)

The notifier formats events with visual cues and dispatches them asynchronously:
- 🟢 **Position Opened**: Symbol, Side, Entry Price, Size, Stop Loss, and Take Profit.
- 🔴 **Position Closed**: Exit Reason (Stop Loss, Take Profit, Time Barrier), Net PnL in USDT, Holding Duration.
- ⚠️ **Circuit Breaker / Risk Violation**: Session drawdown breach, order sizing rejection.

---

## 3. Data Warehouse Schemas & BigQuery DDL (`atsnt_bi`)

The engine streams tabular metrics directly into four analytical tables:

### 3.1 `atsnt_bi.trades` (Execution Audit & Trade Journal)
```sql
CREATE TABLE IF NOT EXISTS atsnt_bi.trades (
    trade_id STRING NOT NULL,
    session_id STRING NOT NULL,
    strategy_id STRING NOT NULL,
    symbol STRING NOT NULL,
    side STRING NOT NULL,               -- 'Buy' | 'Sell'
    entry_timestamp TIMESTAMP NOT NULL,
    exit_timestamp TIMESTAMP NOT NULL,
    entry_price NUMERIC NOT NULL,
    exit_price NUMERIC NOT NULL,
    quantity NUMERIC NOT NULL,
    gross_pnl NUMERIC NOT NULL,
    fees_paid NUMERIC NOT NULL,
    net_pnl NUMERIC NOT NULL,
    exit_reason STRING NOT NULL,        -- 'StopLoss' | 'TakeProfit' | 'TimeBarrier'
    holding_duration_seconds INT64
)
PARTITION BY DATE(exit_timestamp)
CLUSTER BY symbol, strategy_id;
```

### 3.2 `atsnt_bi.equity_snapshots` (Account Balance & Drawdown Telemetry)
```sql
CREATE TABLE IF NOT EXISTS atsnt_bi.equity_snapshots (
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

### 3.3 `atsnt_bi.hpo_evaluations` (Hyperparameter Optimization Surface)
```sql
CREATE TABLE IF NOT EXISTS atsnt_bi.hpo_evaluations (
    run_id STRING NOT NULL,
    timestamp TIMESTAMP NOT NULL,
    strategy_id STRING NOT NULL,
    parameter_space STRING NOT NULL,     -- JSON: {"dollar_threshold": 1000000, "z_entry": 2.1...}
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

### 3.4 `atsnt_bi.monte_carlo_runs` (Quantitative Stress Testing Summary)
```sql
CREATE TABLE IF NOT EXISTS atsnt_bi.monte_carlo_runs (
    run_id STRING NOT NULL,
    timestamp TIMESTAMP NOT NULL,
    strategy_id STRING NOT NULL,
    iterations INT64 NOT NULL,
    resample_method STRING NOT NULL,     -- 'IID' | 'CircularBlockBootstrap'
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

## 4. Massive Trajectory Archival (GCS Parquet)

For detailed Monte Carlo stress simulations (e.g. 10,000 synthetic equity curves with 500 points each):
1. Trajectories are encoded in memory using Apache Arrow `Float64Array` columns:
   - `trajectory_id`: Integer identifier.
   - `step_index`: Trade or bar index.
   - `equity`: Simulated equity level.
2. Compressed using Snappy into Apache Parquet format.
3. Uploaded to `gs://atsnt-lake/simulations/monte_carlo_<timestamp>.parquet`.
4. BigQuery can optionally query this bucket directly via **External Tables** (`CREATE EXTERNAL TABLE ... OPTIONS (format = 'PARQUET')`).

---

## 5. CLI Execution & Operation

All three core analytics runners support Cloud-First export flags:

```bash
# 1. Paper Trading with BigQuery and Telegram Alerts
cargo run -p backtest --bin paper_trading -- \
  --symbol btcusdt \
  --gcp-bigquery \
  --telegram-alerts

# 2. Walk-Forward HPO with parameter surface streaming
cargo run -p backtest --bin run_hpo -- \
  --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet \
  --gcp-bigquery

# 3. Backtest & Monte Carlo with BigQuery and GCS Parquet
cargo run -p backtest -- \
  --monte-carlo \
  --mc-iterations 10000 \
  --gcp-bigquery \
  --gcs-parquet
```

### Environment Configuration (Fail-Open / Offline Safe)
If credentials are not set, all sinks log information via `tracing` without erroring or interrupting engine execution:
```bash
export GCP_PROJECT_ID="your-project-id"
export GCP_DATASET_ID="atsnt_bi"
export GCP_AUTH_TOKEN="$(gcloud auth print-access-token)"
export GCS_BUCKET_NAME="atsnt-lake"
export TELEGRAM_BOT_TOKEN="123456789:ABC..."
export TELEGRAM_CHAT_ID="your_chat_id"
```
