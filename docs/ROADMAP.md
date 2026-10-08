# ATSNT Project Roadmap

## Architectural Overview
ATSNT is a modular, high-reliability algorithmic trading engine built 100% in Rust following Hexagonal Architecture.

---

## Roadmap Milestones

### Milestone 1: Core Domain, CUSUM Strategy & 1:1 Backtest Engine
- [x] Cargo Workspace multi-crate setup (`domain`, `strategies`, `adapters`, `backtest`, `web`).
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

### Milestone 5: Web Presentation & Telemetry Dashboard (`crates/web`)
*(Architecture specification: [`docs/architecture/07-web-telemetry-dashboard.md`](architecture/07-web-telemetry-dashboard.md))*
- [x] Axum HTTP & WebSocket server in `crates/web`.
- [x] Real-time streaming of Dollar Bars, signals, and open positions over `/ws/telemetry`.
- [x] Web dashboard Single Page Application with Lightweight Charts (TradingView) dark financial terminal.
- [x] Multi-asset and multi-strategy ready architecture with Zero-Trust perimeter deployment model.

---

### Milestone 6: Live Execution Gateway (Real Exchange Trading)
*(Architecture specification: [`docs/architecture/08-live-execution-gateway.md`](architecture/08-live-execution-gateway.md))*
- [x] Authenticated REST & WebSocket order gateway in `crates/adapters`.
- [x] HMAC-SHA256 signature generator for Binance API keys with official test vector validation.
- [x] Pre-trade risk manager (Account margin checks, balance verification, circuit breakers).
- [x] Order reconciliation listener for exchange user data stream.

---

### Proposed Workstream: Read-Only Web Workspace (W0–W6)
*(Canonical product/UI contract and phase acceptance: [Read-Only Web Workspace](architecture/09-read-only-web-workspace.md). Implementation remains gated by W0 decisions.)*

- W0 approves product topology/package ownership, data sources/identity, Dollar Bars workflow, metric attribution/formulas, visual/accessibility and auth/deployment boundaries.
- W1 builds the shared shell; W2 consumes existing T2b snapshot/freshness resilience and T3 real paper-session wiring, without duplicating their tasks. W3 Operations depends on W1/W2.
- W4 research history/detail may proceed independently of paper runtime after W0/W1 and report-contract agreement; W5 analysis/recorded replay depends on W4.
- W6 integrates supporting sections and reversible UI retirement, intersecting T7 milestone closure/browser evidence.

The Milestone 5/6 closure work (T1–T7) is now 100% complete, verified and closed on `main` (detailed evidence in the [milestone closure tracker](../odd/tasks/milestone-5-6-closure.md)). All prerequisites for telemetry reliability, explicit in-process paper trading, Spot funding guards, shared risk sizing and private account events are fulfilled. The upcoming work focuses on the [Read-Only Web Workspace (W0–W6)](architecture/09-read-only-web-workspace.md), gated by W0 decisions. Portal/Econoweb were not found in this repository, so topology and integration ownership remain W0 gates, not implemented modules.

---

### Milestone 7: Strategy Catalog & Machine Learning Expansion
- [ ] Microstructure feature pipelines (Order Flow Imbalance, Volume-Synchronized Probability of Toxicity - VPIN).
- [ ] Pure Rust ML integration (`ort` for ONNX models / native Rust inference).
- [ ] Multi-asset portfolio balancing and risk parity allocation.
