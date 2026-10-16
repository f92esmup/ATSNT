# sim-live-fidelity
Objective: 1:1 fidelity between the simulated and live environments, and strategy-agnostic hexagonal wiring.
Branch: feat/sim-live-fidelity | Delivery: ask-on-risk | Test runner: `cargo test --workspace` (+ `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`)

## Specs
- S1: Live sizing and risk limits come from the strategy deployment artifact, not from `RiskPolicy::default()` or a hardcoded 1% risk per trade. An explicit `--risk-pct` still overrides the artifact.
- S2: The live daily drawdown breaker uses the same semantics as the simulator: peak equity per UTC day, reset at 00:00:00 UTC.
- S3: Live PnL reports the real commissions charged by the exchange instead of an estimated taker fee.
- S4: `docs/ROADMAP.md` must not claim features that do not exist (M4 "Real-time order matching against live bid/ask spreads").
- S5 (decision, user): entries are `market` orders (taker), "si quiero que sea market".
- S6 (later phases): venue ports (`ExecutionVenue`, `Clock`, `MarketData`) with `SimulatedVenue` and `BinanceVenue`; tick-level realism (latency, tape-based market slippage, taker TP, mark-price stop); funding and liquidation; parity harness with live warm-up; single `StrategyRegistry` plus a generic strategy contract suite.

## Tasks
- [ ] T1 (S1) P0 — risk policy and risk-per-trade from the artifact in live | inline | commit: -
- [ ] T2 (S2) P0 — UTC-daily peak watermark in live | inline | commit: -
- [ ] T3 (S3) P0 — real commissions in live PnL | inline | commit: -
- [ ] T4 (S4) P0 — ROADMAP correction | inline | commit: -
- [ ] T5 (S5, S6) P1 — ports and shared `TradingSession` | pending, needs its own plan
- [ ] T6 (S5, S6) P2 — market entries, latency, tape slippage, taker TP, mark-price stop | pending
- [ ] T7 (S6) P3 — funding and liquidation | pending
- [ ] T8 (S6) P4 — warm-up and parity harness | pending
- [ ] T9 (S6) P5 — `StrategyRegistry` and contract suite | pending, can start after T5

## Log
- L1 (2026-10-16): "Quiero que hagas una investigación exhaustiva sobre la fidelidad entre el netorno simulado y live. Nosotros buscamos una similitud 1:1 para que las estrategias que evaluamos no arrojen datos sesgados. Se ha conseguido? hay cosas que ajustar? Lo segundo a auditar es que el proyecto este aplicando bien la generalización y esquema hexagonal, que todos los modulos sean estandarizados para qeu cuando cree una nueva estrategia. esta no tenga fricción."
- L2 (2026-10-16): "me parece una idea genial lo de las venues. Voy a seguir tus recomendaciones. si quiero que sea market. Adelante con la refactorización. vamos paso a paso"
- L3: Audit evidence: live gateway sizes with `RiskPolicy::default()` (live_trading.rs:322) and `Decimal::new(1, 2)` (binance_gateway.rs:1213); the artifact `risk_pct`/`execution_context` only reach run metadata. Live passes a whole-session peak to the breaker while the engine resets daily (engine.rs:189-197).
