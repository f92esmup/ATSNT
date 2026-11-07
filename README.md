# ATSNT: Algorithmic Trading System, Native in Rust

ATSNT is a local research engine written in Rust. It backtests and optimizes one strategy, the
swarm consensus, on tick data. The plan is to trade that strategy on Kraken spot in EUR, long
only and without leverage, once its economics pass the gates in
[`docs/plan`](docs/plan/refactor-local-kraken.md).

## Status

- **Stage:** the refactor is complete: batch 1 (the swarm-only refactor) and batch 2 (gate A,
  cleanup and the SQLite run store) are done. The only registered strategy is `swarm_consensus`,
  under `usdm_perpetual/`. The cleanup branch removes the Binance execution path and its binaries
  (L3N) and is merged into `main`. A Kraken client from an earlier branch, `feat/kraken-eur-spot-portfolio`,
  is archived as the tag `archive/kraken-eur-spot-portfolio`; it does not merge with this refactor.
  See [`HANDOFF.md`](HANDOFF.md).
- **Gates:** G0, the plan, is approved. GA, the economics screen on a perpetual BTCUSDT proxy, did
  not pass on 2026-10-31 (`docs/plan/resultado-prueba-a.md`). Plan v4 therefore stops the Kraken
  route: GB (Kraken data, twelve months, walk-forward and DSR) and GP (paper trading) do not start.
- **Not built:** any exchange adapter. The Binance execution path was removed in L3N, so no binary
  sends orders. The Kraken route is stopped (see above).

## Architecture

| Crate | Role |
|---|---|
| `domain` | Pure rules: orders, positions, risk, fees, accounting and metrics. No I/O and no floats. |
| `ports` | The traits and plain data the core uses: market data, execution, clock, run recording and alerts. |
| `strategies` | The swarm consensus strategy and the registry that names it. |
| `adapters` | Concrete ports: the Parquet tape store, the SQLite run store, the simulated venue, Telegram and the Binance downloader (until WP-09). |
| `backtest` | Simulators, the optimizer and the binaries. The library names no adapter: `main.rs` and `wiring.rs` compose them. |

The architecture rules are tests in `crates/backtest/tests/architecture_contract.rs`:

- the core crates (`domain`, `ports`, `strategies`) depend on no I/O crate and do no I/O;
- `adapters` depends only on `domain` and `ports`;
- the library names no adapter outside the composition, and reaches the wiring only through its
  entries;
- `ports` name no exchange and no storage format;
- `domain` uses no `f32` or `f64`;
- only `adapters` and the composition read the wall clock;
- runners dispatch through the strategy registry.

`AGENTS.md` forbids `unwrap()` and `expect()` outside tests. Every crate root and binary root
denies `clippy::unwrap_used` and `clippy::expect_used` when it is not built for tests.

## Quick start

You need a recent stable Rust toolchain and a C compiler (SQLite is bundled).

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Golden tests pin the swarm's trades and its recorded rows. After an intended behaviour change,
rewrite them and review the diff:

```bash
UPDATE_GOLDEN=1 cargo test -p backtest --test golden_swarm
```

## Binaries

| Binary | Crate | What it does |
|---|---|---|
| `fetch_data` | `adapters` | Downloads Binance USD-M `aggTrades` archives and converts them to Parquet. Used for the economics screen; Kraken history replaces it (WP-09). |
| `backtest` | `backtest` | Replays a tape through the swarm, with bar or tick fills, optional Monte Carlo and optional run recording. |
| `run_hpo` | `backtest` | Walk-forward optimization of the swarm, with embargo and the Deflated Sharpe Ratio. |
| `calibrate_feeds` | `backtest` | Calibrates the swarm's feed thresholds on a period before the evaluation. |
| `prune_swarm` | `backtest` | Prunes candidate agents by their information coefficient. |

Every binary takes `--help`. The backtester takes a single Parquet file or a directory of them.

```bash
cargo run --release -p backtest --bin backtest -- \
  --data data/historical/BTCUSDT --config configs/hpo_btcusdt_2026.json --capital 5000
```

## The economics screen (gate A)

`scripts/gate_a.sh` runs the screen described in
[`docs/plan/refactor-local-kraken.md`](docs/plan/refactor-local-kraken.md), Fase 1. It simulates
spot costs on the perpetual proxy, long only, over February to September 2026.

```bash
scripts/gate_a.sh --months "02"   # preflight: one month, to measure the trade rate
scripts/gate_a.sh                 # the full window
```

The monthly archives must be under `data/historical/BTCUSDT/`, downloaded with `fetch_data`. The
script writes its logs and `summary.txt` under `~/atsnt-gate/`. `--dry-run` prints what it would do
without running anything.

## Recording runs

`backtest` and `run_hpo` take `--record <PATH>`. They write each run's spec, metrics, trades and
equity to a SQLite store, creating the file if needed. Without the flag nothing is written. The
tables are described in [`docs/architecture/13-run-registry.md`](docs/architecture/13-run-registry.md).

## Documentation

- [`HANDOFF.md`](HANDOFF.md): current state, next steps, decisions and rules for the next session. Read it first.
- [`docs/plan/refactor-local-kraken.md`](docs/plan/refactor-local-kraken.md): the strategy and
  its decisions (v4).
- [`docs/plan/implementacion-kraken-spot.md`](docs/plan/implementacion-kraken-spot.md): the work
  packages and their gates.
- [`docs/ROADMAP.md`](docs/ROADMAP.md): status against the gates.
- [`docs/strategy/03-swarm-consensus.md`](docs/strategy/03-swarm-consensus.md) and
  [`docs/archive/research/01-swarm-consensus-optimization-logbook.md`](docs/archive/research/01-swarm-consensus-optimization-logbook.md):
  the strategy and its archived logbook.
- [`docs/architecture`](docs/architecture): the domain, the adapters, the optimizer, Monte Carlo,
  the run registry and how to add a strategy. The paper and live gateway designs are in the archive.
- [`docs/capital-scaling.md`](docs/capital-scaling.md): sizing and caps for the spot account.
- [`docs/archive`](docs/archive/README.md): history of the removed strategies, the cloud tooling and the Binance gateway.
- [`AGENTS.md`](AGENTS.md): engineering rules.

## License

Dual-licensed under MIT or Apache-2.0.
