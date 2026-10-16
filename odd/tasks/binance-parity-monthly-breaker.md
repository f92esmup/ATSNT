# Binance Execution Parity, Monthly Circuit Breaker and HPO Granularity

Objective: remove non-executable sizing assumptions from the simulator and add a monthly kill-switch.
Branch: `feat/binance-parity-monthly-breaker` (from `main`) | Delivery: single branch, work-unit commits | Test runner: `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`.

## Specs

- **S1 — LOT_SIZE / MIN_NOTIONAL / leverage in sizing.** User: "Evitar posiciones teóricas microscópicas cuando la cuenta se degrada (ej. 0.000000134 BTC)". `RiskPolicy::size_order` computes risk quantity, caps it to `equity * max_leverage / price`, floors it to `quantity_step_size`, then rejects with `RiskError::OrderBelowMinLotSize` (qty < min_order_quantity) or `RiskError::OrderBelowMinNotional` (qty*price < min_order_notional). Defaults: max_leverage 20, min qty 0.001, step 0.001, min notional 5.0 (assumption: real BTCUSDT minimum may differ; live preflight keeps using exchangeInfo).
- **S2 — Config surface, backward compatible.** `BacktestConfig` gains `max_monthly_drawdown_pct` (0.15), `max_leverage` (20), `min_order_quantity` (0.001), `quantity_step_size` (0.001), `min_order_notional` (5.0). All `#[serde(default)]` so existing artifacts still load. The engine stops hardcoding `RiskPolicy::default()` limits for these.
- **S3 — Daily breaker unchanged.** User: "Al llegar a las 00:00:00 UTC, el disyuntor diario se reinicia automáticamente". Already implemented (`engine.rs`); behavior preserved and pinned by a test. Stale "no daily reset" doc comments corrected.
- **S4 — Monthly kill-switch.** User: "Cierra de inmediato cualquier posición abierta a mercado" and "En simulación/backtest: detiene la ejecución por completo". Drawdown is measured from the in-month peak equity (assumption, mirrors daily). On trip: open position closed at market with reason `MonthlyCircuitBreaker`; engine becomes halted and ignores all later bars. Resets are NOT applied after a trip within the simulation.
- **S5 — Paper/live propagation.** `PaperTradingEvent::CircuitBreakerTripped { scope, drawdown_pct }` emitted for monthly trips; paper runner reflects halted state.
- **S6 — HPO space.** User: "Fijar la barrera temporal a un valor constante amplio (100 barras)" with denser `rolling_window_lens [14,18,22,26,30,35,40]`, `cusum_vol_multipliers [0.8,1.0,1.2,1.5,1.8,2.0,2.4]`, `z_entry_thresholds [1.5,1.7,1.9,2.1,2.3,2.6,3.0]`, `z_stop_thresholds [2.5,3.0,3.5,4.0]`; `configs/param_space_daytrading_cusum.json` updated. `perturb_neighbors` kept coherent with the grid.

## Tasks

- [x] T1 (b531cf7) — S1 — inline — domain sizing
- [x] T2 — S2, S3 — inline — config + engine wiring
- [x] T3 — S4 — inline — monthly breaker in engine
- [ ] T4 — S5 — inline — paper propagation
- [ ] T5 — S6 — inline — HPO space
- [ ] T6 — all — final checks + independent verifier

## Log

- L1 (2026-10-16): "Investiga si esto es real y crea un plan de implementación exhustivo para solventarlo" (pasted plan: Binance parity, daily/monthly breakers, HPO granularity).
- L2 (2026-10-16): "pues si ya has verificado lo que era verdad y lo que no. Centrate en aplicar las correcciones adecuadamente".
- L3: Investigation: daily breaker already exists; 20x is a self-imposed risk cap (Binance brackets are per symbol); monthly breaker and paper event absent. Monthly measured from in-month peak (assumption, user question unanswered).
