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
- [x] Automated JSON persistence for HPO results, Backtest manifests, and audit telemetry (`storage/reports/`; superseded by the run-centric BigQuery model).
- [x] Automated end-to-end production staging pipeline (`scripts/run_production_pipeline.sh`) with native YTD ETL and `systemd-inhibit` sleep locking.

---

### Milestone 3: Live Market Data Ingestion (WebSocket Adapter)
- [x] Implement asynchronous Binance WebSocket client in `crates/adapters` (`wss://fstream.binance.com/ws/{symbol}@aggTrade`).
- [x] Reconnection state machine with exponential backoff and heartbeat ping/pong.
- [x] Stream real-time `Trade` events directly into `DollarBarAggregator`.

---

### Milestone 4: Real-Time Paper Trading Engine
- [x] Connect `BacktestEngine` to live WebSocket stream.
- [x] Fills through the live trading session on a tick-level simulated venue (latency, tape-walk fills; no bid/ask book): see the fidelity gaps below.
- [x] Mark-to-market live PnL and active position telemetry without risking capital.

---

### Milestone 5: Web Decommission & Decoupled Cloud-First Analytics
*(Architecture transition: `crates/web` decommissioned in favor of zero-overhead, air-gapped Cloud-First reporting)*
- [x] Decommission and remove `crates/web` (Axum server, static SPA assets, and web dependencies).
- [x] Migrate asynchronous paper trading runtime (`PaperSessionRuntime` & `PaperSessionOwner`) to `crates/backtest`.
- [x] Reorganize quantitative sampling and filters into pure module `domain::analytics` (`sampling`, `filters`, `stats`).
- [x] Workspace dependencies sanitized (`axum`, `tower-http`, `tokio-stream` removed).
- [x] *(Phase 2)* GCP Cloud-First Data Layer: BigQuery / GCS Parquet exporter, Telegram alerting, and a BI data contract for downstream consumers ([`docs/architecture/10-gcp-cloud-architecture.md`](architecture/10-gcp-cloud-architecture.md)).

---

### Milestone 6: Live Execution Gateway (Real Exchange Trading)
*(Architecture specification: [`docs/architecture/08-live-execution-gateway.md`](architecture/08-live-execution-gateway.md))*
- [x] Authenticated REST & WebSocket order gateway in `crates/adapters`.
- [x] HMAC-SHA256 signature generator for Binance API keys with official test vector validation.
- [x] Pre-trade risk manager (Account margin checks, balance verification, circuit breakers).
- [x] Order reconciliation listener for exchange user data stream.
- [x] End-to-end `live_trading` runner against Binance Futures Testnet & Production with automated BigQuery telemetry.

---

### Cloud-First Architecture & BI Data Contract (Milestone 5 Phase 2)
*(Architecture specification: [`docs/architecture/10-gcp-cloud-architecture.md`](architecture/10-gcp-cloud-architecture.md))*
- [x] **De-risking & Zero-Trust**: In-process web servers are retired to ensure the trading engine runs headless, air-gapped in private VPC networks without public open ports.
- [x] **Reporting & Business Intelligence**: Post-trade auditing, Monte Carlo trajectories, and HPO parameter evaluations stream to BigQuery / GCS Parquet for visualization by downstream consumers (the engine ships no frontend).
- [x] **Proactive Mobile Alerts**: Execution fills, risk threshold breaches, and circuit breakers trigger outbound webhooks (Telegram/Discord) directly to operators' devices.

---

