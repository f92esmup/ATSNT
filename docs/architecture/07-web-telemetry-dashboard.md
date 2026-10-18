# Cloud-First BI Telemetry & Real-Time Alerting Specification

> [!NOTE]
> **Evolution Note (Milestone 5 Transition):**
> This specification supersedes the former in-process web presentation crate (`crates/web`). Under Milestone 5, the in-process Axum web server and static SPA frontend were **completely decommissioned** to achieve true air-gapped isolation, eliminate open inbound network ports, and replace custom UI maintenance with institutional Business Intelligence (**Google Cloud BigQuery as the analytics store, read by downstream consumers**) and instant mobile push notifications (**Telegram Bot Webhooks**).

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
│ (atsnt_bi)       │  │  (GCS artifacts) │  │    (@BotFather webhook)    │
└────────┬─────────┘  └──────────────────┘  └──────────────┬─────────────┘
         │ Read-only view queries                          │ Push Notification
         ▼                                                 ▼
┌────────────────────────────────────────┐  ┌────────────────────────────┐
│      Analytics consumers (read-only)   │  │    Mobile Push Alerts      │
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
2. **Zero Maintenance Analytics (Downstream Consumers)**:
   - Data scientists and operators consume normalized tabular SQL data directly from the `v_*` views in their own tools (see [13-bi-data-contract.md](13-bi-data-contract.md)).
   - Zero JavaScript/CSS/HTML maintenance; zero client-side WebSocket synchronization bugs.
3. **Instant Mobile Situational Awareness (Telegram)**:
   - Critical operational events (position entries, exits, risk threshold violations) are pushed directly to mobile devices in under 200 milliseconds.
4. **Massive Scale at Minimal Cost**:
   - Monte Carlo fan-chart curves are stored as event-based series rows in `atsnt_bi.run_series` (a handful of percentile curves per run), not as one file per trajectory.

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

`paper_trading` and `live_trading` never wait on Telegram: alerts go into a bounded queue (`backtest::operator_alerts::AlertQueue`, 64 messages) drained by its own task. When the queue is full a message is dropped and counted in the log; a slow or unreachable Telegram cannot delay a trade. Pending alerts get 5 seconds to leave at shutdown.

What an operator is told (`paper` and `live` unless noted):

| Alert | Trigger | Rate limit |
|---|---|---|
| 💓 Heartbeat | At start (doubles as the start notice) and every 12 h: equity, open position, age of the last market trade | 1 per 12 h |
| 🛑 Stopped | Graceful shutdown (Ctrl+C / SIGINT): equity and open position | once |
| ⚠️ Feed down | More than 60 s without a market trade, or 3 failed WebSocket reconnects in a row | 1 per 10 min |
| ✅ Feed back | First market trade after a reported outage | once per outage |
| 🟢 Position opened / 🔴 closed | Entry fill / exit with net PnL | none |
| 🚨 Circuit breaker | Monthly breaker (live), daily or monthly breaker (paper) | once per trip |
| 🚫 Order rejected (live) | An order reported `REJECTED` on the user data stream (once per order id), or an entry refused by the exchange REST API | per order id / 1 per min |
| 🚨 Exits, entry timeouts, account drift (live) | Protective-order failures, unresolved entries, account profile mismatch or unverifiable account | as before |
| 📊 Daily summary | 20:00 Madrid | 1 per day |

A process crash cannot be reported by the process itself. Every restart sends a heartbeat, so a crash loop shows as repeated start notices; `docs/deployment/atsnt-alert@.service` (installed by hand) also sends a notice when systemd stops restarting the service.

---

## 3. Data Warehouse Schema (`atsnt_bi`)

The engine streams run-centric rows into four append-only tables (`runs`, `run_metrics`, `trades`, `run_series`) and exposes deduplicated `v_*` views. The DDL is `scripts/gcp/bigquery_schema.sql`; the columns, semantics and rules of change that consumers can rely on are the contract in [13-bi-data-contract.md](13-bi-data-contract.md). (The earlier per-purpose tables `trades`, `equity_snapshots`, `hpo_evaluations` and `monte_carlo_runs` described in previous revisions of this document were replaced by this model and are dropped by `scripts/gcp/bigquery_drop_legacy.sql`.)

---

## 4. CLI Execution & Operation

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

# 3. Backtest & Monte Carlo with BigQuery
cargo run -p backtest -- \
  --monte-carlo \
  --mc-iterations 10000 \
  --gcp-bigquery
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
