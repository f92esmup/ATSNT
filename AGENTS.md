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
     - `MarketDataStream`: Trait for receiving ticks, bars, or depth updates.
     - `ExecutionGateway`: Trait for dispatching and canceling orders.
     - `Clock`: Time abstraction to allow time-travel in backtests.

3. **Adapters (`adapters`)**:
   - Concrete implementations of ports:
     - Live exchange adapter (e.g., Binance WebSocket + REST).
     - Deterministic backtest simulator adapter (feeding historical data).
     - Storage and persistence adapters.

4. **Strategies (`strategy`)**:
   - Strategy traits that consume normalized market events and emit trading signals or order intents.

---

## Technical Standards & Rust Best Practices

### Financial Arithmetic & Correctness
- **NEVER use `f32` or `f64`** for asset quantities, order prices, or account balances. Always use fixed-point decimal math (via `rust_decimal::Decimal`). Floating-point inaccuracies cause catastrophic accounting and execution failures.
- Enforce explicit state machines for order lifecycles (`PendingNew` -> `New` -> `PartiallyFilled` -> `Filled` / `Canceled` / `Rejected`). Invalid state transitions must be compile-time or runtime errors.

### Error Handling
- Never use `unwrap()` or `expect()` in production or domain code.
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