### Run-Centric Analytics Data Model
*(Feature log: [`odd/tasks/data-model-refactor.md`](../odd/tasks/data-model-refactor.md); schema: [`scripts/gcp/bigquery_schema.sql`](../scripts/gcp/bigquery_schema.sql))*
- [x] Common `RunSpec` contract (strategy params as JSON, sampling, execution context) shared by backtest, HPO, Monte Carlo, paper and live runs, keyed by UUID v7 `run_id` with parent/child lineage.
- [x] BigQuery tables `runs`, `run_metrics`, `trades`, `run_series` with latest-row views; legacy `equity_snapshots`, `hpo_evaluations`, `monte_carlo_runs` dropped.
- [x] Event-based equity (entry/trough/peak/exit per trade with MAE/MFE) replacing time-sampled snapshots; per-trade drawdown reproduces the engine max drawdown exactly.
- [x] Local JSON reports and GCS Parquet exports removed; GCS keeps only deployment artifacts.
- [x] BI data contract for consumers of the `v_*` views: [`docs/architecture/12-bi-data-contract.md`](architecture/12-bi-data-contract.md).
- [ ] ETL backlog requested by consumers (gaps G1-G15 in the contract), starting with `baseline_run_id` and the quote currency of each run.
- [x] Perpetual futures funding payments are modelled as cash flows inside each trade's net PnL (`funding_paid`; not yet a separate BigQuery column).

---

### Simulation Fidelity (simulated vs live parity)
*(Feature log: [`odd/tasks/sim-live-fidelity.md`](../odd/tasks/sim-live-fidelity.md))*
- [x] Live sizing, drawdown limit and leverage cap come from the deployment artifact, as in the simulator.
- [x] Live daily drawdown breaker uses the per-UTC-day peak, as in the simulator.
- [x] Live PnL reports the commissions Binance charged instead of an estimate.
- [x] Bar-level engine: entries are market orders that fill at the next bar's open, the strategy sees every bar, TP/SL exits are taker orders with slippage and gap-aware fills.
- [x] Paper trading runs the live `TradingSession` on the tick-level `SimulatedVenue`.
- [x] Perpetual funding at every settlement and isolated-margin liquidation in both the bar-level engine and the tick-level venue (shared rules in `domain::futures`).
- [x] Tick-level `SimulatedVenue` (`--fill-model tick`): order latency, tape-walk market fills and tape-armed conditional exits, driven through the live `TradingSession`; used to validate finalists, while the bar-level engine stays the optimizer loop.
- [ ] Known gaps, not yet modeled by the simulator: real mark-price stop triggers (last trade price is the proxy), bid/ask spread, notional-bracket maintenance margins, and live warm-up of rolling strategy windows.
- [x] Strategy warm-up from recent trades (`--warmup-minutes` in live and paper) and a parity harness (`live_trading --record-fills` + `parity_check`) that recommends the simulator's order latency from real fills.
- [x] One session core shared by simulation, paper and live: `ExecutionVenue` port, `TradingSession` and the `SessionDriver` loop. Separate `Clock` and `MarketData` venue ports were judged unnecessary (time comes from the trade stream and market data already has `MarketDataStream`).
- [x] Single strategy registry (`strategies::registry`, one line per strategy) and a generic contract suite; backtest, HPO, paper and live resolve strategies through it.

---

### Milestone 7: Strategy Catalog & Microstructure Expansion
- [x] Microstructure feature pipeline: `OrderFlowImbalanceStrategy` (OFI metric, rolling Z-score standardization, price efficiency absorption ratio, dynamic Triple Barrier exits, and friction filters).
- [x] Quantitative engine hardening:
  - Exact slippage accounting: removed double-deductions in net PnL and cash equity; exact cash identity $\text{Cash} = \text{Initial Capital} + \sum \text{Net PnL}$ verified.
  - 00:00:00 UTC daily circuit breaker rollover: watermark and halt reset every midnight UTC for continuous multi-month backtesting.
  - Binance execution parity: leverage cap, lot-step flooring and minimum lot/notional rejection in `RiskPolicy::size_order`; live gateway uses per-symbol exchange filters.
  - Monthly kill-switch (loss versus UTC month-open equity): closes at market, halts the run, flags `halted` in metrics, emits a paper event and blocks live entries.
  - Denser CUSUM HPO grid with a fixed 100-bar time barrier.
  - Microstructure friction guard (`min_take_profit_pct`): prevents executing sub-friction trades in compressed volatility regimes.
- [ ] Volume-Synchronized Probability of Toxicity (VPIN) pipeline.
- [ ] Pure Rust ML integration (`ort` for ONNX models / native Rust inference).
- [ ] Multi-asset portfolio balancing and risk parity allocation.
