# Engineering Guidelines & Agent Rules

## Project Vision & Philosophy
This repository implements a modular, high-reliability algorithmic trading engine built 100% in Rust.

- **Objective**: Educational and architectural mastery of system design, asynchronous concurrency, deterministic backtesting, and algorithmic execution.
- **Architectural Scope**: We deliberately avoid monolithic black-box trading frameworks (e.g., NautilusTrader). Instead, we build and own the domain model, order state machines, risk management, and event-driven engine from first principles.
- **Dependency Policy**:
  - Foundational infrastructure crates are encouraged for low-level mechanics: async runtime (`tokio`), serialization (`serde`), network protocols (`reqwest`, `tokio-tungstenite`), decimal arithmetic (`rust_decimal`), and ML inference (`ort` or native Rust ML crates like `burn` / `linfa`).
  - Monolithic end-to-end frameworks that conceal core trading mechanics are rejected.

---

## Architectural Principles (Hexagonal / Ports & Adapters)

The project enforces strict separation of concerns:

1. **Domain (`domain` / `core`)**:
   - Pure business logic: `Order`, `Trade`, `Position`, `Portfolio`, `RiskPolicy`, `OrderBook`.
   - Zero I/O, zero network dependencies, zero broker-specific logic.
   - Must be 100% deterministic and unit-testable.

2. **Ports (`ports`)**:
   - Abstract traits defining contracts for external interactions:
     - `MarketDataStream`: Trait for receiving market trades.
     - `ExecutionVenue` and `ExitGateway`: Traits for dispatching orders of an exact quantity and
       managing exchange-held exits. A venue executes; it sizes nothing and applies no risk rule.
     - `TapeVenue`: A venue replayed trade by trade, for simulations.
     - `Entropy`: Randomness, so a replay is deterministic.
     - `RunRecorder`, `AlertNotifier`, `FillLog`, `ProgressSink`: Outbound records and reports.

3. **Strategies (`strategies`)**:
   - Strategy traits that consume normalized market events and emit order intents.

4. **Application (`application`)**:
   - The session core that runs on any venue: the trading session and its driver, the exit
     lifecycle, the execution core, the tick simulation and the parity measure.
   - Depends only on `domain`, `ports` and `strategies`. It names no adapter and opens no file.

5. **Adapters (`adapters`)**:
   - Concrete implementations of ports:
     - Live exchange adapter (e.g., Kraken WebSocket + REST).
     - Deterministic backtest simulator adapter (feeding historical data).
     - Storage and persistence adapters.

6. **Composition (`backtest`)**:
   - The bar engine, the optimizer, Monte Carlo and the binaries. `wiring.rs`, `main.rs` and
     `bin/` build the adapters and pass them down as ports.

### Enforced Rules

`crates/backtest/tests/architecture_contract.rs` checks these rules. The checks match source text, so they catch the usual violations, not every alias. A rule that blocks a change is discussed, never disabled silently.

- **R1**: `domain` depends on no other workspace crate; `ports` and `strategies` depend only on `domain`.
- **R2**: `adapters` depends only on `domain` and `ports`; `application` depends only on `domain`, `ports` and `strategies`.
- **R3**: the `backtest` library names no `adapters::` outside the wiring module, the binaries and `main.rs`.
- **R4**: `ports` names no exchange or storage format (`Binance`, `Kraken`, `Parquet`, `BigQuery`).
- **R5**: `domain` uses no `f32` or `f64`.
- **R6**: `backtest` runners select strategies through the registry and never match on strategy names.
- **R7**: `domain`, `ports`, `strategies`, `application` and the `backtest` library never read the wall clock; the binaries and `main.rs` may.
- **R8**: `application` names no adapter and performs no file or network I/O.
- **R9**: no library uses `anyhow`; the binaries and `main.rs` may.
- **R10**: `domain` names no strategy.

Sizing and the pre-trade risk rules live in `domain::EntryGate`, which every runner asks. Review, not
a test, keeps them out of adapters.

---

## Technical Standards & Rust Best Practices

### Financial Arithmetic & Correctness
- **NEVER use `f32` or `f64`** for asset quantities, order prices, or account balances. Always use fixed-point decimal math (via `rust_decimal::Decimal`). Floating-point inaccuracies cause catastrophic accounting and execution failures.
- Enforce explicit state machines for order lifecycles (`PendingNew` -> `New` -> `PartiallyFilled` -> `Filled` / `Canceled` / `Rejected`). Invalid state transitions must be compile-time or runtime errors.

### Error Handling
- Never use `unwrap()` or `expect()` in production or domain code. Every crate root and binary root denies `clippy::unwrap_used` and `clippy::expect_used` outside `cfg(test)`, so `cargo clippy` fails on them.
- Define domain-specific error enums using `thiserror`.
- Reserve `anyhow` strictly for application entrypoints (`main.rs`) or top-level integration harnesses.

### Asynchronous Concurrency & Latency
- Use `tokio` as the default async runtime.
- Favor message-passing architecture using bounded channels (`tokio::sync::mpsc::channel`) to maintain explicit backpressure control.
- Avoid locking mutexes across `await` points.
- Isolate CPU-bound tasks (e.g., ML inference or heavy number crunching) using `tokio::task::spawn_blocking` or dedicated worker threads.

### Code Quality & Formatting
- All code must pass `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.
- Document public structs, traits, and non-obvious algorithms with idiomatic doc comments (`///`).
- Keep code clean, modular, and adhere to SOLID principles.

---

## Testing & Verification

### Realistic Simulation Fidelity (Zero-Toy-Assumption Principle)
- **1:1 Behavioral Parity**: Backtesting simulators and paper-trading engines must mirror live exchange mechanics with zero toy simplifications or idealistic assumptions.
- **Friction & Latency**: Mandatory modeling of fee schedules (maker/taker fees, funding rates for perpetual futures), bid/ask spreads, and latency-induced slippage.
- **Realistic Fill Mechanics**: Orders must never execute against naive bar closes or mid-prices. Limit orders must honor queue priority and require trade prints through the level. Market orders must walk the simulated book or pay realistic slippage penalties.
- **Deterministic & Replayable**: The simulation engine must run purely on discrete event streams with deterministic clock abstractions.

### General Testing Rules
- Unit test all domain rules, state machines, and risk checks using deterministic synthetic test fixtures.
- Mock network and exchange adapters in tests; never make external network calls during unit or integration test suites.
- Code changes must pass all test suites before merge.
