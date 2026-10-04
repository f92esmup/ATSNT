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
- [ ] Multi-threaded CPU parallelism engine powered by `rayon`.
- [ ] Walk-Forward Optimization (WFO) with Purged & Embargoed temporal splits (AFML Ch. 7).
- [ ] Bayesian Optimization & Parameter Stability Plateau scoring (anti-overfitting).
- [ ] Deflated Sharpe Ratio (DSR) statistical significance testing.
- [ ] Discrete Event Monte Carlo Stress-Testing (Trade bootstrapping, Ruin probability, Drawdown percentiles).
- [ ] Automated JSON persistence for HPO results, Backtest manifests, and Monte Carlo fan-chart telemetry (`storage/reports/`).

---

### Milestone 3: Live Market Data Ingestion (WebSocket Adapter)
- [ ] Implement asynchronous Binance WebSocket client in `crates/adapters` (`wss://fstream.binance.com/ws/{symbol}@aggTrade`).
- [ ] Reconnection state machine with exponential backoff and heartbeat ping/pong.
- [ ] Stream real-time `Trade` events directly into `DollarBarAggregator`.

---

### Milestone 4: Real-Time Paper Trading Engine
- [ ] Connect `BacktestEngine` to live WebSocket stream.
- [ ] Real-time order matching against live bid/ask spreads.
- [ ] Mark-to-market live PnL and active position telemetry without risking capital.

---

### Milestone 5: Web Presentation & Telemetry Dashboard (`crates/web`)
- [ ] Axum HTTP & WebSocket server in `crates/web`.
- [ ] Real-time streaming of Dollar Bars, signals, and open positions.
- [ ] Web dashboard visualizing strategy catalog, backtest equity curves, and performance metrics.
- [ ] Deployment setup for VPS and custom domain reverse proxy (`pescudem.es`).

---

### Milestone 6: Live Execution Gateway (Real Exchange Trading)
- [ ] Authenticated REST & WebSocket order gateway in `crates/adapters`.
- [ ] HMAC-SHA256 signature generator for Binance API keys.
- [ ] Pre-trade risk manager (Account margin checks, balance verification, circuit breakers).
- [ ] Order reconciliation listener for exchange user data stream.

---

### Milestone 7: Strategy Catalog & Machine Learning Expansion
- [ ] Microstructure feature pipelines (Order Flow Imbalance, Volume-Synchronized Probability of Toxicity - VPIN).
- [ ] Pure Rust ML integration (`ort` for ONNX models / native Rust inference).
- [ ] Multi-asset portfolio balancing and risk parity allocation.
