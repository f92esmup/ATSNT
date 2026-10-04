# ATSNT - Algorithmic Trading System Native in Rust

A modular, high-reliability algorithmic trading engine built 100% in Rust, focusing on system architecture, event-driven concurrency, and quantitative finance principles.

## Core Philosophy
- **Zero Black-Box Frameworks**: We build our domain model, state machines, and execution logic from first principles without monolithic wrappers.
- **Hexagonal Architecture**: Strict separation of concerns between pure domain logic, abstract ports, and infrastructure adapters.
- **Realistic 1:1 Simulation**: Zero-toy assumptions. Every backtest models realistic fee schedules, spreads, slippage, and queue priority.

## Documentation
- [`AGENTS.md`](AGENTS.md): Architectural standards, engineering guidelines, and agent rules.
- [`docs/strategy/01-dollar-bars-cusum.md`](docs/strategy/01-dollar-bars-cusum.md): Complete strategy specification covering Dollar Bars, CUSUM filtering, dynamic Z-Scores, the Triple Barrier Method, position sizing, and backtesting metrics.
- [`docs/architecture/02-domain-models-and-lifecycle.md`](docs/architecture/02-domain-models-and-lifecycle.md): Domain architecture, mathematical foundations, and the 3-stage execution lifecycle (OrderIntent -> Order FSM -> Position).
- [`docs/architecture/03-adapters-and-market-data.md`](docs/architecture/03-adapters-and-market-data.md): Market data adapters, Binance aggTrades schema mapping, and the three operational modes (Backtest, Paper, Live).
- [`docs/architecture/04-hpo-and-walk-forward.md`](docs/architecture/04-hpo-and-walk-forward.md): High-performance HPO, Walk-Forward Optimization, parameter stability plateau scoring, and Rayon parallelization.
- [`docs/ROADMAP.md`](docs/ROADMAP.md): Project roadmap milestones from backtesting to paper trading, web dashboard, and live execution.
