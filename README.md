# ATSNT - Algorithmic Trading System Native in Rust

A high-performance, modular algorithmic trading engine built 100% in Rust from first principles. Focuses on system architecture, event-driven concurrency, deterministic backtesting, anti-overfitting optimization, and financial correctness.

---

## 1. Architectural Philosophy

- **Zero Black-Box Frameworks**: We deliberately avoid monolithic trading frameworks (e.g. NautilusTrader). We design and own domain models, state machines, indicators, risk managers, and execution pipelines from first principles.
- **Hexagonal Architecture (Ports & Adapters)**: Strict separation of concerns. Pure business logic has zero I/O or network dependencies; infrastructure adapters plug in via abstract traits.
- **Financial Correctness (Zero-Float Rule)**: Never use `f32` or `f64` for quantities, prices, or account balances. All financial calculations use fixed-point arithmetic via `rust_decimal::Decimal`.
- **Zero-Toy Assumptions**: Every backtest models realistic fee schedules (maker/taker), a fixed per-fill slippage penalty, and Triple Barrier exits. Remaining simulation gaps versus live are tracked in the [ROADMAP](docs/ROADMAP.md#simulation-fidelity-simulated-vs-live-parity).
- **Anti-Overfitting Validation**: Parameter optimization enforces Walk-Forward temporal splits with Embargo periods (AFML Ch. 7), Parameter Stability Plateau scoring, Deflated Sharpe Ratio (DSR), and Discrete Event Monte Carlo trade sequence bootstrapping.

---

## 2. Workspace Structure

```text
ATSNT/
├── crates/
│   ├── domain/       # Pure business logic (DollarBar, Aggregator, CusumFilter, Z-Score, Order FSM, Position)
│   ├── ports/        # Hexagonal contracts (MarketDataStream, AlertNotifier, ExitGateway); depends only on `domain`
│   ├── strategies/   # Strategy traits & implementations (DollarBarsCusumStrategy, OrderFlowImbalanceStrategy)
│   ├── adapters/     # Infrastructure adapters implementing the ports (Binance CSV, Parquet columnar, Binance ETL fetcher, Binance WebSocket Stream, GCP BigQuery/GCS/Telegram)
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

#### Recommended intraday HPO run (full year, 30-vCPU Spot VM)

A dollar bar closes when its traded notional reaches the threshold, so the threshold fixes how long a bar lasts and therefore what the strategy's bar-count parameters mean in clock time. Averages measured on BTCUSDT (September 2026, about $344B traded, i.e. 6.87 M bars at $50k); real bar duration varies with market activity:

| `--dollar-bar` | Avg. bar | CUSUM window (14-40 bars) | Max. holding (fixed 100 bars) | Style |
|---|---|---|---|---|
| `50000` | ~0.4 s | 6-16 s | ~40 s | noise, ~7 M bars/month, days of compute |
| `1000000` | ~7.5 s | 2-5 min | ~12 min | scalping |
| **`10000000`** | **~75 s** | **17-50 min** | **~2 h** | **intraday / day trading (recommended)** |
| `25000000` | ~3 min | 42-120 min | ~5 h | slower day trading |

The default CUSUM search space has 1372 combinations (7 x 7 x 7 x 4 x 1; the time barrier is fixed at 100 bars, about 2 h at `--dollar-bar 10000000`, so trades close by take profit or stop loss rather than by the clock); `--candidates 120` evaluates a seeded random sample of 120 of them (about 9 %, and a multiple of the 30 vCPU, so Rayon gives each thread four candidates), which is enough to decide whether the strategy produces trades at that scale. Search time grows linearly with bars x candidates x months, while fetching and VM start-up are fixed costs.

```bash
./scripts/gcp/run_ephemeral_batch_hpo.sh \
    --project "$GCP_PROJECT_ID" --region europe-southwest1 \
    --machine-type n2-custom-30-30720 \
    --year 2026 \
    -- --dollar-bar 10000000 --candidates 120 --capital 200
```

- `--year 2026` without `--month` fetches every closed month plus the current month day by day, and one job optimizes over all of them.
- Measured on 2026-10-15 (2026-01-01 to 2026-10-14, 447.8 M ticks, `n2-custom-30-30720` Spot): fetch 302 s, ingest 77 s (333,517 bars), search 206 s (120 candidates x 5 folds), backtest 92 s; the job ran 10.4 minutes from `RUNNING` to `SUCCEEDED`. Scale from these numbers: search time is linear in bars x candidates, fetch in months.
- The infrastructure path is validated end to end, the strategy is not: that run's best candidate lost the whole 200 USDT capital (1,145 trades, 16.85% win rate, Sortino -346) and failed DSR (p = 1.0). Do not deploy a winner without positive out-of-sample Sortino and a passing DSR.
- The winner is uploaded create-only to `gs://<bucket>/artifacts/hpo_btcusdt_2026-<job>.json` and downloaded to `configs/`; trials, the HPO run and the backtest go to BigQuery (`v_runs`, `v_trades`, `v_equity`).
- Check `DSR p` and the closed-trade count in the log before deploying the winner: a configuration with 0 trades or a failed DSR is not tradeable.
- Spot preemption restarts the task from scratch (`--max-retries 2`); add `--standard` for multi-hour runs you cannot afford to repeat, and `--max-run-seconds` above the default 14400 for anything longer than four hours.
- The project CPU quota (`CPUS_ALL_REGIONS` and the family quota) must cover the machine; the script aborts before submitting when it does not.

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

### Step 2b: Validate a Finalist at Tick Level
The default `--fill-model bar` fills at bar granularity and is what the optimizer uses. To validate a finalist, replay every trade through the same trading session that runs live, on a simulated venue:

```bash
cargo run --release -p backtest -- --data data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01-01.parquet --config configs/hpo_results.json --fill-model tick --latency-ms 100
```
- Orders reach the market after `--latency-ms` (an assumption until measured against live fills).
- Market orders walk the tape: they consume only same-side aggressor prints after arrival and pay each print's price.
- Stop-loss and take-profit are conditional market orders armed by the tape (last trade price stands in for the mark price, since `aggTrades` carries none).
- Perpetual funding is charged at every 00:00/08:00/16:00 UTC settlement to the position held then (`--funding-rate`, default 0.01%, or recorded rates from `--funding-file`, the JSON that `GET /fapi/v1/fundingRate` returns), and it is part of the trade's net PnL.
- Isolated margin is modelled: the venue picks the lowest leverage the balance supports (as the live gateway does), and a position that reaches its liquidation price is closed at the bankruptcy price (exit reason `Liquidation`), losing exactly its margin. Maintenance margin uses one flat 0.4% rate instead of the exchange's notional brackets.
- Replaying tens of millions of ticks is slow, so use it on finalists, not inside the search. `--gcp-bigquery` records the run with `fill_model = tick`, so tick-level results are never mixed with bar-level ones.

---

### Step 2c: Calibrate the Simulator Against Live Fills
`--latency-ms` is an assumption until it is measured. Record the entry fills of a live run and compare them with the simulation of the same orders on the tape of that day:

```bash
# 1. Live (or testnet) run that records every entry fill
cargo run -p backtest --bin live_trading -- --config configs/hpo_results.json --record-fills fills.jsonl
# 2. Next day, with that day's aggTrades archive (see Step 1 for the data fetcher)
cargo run --release -p backtest --bin parity_check -- --tape data/BTCUSDT-aggTrades-2026-10-17.parquet --fills fills.jsonl
```
- Replays each recorded market order on the tape with a sweep of order latencies and prints the mean price error and bias of each; the best fit is the `--latency-ms` to use for `--fill-model tick` and paper trading.
- A positive bias means the simulation is pessimistic (it paid more than live did), a negative one optimistic. The tape must be the market the run traded (production archive for production fills; testnet prices differ).
- The send time is the local wall clock and the fill time the exchange's, so clock skew shifts the reported time error; the price error drives the recommendation. Few fills give a noisy estimate: use at least a few dozen.

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
- Evaluates symmetric CUSUM filter and rolling Z-Score signals in real time upon bar completion. `--warmup-minutes N` first feeds the strategy N minutes of recent trades (public REST `aggTrades`, futures only) so its rolling windows are warm instead of cold, as in a backtest. USD-M futures paper trading follows the production market feed.
- Runs the same `TradingSession` as live trading on a tick-level simulated venue: orders reach the market after `--latency-ms` (default 100, an assumption until measured), market orders walk the tape of real trades and pay the taker fee, and stop-loss/take-profit are conditional market orders armed by the tape.
- Computes real-time *mark-to-market* unrealized PnL and drawdown tracking.
- Implements Triple Barrier exits (Stop Loss, Take Profit, Time Barrier).
- Records the session as a `paper` run in BigQuery (`--gcp-bigquery`): lifecycle rows, each closed trade with its entry/trough/peak/exit equity, and final metrics; sends instant push alerts to Telegram (`--telegram-alerts`).
- Graceful shutdown on `Ctrl+C`: an open position is not liquidated (like an exchange-held position it stays open and is not part of the metrics); compiles full quantitative performance attribution (Sortino, Win Rate, Profit Factor), and records the final `stopped` run row with its metrics when `--gcp-bigquery` is enabled.

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
- **Monthly Kill-Switch**: If equity loses `max_monthly_drawdown_pct` (default: 15%) versus the equity at the start of the UTC month, the simulator closes any open position at market (reason `MonthlyCircuitBreaker`) and halts for the rest of the run; metrics report `halted: true`. Paper trading emits `CircuitBreakerTripped { scope: "monthly", drawdown_pct }`. The live loop cancels all orders, closes the position at market (`reduceOnly`), confirms it is flat before reporting `Halted`, blocks new orders and alerts via Telegram; its equity is the wallet balance plus the open position's unrealized PnL at the last tick. `halted` is part of the metrics JSON and the nullable BigQuery column `run_metrics.halted` (TRUE only for halted runs).
- **Binance Execution Parity**: Sizing caps the position at `max_leverage` (default: 20x, a self-imposed risk cap), floors the quantity to `quantity_step_size` (default: 0.001) and rejects orders below `min_order_quantity` or `min_order_notional` (`OrderBelowMinLotSize` / `OrderBelowMinNotional`). The live gateway overrides these with the symbol's real `LOT_SIZE`/`MIN_NOTIONAL` filters (Spot is sized at 1x). Configs from older artifacts load with these defaults.
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
- [`docs/architecture/12-adding-a-strategy.md`](docs/architecture/12-adding-a-strategy.md): How to add a strategy (one registry line) and what the generic contract suite checks.
- [`docs/architecture/06-paper-trading-and-realtime-execution.md`](docs/architecture/06-paper-trading-and-realtime-execution.md): Real-time Paper Trading architecture, event broadcasting, and mark-to-market telemetry.
- [`docs/architecture/07-web-telemetry-dashboard.md`](docs/architecture/07-web-telemetry-dashboard.md): Cloud-First BI telemetry, BigQuery schemas, GCS Parquet export, and Telegram mobile alerting (supersedes web presentation).
- [`docs/architecture/08-live-execution-gateway.md`](docs/architecture/08-live-execution-gateway.md): Live Execution Gateway, HMAC-SHA256 authentication, Pre-Trade Risk Manager, and User Data Stream reconciliation.

---

## 5. License
Dual-licensed under MIT or Apache-2.0.
