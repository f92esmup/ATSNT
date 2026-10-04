# ATSNT - Algorithmic Trading System Native in Rust

A high-performance, modular algorithmic trading engine built 100% in Rust from first principles. Focuses on system architecture, event-driven concurrency, deterministic backtesting, anti-overfitting optimization, and financial correctness.

---

## 1. Architectural Philosophy

- **Zero Black-Box Frameworks**: We deliberately avoid monolithic trading frameworks (e.g. NautilusTrader). We design and own domain models, state machines, indicators, risk managers, and execution pipelines from first principles.
- **Hexagonal Architecture (Ports & Adapters)**: Strict separation of concerns. Pure business logic has zero I/O or network dependencies; infrastructure adapters plug in via abstract traits.
- **Financial Correctness (Zero-Float Rule)**: Never use `f32` or `f64` for quantities, prices, or account balances. All financial calculations use fixed-point arithmetic via `rust_decimal::Decimal`.
- **Zero-Toy Assumptions**: Every backtest models realistic fee schedules (maker/taker), latency-induced slippage, and Triple Barrier exits.
- **Anti-Overfitting Validation**: Parameter optimization enforces Walk-Forward temporal splits with Embargo periods (AFML Ch. 7), Parameter Stability Plateau scoring, Deflated Sharpe Ratio (DSR), and Discrete Event Monte Carlo trade sequence bootstrapping.

---

## 2. Workspace Structure

```text
ATSNT/
├── crates/
│   ├── domain/       # Pure business logic (DollarBar, Aggregator, CusumFilter, Z-Score, Order FSM, Position)
│   ├── strategies/   # Strategy traits & implementations (DollarBarsCusumStrategy with Triple Barrier exits)
│   ├── adapters/     # Ports & infrastructure adapters (Binance CSV, Parquet columnar, Binance ETL fetcher)
│   ├── backtest/     # 1:1 Event Simulator, Walk-Forward Splitter, Rayon HPO Engine, Monte Carlo Simulator
│   └── web/          # REST & WebSocket telemetry server (Axum)
├── data/             # Historical market data (CSV samples & compressed Parquet archives)
├── configs/          # Serialized strategy parameters and winning HPO artifacts
├── storage/reports/  # Machine-readable JSON telemetry reports (Backtests, HPO sweeps, Monte Carlo fan charts)
└── docs/             # Mathematical specifications and architecture guides
```

---

## 3. Quickstart & Command Reference

### Prerequisites
- Rust 1.80+ (managed via `rustup` or `mise`).

### Verify & Test Suite
Run all unit and integration tests across the workspace:
```bash
cargo test --all
```
Run strict lint and formatting verification:
```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

---

### Step 1: Download & Convert Real Market Data (Binance ETL)
Download official Binance Futures USDT-M `aggTrades` archives directly from `data.binance.vision` and convert them into compressed Apache Parquet format:

```bash
# Download and convert an entire year automatically (all 12 months)
cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2024

# Download a specific month (e.g. January 2024)
cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2024 --month 1

# Download a single day (e.g. 2024-01-01) -> ~760k ticks in ~5MB Parquet
cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2024 --month 1 --day 1
```

---

### Step 2: Run Deterministic 1:1 Backtest
Execute the event-driven backtest simulator against either CSV or Parquet market data:

```bash
# Run against the synthetic sample trades
cargo run -p backtest -- --data data/sample_trades.csv --dollar-bar 100000

# Run against real Binance Parquet data with $1,000,000 Dollar Bars
cargo run -p backtest -- --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet --dollar-bar 1000000

# Run with a saved HPO configuration
cargo run -p backtest -- --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet --config configs/hpo_results.json
```

---

### Step 3: Run Monte Carlo Stress Simulation
Audit backtest trade sequences to test drawdown probability distributions and ruin risk:

```bash
cargo run -p backtest -- --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet --dollar-bar 1000000 --monte-carlo --mc-iterations 10000
```
- Performs **Circular Block Bootstrap (CBB)** resampling over realized trades.
- Computes drawdown percentiles ($p_1, p_5, p_{25}, p_{50}, p_{75}, p_{95}, p_{99}$, worst case).
- Calculates Probability of Ruin ($P_{\text{ruin}}$) for critical account drawdowns (e.g. 30%).
- Exports a 50-curve **Fan Chart** to `storage/reports/` for instant web dashboard rendering.

---

### Step 4: Run Parallel Walk-Forward Hyperparameter Optimization (HPO)
Optimize strategy parameters across temporal folds using multi-core CPU parallelism with `Rayon`:

```bash
cargo run -p backtest --bin run_hpo -- \
  --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet \
  --dollar-bar 1000000 \
  --folds 3 \
  --train-ratio 0.70 \
  --embargo-bars 20 \
  --candidates 50
```
- **Embargo Buffering**: Quarantines bars between train and test windows to prevent indicator lookahead leakage.
- **Plateau Stability Score**: Evaluates parameter neighborhoods ($\pm \delta$) and penalizes fragile overfitting spikes.
- **Deflated Sharpe Ratio (DSR)**: Assesses statistical significance ($p < 0.05$) adjusted for trial count.
- **Automated Serialization**: Saves the winning configuration to `configs/hpo_results.json` and full audit telemetry to `storage/reports/hpo_run_<timestamp>.json`.

---

## 4. Documentation Index

- [`AGENTS.md`](AGENTS.md): Architectural standards, engineering guidelines, and agent rules.
- [`docs/ROADMAP.md`](docs/ROADMAP.md): Milestone progress and upcoming deliverables.
- [`docs/strategy/01-dollar-bars-cusum.md`](docs/strategy/01-dollar-bars-cusum.md): Mathematics of Dollar Bars, symmetric CUSUM filter, rolling Z-Scores, and Triple Barrier exits.
- [`docs/architecture/02-domain-models-and-lifecycle.md`](docs/architecture/02-domain-models-and-lifecycle.md): Domain models and 3-stage execution lifecycle (`OrderIntent` -> `Order` FSM -> `Position`).
- [`docs/architecture/03-adapters-and-market-data.md`](docs/architecture/03-adapters-and-market-data.md): Ports and Adapters, Binance `aggTrades` mapping, and operational modes.
- [`docs/architecture/04-hpo-and-walk-forward.md`](docs/architecture/04-hpo-and-walk-forward.md): Walk-Forward Optimization, parameter stability scoring, DSR, and Rayon parallelism.
- [`docs/architecture/05-monte-carlo-and-telemetry.md`](docs/architecture/05-monte-carlo-and-telemetry.md): Discrete Event Monte Carlo Stress-Testing, trade sequence bootstrap, and web telemetry schemas.

---

## 5. License
Dual-licensed under MIT or Apache-2.0.
