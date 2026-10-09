# ATSNT Project Roadmap

## Architectural Overview
ATSNT is a modular, high-reliability algorithmic trading engine built 100% in Rust following Hexagonal Architecture.

---

## Roadmap Milestones

### Milestone 1: Core Domain, CUSUM Strategy & 1:1 Backtest Engine
- [x] Cargo Workspace multi-crate setup (`domain`, `strategies`, `adapters`, `backtest`; `web` decommissioned in Milestone 5).
- [x] Zero floating-point rule enforced with `rust_decimal::Decimal`.
- [x] Domain models (`Trade`, `DollarBar`, `DollarBarAggregator` with continuous rollover).
- [x] Mathematical indicators: `RollingZScore` and symmetric `CusumFilter`.
- [x] Execution lifecycle state machine (`OrderIntent` $\rightarrow$ `Order` FSM $\rightarrow$ `Position`).
- [x] Strategy Trait and `DollarBarsCusumStrategy` with Triple Barrier exits.
- [x] 1:1 Backtest Engine modeling fee drag, slippage, and quantitative metrics (Sortino, Expectancy, Drawdown).
- [x] Streaming Binance `aggTrades` CSV parser in `adapters`.
- [x] End-to-end executable CLI runner (`cargo run -p backtest`).

---

### Milestone 2: Real Market Data ETL, HPO Engine & Monte Carlo Validation
- [x] Binance data fetcher utility to download official `aggTrades` daily/monthly datasets and convert to Parquet.
- [x] Multi-threaded CPU parallelism engine powered by `rayon`.
- [x] Walk-Forward Optimization (WFO) with Purged & Embargoed temporal splits (AFML Ch. 7).
- [x] Parameter Stability Plateau scoring (anti-overfitting).
- [x] Deflated Sharpe Ratio (DSR) statistical significance testing.
- [x] Discrete Event Monte Carlo Stress-Testing (Trade bootstrapping, Ruin probability, Drawdown percentiles).
- [x] Automated JSON persistence for HPO results, Backtest manifests, and audit telemetry (`storage/reports/`).

---

### Milestone 3: Live Market Data Ingestion (WebSocket Adapter)
- [x] Implement asynchronous Binance WebSocket client in `crates/adapters` (`wss://fstream.binance.com/ws/{symbol}@aggTrade`).
- [x] Reconnection state machine with exponential backoff and heartbeat ping/pong.
- [x] Stream real-time `Trade` events directly into `DollarBarAggregator`.

---

### Milestone 4: Real-Time Paper Trading Engine
- [x] Connect `BacktestEngine` to live WebSocket stream.
- [x] Real-time order matching against live bid/ask spreads.
- [x] Mark-to-market live PnL and active position telemetry without risking capital.

---

### Milestone 5: Web Decommission & Decoupled Cloud-First Analytics
*(Architecture transition: `crates/web` decommissioned in favor of zero-overhead, air-gapped Cloud-First reporting)*
- [x] Decommission and remove `crates/web` (Axum server, static SPA assets, and web dependencies).
- [x] Migrate asynchronous paper trading runtime (`PaperSessionRuntime` & `PaperSessionOwner`) to `crates/backtest`.
- [x] Reorganize quantitative sampling and filters into pure module `domain::analytics` (`sampling`, `filters`, `stats`).
- [x] Workspace dependencies sanitized (`axum`, `tower-http`, `tokio-stream` removed).
- [x] *(Phase 2)* GCP Cloud-First Data Layer: BigQuery / GCS Parquet exporter, Telegram alerting, and Looker Studio BI integration ([`docs/architecture/10-gcp-cloud-architecture.md`](architecture/10-gcp-cloud-architecture.md)).

---

### Milestone 6: Live Execution Gateway (Real Exchange Trading)
*(Architecture specification: [`docs/architecture/08-live-execution-gateway.md`](architecture/08-live-execution-gateway.md))*
- [x] Authenticated REST & WebSocket order gateway in `crates/adapters`.
- [x] HMAC-SHA256 signature generator for Binance API keys with official test vector validation.
- [x] Pre-trade risk manager (Account margin checks, balance verification, circuit breakers).
- [x] Order reconciliation listener for exchange user data stream.
- [x] End-to-end `live_trading` runner against Binance Futures Testnet & Production with automated BigQuery telemetry.

---

### Cloud-First Architecture & Looker Studio Integration (Milestone 5 Phase 2)
*(Architecture specification: [`docs/architecture/10-gcp-cloud-architecture.md`](architecture/10-gcp-cloud-architecture.md))*
- [x] **De-risking & Zero-Trust**: In-process web servers are retired to ensure the trading engine runs headless, air-gapped in private VPC networks without public open ports.
- [x] **Reporting & Business Intelligence**: Post-trade auditing, Monte Carlo trajectories, and HPO parameter evaluations stream to BigQuery / GCS Parquet for visualization via Looker Studio (zero-code frontend maintenance).
- [x] **Proactive Mobile Alerts**: Execution fills, risk threshold breaches, and circuit breakers trigger outbound webhooks (Telegram/Discord) directly to operators' devices.

---

### Milestone 7: Strategy Catalog & Machine Learning Expansion
- [ ] Microstructure feature pipelines (Order Flow Imbalance, Volume-Synchronized Probability of Toxicity - VPIN).
- [ ] Pure Rust ML integration (`ort` for ONNX models / native Rust inference).
- [ ] Multi-asset portfolio balancing and risk parity allocation.
