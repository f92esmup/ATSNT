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
│   ├── strategies/   # Strategy traits & implementations (DollarBarsCusumStrategy, OrderFlowImbalanceStrategy)
│   ├── adapters/     # Ports & infrastructure adapters (Binance CSV, Parquet columnar, Binance ETL fetcher, Binance WebSocket Stream, GCP BigQuery/GCS/Telegram)
│   └── backtest/     # 1:1 Event Simulator, Walk-Forward Splitter, Rayon HPO Engine, Monte Carlo, Real-Time Paper Trading
├── data/             # Historical market data (CSV samples & compressed Parquet archives)
├── configs/          # Serialized strategy parameters and winning HPO artifacts
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

### CLI Tools & Binaries Index

The repository provides 6 specialized CLI binaries built with `clap` (run any with `--help` for full parameter documentation):

| Binary / Tool | Crate | Purpose | Quick Command | Detailed Guide |
| :--- | :--- | :--- | :--- | :--- |
| `fetch_data` | `adapters` | Official Binance ETL: downloads historical `aggTrades` & converts to Parquet | `cargo run -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2026 --month 1` | [Step 1](#step-1-download--convert-real-market-data-binance-etl) |
| `backtest` | `backtest` | Deterministic 1:1 event simulator and Monte Carlo stress testing (BigQuery & GCS export) | `cargo run -p backtest -- --data data/sample_trades.csv --strategy dollar_bars_cusum --capital 200 --gcp-bigquery` | [Step 2](#step-2-run-deterministic-11-backtest) & [Step 3](#step-3-run-monte-carlo-stress-simulation) |
| `run_hpo` | `backtest` | Multi-core CPU parallel hyperparameter optimization (Rayon + WFO, BigQuery export) | `cargo run -p backtest --bin run_hpo -- --data <PATH> --strategy dollar_bars_cusum --capital 200 --folds 3 --gcp-bigquery` | [Step 4](#step-4-run-parallel-walk-forward-hyperparameter-optimization-hpo) |
| `stream_trades`| `adapters` | Live Binance WebSocket feed accumulating and printing Dollar Bars | `cargo run -p adapters --bin stream_trades -- --symbol btcusdt --threshold 50000` | [Step 5](#step-5-live-market-data-ingestion--dollar-bar-streaming) |
| `paper_trading`| `backtest` | Real-time simulated execution with continuous mark-to-market PnL, BigQuery & Telegram alerts | `cargo run -p backtest --bin paper_trading -- --symbol btcusdt --spot --capital 1000 --gcp-bigquery --telegram-alerts` | [Step 6](#step-6-run-real-time-paper-trading-engine) |
| `live_gateway` | `adapters` | Authenticated live order gateway with HMAC signing and circuit breakers | `cargo run -p adapters --bin live_gateway -- --testnet --check-balance` | [Step 8](#step-8-live-execution-gateway-binance-testnet--production) |


---

### Automated End-to-End Production Pipeline (`run_production_pipeline.sh`)

For full-year unattended production staging, the script [`scripts/run_production_pipeline.sh`](scripts/run_production_pipeline.sh) chains the entire quant research workflow into a single execution:
1. **Compiles Release Binaries**: Pre-compiles `fetch_data`, `run_hpo`, and `backtest` once with full optimizations.
2. **Phase 1 (Binance ETL)**: Ingests Binance `aggTrades` (full monthly archives + daily YTD up to today) into compressed Parquet.
3. **Phase 2 (Parallel Walk-Forward HPO)**: Evaluates candidate configurations across purged rolling folds with embargo buffering, saving the winning config to JSON.
4. **Phase 3 (Backtest & Monte Carlo)**: Replays trade events through the 1:1 simulator and runs 10,000 Circular Block Bootstrap (CBB) resampled trajectories to audit drawdown percentiles and ruin probability ($P_{\text{ruin}}$).
5. **Sleep/Idle Immunity**: Automatically wraps execution in `systemd-inhibit` (locks sleep, idle, and laptop lid-switch while running).

```bash
# 1. Standard execution in foreground (auto-inhibits sleep):
./scripts/run_production_pipeline.sh --symbol BTCUSDT --year 2026

# 2. Run in background (unattended with nohup & log tracking):
./scripts/run_production_pipeline.sh --symbol BTCUSDT --year 2026 --detach
tail -f pipeline_btcusdt_2026.log

# 3. High-resolution parameter sweep on existing local data:
./scripts/run_production_pipeline.sh --skip-fetch --candidates 100 --mc-iterations 25000
```

#### Pipeline Configuration Flags:
| Flag | Description | Default |
| :--- | :--- | :--- |
| `-s, --symbol <SYM>` | Trading pair symbol | `BTCUSDT` |
| `-y, --year <YYYY>` | Historical or current year | Current year (`2026`) |
| `-m, --month <MM>` | Single completed month: HPO and backtest read only `<SYM>-aggTrades-<YEAR>-<MM>.parquet` (fails fast if missing) | All months YTD |
| `-b, --dollar-bar <N>` | Dollar bar volume threshold (USD) | `1000000` ($1M) |
| `-f, --folds <N>` | Walk-Forward rolling validation folds | `5` |
| `-r, --train-ratio <F>` | In-sample train window ratio | `0.70` (70%) |
| `-e, --embargo-bars <N>`| Quarantined embargo bars between train/test | `30` |
| `-c, --candidates <N>` | Parameter combinations to evaluate | `60` |
| `-i, --mc-iterations <N>`| Monte Carlo bootstrap resample paths | `10000` |
| `-o, --output-dir <DIR>` | Parquet destination root directory | `data/historical_<YEAR>` |
| `--config <PATH>` | Output/input path for winning HPO JSON | `configs/hpo_<sym>_<year>[_<MM>].json` |
| `--gcp-bigquery` | Record HPO, backtest and Monte Carlo runs to BigQuery | `false` |
| `--strict-sinks` | Fail the run if any BigQuery recording or GCS upload fails | `false` |
| `--upload-winner` | Upload the winning artifact to GCS `artifacts/<config stem>-<suffix>.json` | `false` |
| `--artifact-suffix <S>` | Unique suffix for the uploaded artifact | UTC timestamp |
| `--bin-dir <DIR>` | Use prebuilt binaries from `DIR` instead of `cargo build` (containers) | `target/release` + build |
| `--dry-run` | Print the resolved commands without executing anything | `false` |
| `--skip-fetch` | Skip ETL download (use existing cached Parquet) | `false` |
| `--skip-hpo` | Skip HPO (replay backtest with existing config) | `false` |
| `--no-inhibit` | Disable automatic `systemd-inhibit` wrapper | `false` |
| `-d, --detach` | Run in background via `nohup` | `false` |
| `-l, --log <PATH>` | Custom logfile path when detached | `pipeline_<sym>_<year>[_<MM>].log` |

Argument resolution is covered by `bash scripts/tests/pipeline_args_test.sh` (dry-run based, no network or build).

#### Ephemeral run on Google Cloud Batch
The same pipeline runs on a disposable Cloud Batch VM (`n2-highcpu-64` Spot by default; the project's CPU quota must cover it, e.g. `--machine-type n2-custom-30-30720` for 30 vCPU) via `scripts/gcp/run_ephemeral_batch_hpo.sh`, which builds the repository `Dockerfile`, submits the job, waits for it, prints its task logs and downloads the winning artifact into `configs/`. Market data never leaves the VM disk; only the winning JSON goes to GCS and metrics to BigQuery. See [GCP architecture §8, Step 2b](docs/architecture/10-gcp-cloud-architecture.md). `bash scripts/tests/batch_job_test.sh` checks the rendered job spec in dry-run mode.

---

### Step 1: Download & Convert Real Market Data (Binance ETL)
Download official Binance Futures USDT-M `aggTrades` archives directly from `data.binance.vision` and convert them into compressed Apache Parquet format:

```bash
# Download and convert an entire year automatically (historical year or Year-to-Date up to today):
cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2026 --output-dir data/historical_2026

# Download a specific month (e.g. January 2024)
cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2024 --month 1

# Download a single day (e.g. 2024-01-01) -> ~760k ticks in ~5MB Parquet
cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2024 --month 1 --day 1

# Download up to 8 archives concurrently (default: 4; --jobs 1 restores sequential mode with progress bars)
cargo run --release -p adapters --bin fetch_data -- --symbol BTCUSDT --year 2026 --jobs 8
```

Each archive (month or day) is independent and writes its own Parquet file, so concurrent downloads produce exactly the same dataset. A daily archive that Binance has not published yet (HTTP 404) is reported and skipped; any other failure is collected and makes the command exit non-zero after the remaining archives finish.

`fetch_data`, `run_hpo` and `backtest` print `[TIMING] phase=<name> seconds=<s>` lines (and the production pipeline prints `[TIMING] stage=<name> seconds=<s>`) so wall time per phase can be measured from logs.

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
- With `--gcp-bigquery`, records the backtest run (spec, metrics, trades, event-based equity) and the Monte Carlo child run (percentiles, ruin probability and 8 representative fan-chart curves) to BigQuery.

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
- **Automated Serialization**: Saves the winning Strategy Deployment Artifact to `configs/hpo_results.json` (`--gcs-upload` also uploads it to GCS as `artifacts/<config stem>-<suffix>.json`, where `--artifact-suffix` sets the suffix and the default is a UTC timestamp). `--strict-sinks` (also on `backtest`) makes any BigQuery/GCS failure exit non-zero after local work finishes. With `--gcp-bigquery`, records an `hpo` run plus one `hpo_trial` run per candidate with in-sample/out-of-sample metrics for every fold. `--execution-context <json>` sets the fee/slippage/capital schedule used by every trial.

---

### Step 5: Live Market Data Ingestion & Dollar Bar Streaming
Subscribe to the real-time Binance WebSocket feed and stream live ticks into the `DollarBarAggregator`:

```bash
# Stream from Binance Futures (default)
cargo run -p adapters --bin stream_trades -- --symbol btcusdt --threshold 50000

# Stream from Binance Spot (recommended for EU/regulated networks)
cargo run -p adapters --bin stream_trades -- --symbol btcusdt --threshold 50000 --spot
```
- Ingests real-time `aggTrades` using `BinanceWebSocketStream`.
- Enforces strict zero-float parsing via `rust_decimal::Decimal`.
- Handles automatic reconnection with exponential backoff and Ping/Pong heartbeats.
- Aggregates ticks on the fly and prints finalized Dollar Bars with exact OHLCV metrics.

---

### Step 6: Run Real-Time Paper Trading Engine
Connect the full trading engine to the live market stream and simulate execution with realistic friction without risking capital:

```bash
# Run paper trading on Binance Spot with $10,000 capital and 1% risk per trade
cargo run -p backtest --bin paper_trading -- --symbol btcusdt --spot --dollar-bar 50000 --capital 10000

# Run with an optimized HPO configuration file
cargo run -p backtest --bin paper_trading -- --symbol btcusdt --spot --config configs/hpo_results.json
```
- Evaluates symmetric CUSUM filter and rolling Z-Score signals in real time upon bar completion.
- Matches simulated orders with realistic maker/taker fees and slippage.
- Computes real-time *mark-to-market* unrealized PnL and drawdown tracking.
- Implements Triple Barrier exits (Stop Loss, Take Profit, Time Barrier).
- Records the session as a `paper` run in BigQuery (`--gcp-bigquery`): lifecycle rows, each closed trade with its entry/trough/peak/exit equity, and final metrics; sends instant push alerts to Telegram (`--telegram-alerts`).
- Graceful shutdown on `Ctrl+C`: liquidates open position at current mark price, compiles full quantitative performance attribution (Sortino, Win Rate, Profit Factor), and records the final `stopped` run row with its metrics when `--gcp-bigquery` is enabled.

---

### Step 7: Cloud-First BI Telemetry & Mobile Alerting

ATSNT runs headless and air-gapped without exposing inbound HTTP/WebSocket ports. Telemetry and metrics are streamed directly to Google Cloud BigQuery for Looker Studio visualization, and critical trade events are dispatched to Telegram:

```bash
# 1. Run paper trading with BigQuery streaming and Telegram alerts
cargo run -p backtest --bin paper_trading -- \
  --symbol btcusdt \
  --capital 10000 \
  --gcp-bigquery \
  --telegram-alerts

# 2. Run HPO and record the sweep and its trials to BigQuery
cargo run -p backtest --bin run_hpo -- \
  --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet \
  --candidates 60 \
  --gcp-bigquery

# 3. Run backtest with Monte Carlo and record both runs to BigQuery
cargo run -p backtest -- \
  --monte-carlo \
  --mc-iterations 10000 \
  --gcp-bigquery
```
- **Looker Studio**: Reads the run-centric views in BigQuery dataset `atsnt_bi` (`v_runs`, `v_run_metrics`, `v_trades`, `v_equity`, `v_latest_equity`); see `docs/architecture/11-looker-studio-bi-spec.md`.
- **Telegram Bot Webhook**: Dispatches instant push notifications (<200ms) for position entries, exits, stop loss triggers, and circuit breakers.
- **Fail-Open / Offline Safe**: If GCP/Telegram environment variables (`GCP_PROJECT_ID`, `TELEGRAM_BOT_TOKEN`, etc.) are omitted, sinks safely log information and continue without interruption.

---

### Step 8: Live Execution Gateway (Binance Testnet & Production)
Connect to Binance Testnet or live production with cryptographic HMAC-SHA256 signing and institutional pre-trade risk controls.

#### Credential Setup (.env)
Copy the template and configure your API keys (never commit `.env` to Git):

```bash
cp .env.example .env
# Edit .env with your keys:
nano .env
# Load the credentials into your current shell session:
set -a; source .env; set +a
```

#### Running Gateway Queries & Streams

```bash
# 1. Query available account balance on Binance Futures Testnet
cargo run -p adapters --bin live_gateway -- \
  --testnet \
  --futures \
  --check-balance \
  --asset USDT

# 2. Query balance on Binance Spot Testnet
cargo run -p adapters --bin live_gateway -- \
  --testnet \
  --check-balance \
  --asset USDT

# 3. Listen for real-time order fill updates over the User Data Stream
cargo run -p adapters --bin live_gateway -- \
  --testnet \
  --futures \
  --listen

# Alternatively, pass credentials explicitly via CLI flags if not using environment variables:
cargo run -p adapters --bin live_gateway -- \
  --api-key "YOUR_KEY" \
  --secret-key "YOUR_SECRET" \
  --testnet \
  --futures \
  --check-balance
```

#### Pre-Trade Risk Policy & Quantitative Safety Guarantees
- **Exact Slippage Accounting**: In 1:1 realistic simulation, slippage is implicitly absorbed into the fill and exit prices (`pnl_gross`). Cash equity satisfies the exact mathematical identity $\text{Cash} = \text{Initial Capital} + \sum \text{Net PnL}$ without double deductions.
- **Circuit Breaker with Daily UTC Reset**: Trading automatically halts if daily account drawdown breaches `max_daily_drawdown_pct` (default: 5% in live, configurable). The watermark and freeze reset automatically at `00:00:00 UTC`, supporting uninterrupted multi-month backtesting.
- **Microstructure Friction Filter**: Strategies enforce `min_take_profit_pct` (default: 25-35 bps), rejecting trades where price displacement cannot clear round-trip exchange fees and latency slippage.
- **Order Size Ceiling**: Single order notional value cannot exceed `max_order_notional` (default: $10,000 USDT).
- **Position Cap**: Aggregate position exposure cannot exceed `max_position_notional` (default: $50,000 USDT).
- **Zero-Float Math**: 100% fixed-point decimal arithmetic via `rust_decimal::Decimal`.
- **Binance Testnet Setup**:
  1. Visit [testnet.binance.vision](https://testnet.binance.vision/) and sign in with GitHub to generate free API/Secret keys.
  2. Test execution, risk limits, and websocket fills with 0 € risk to real capital.
  3. When transitioning to real capital on your VPS, bind your API key to your VPS static public IP and disable withdrawal permissions.

---

## 4. Documentation Index

- [`AGENTS.md`](AGENTS.md): Architectural standards, engineering guidelines, and agent rules.
- [`docs/ROADMAP.md`](docs/ROADMAP.md): Milestone progress and upcoming deliverables.
- [`docs/strategy/01-dollar-bars-cusum.md`](docs/strategy/01-dollar-bars-cusum.md): Mathematics of Dollar Bars, symmetric CUSUM filter, rolling Z-Scores, and Triple Barrier exits.
- [`docs/strategy/02-order-flow-imbalance.md`](docs/strategy/02-order-flow-imbalance.md): Order Flow Imbalance, toxic momentum breakouts, liquidity absorption reversals, and friction filters.
- [`docs/architecture/02-domain-models-and-lifecycle.md`](docs/architecture/02-domain-models-and-lifecycle.md): Domain models and 3-stage execution lifecycle (`OrderIntent` -> `Order` FSM -> `Position`).
- [`docs/architecture/03-adapters-and-market-data.md`](docs/architecture/03-adapters-and-market-data.md): Ports and Adapters, Binance `aggTrades` mapping, and WebSocket streaming.
- [`docs/architecture/04-hpo-and-walk-forward.md`](docs/architecture/04-hpo-and-walk-forward.md): Walk-Forward Optimization, parameter stability scoring, DSR, and Rayon parallelism.
- [`docs/architecture/05-monte-carlo-and-telemetry.md`](docs/architecture/05-monte-carlo-and-telemetry.md): Discrete Event Monte Carlo Stress-Testing, trade sequence bootstrap, and web telemetry schemas.
- [`docs/architecture/06-paper-trading-and-realtime-execution.md`](docs/architecture/06-paper-trading-and-realtime-execution.md): Real-time Paper Trading architecture, event broadcasting, and mark-to-market telemetry.
- [`docs/architecture/07-web-telemetry-dashboard.md`](docs/architecture/07-web-telemetry-dashboard.md): Cloud-First BI telemetry, BigQuery schemas, GCS Parquet export, and Telegram mobile alerting (supersedes web presentation).
- [`docs/architecture/08-live-execution-gateway.md`](docs/architecture/08-live-execution-gateway.md): Live Execution Gateway, HMAC-SHA256 authentication, Pre-Trade Risk Manager, and User Data Stream reconciliation.

---

## 5. License
Dual-licensed under MIT or Apache-2.0.
