# sim-live-fidelity
Objective: 1:1 fidelity between the simulated and live environments, and strategy-agnostic hexagonal wiring.
Branch: feat/sim-live-fidelity | Delivery: ask-on-risk | Test runner: `cargo test --workspace` (+ `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`)

## Specs
- S1: Live sizing and risk limits come from the strategy deployment artifact, not from `RiskPolicy::default()` or a hardcoded 1% risk per trade. An explicit `--risk-pct` still overrides the artifact.
- S2: The live daily drawdown breaker uses the same semantics as the simulator: peak equity per UTC day, reset at 00:00:00 UTC.
- S3: Live PnL reports the real commissions charged by the exchange instead of an estimated taker fee.
- S4: `docs/ROADMAP.md` must not claim features that do not exist (M4 "Real-time order matching against live bid/ask spreads").
- S5 (decision, user): entries are `market` orders (taker), "si quiero que sea market".
- S7: A `ports` crate holds the contracts between the core and the outside world (market data, alerts, exit/execution, later the venue and clock) and depends only on `domain`; adapters implement them and no contract is defined inside a concrete adapter.
- S8: One `TradingSession` owns the entry -> protection -> exit -> accounting lifecycle independently of the venue, and is tested with a fake venue.
- S9: Live runs through that session via a `BinanceVenue` with unchanged observable behavior (signal gates, exchange-held exits, time barrier, halts, alerts, trade recording).
- S6 (later phases): venue ports (`ExecutionVenue`, `Clock`, `MarketData`) with `SimulatedVenue` and `BinanceVenue`; tick-level realism (latency, tape-based market slippage, taker TP, mark-price stop); funding and liquidation; parity harness with live warm-up; single `StrategyRegistry` plus a generic strategy contract suite.

## Tasks
- [x] T1 (S1) P0 — risk policy and risk-per-trade from the artifact in live | inline | commit: 7418287 | RED: `risk_policy_*` (config.rs), `futures_order_quantity_uses_the_configured_risk_per_trade` | previous behavior kept: `futures_preflight_chooses_minimum_supported_leverage_before_dispatch`, `legacy_artifact_context_*`
- [x] T2 (S2) P0 — UTC-daily peak watermark in live | inline | commit: 5d77a3c | RED: `daily_watermark::tests::*`
- [x] T3 (S3) P0 — real commissions in live PnL | inline | commit: 66739e1 | RED: `futures_order_trade_update_*commission*`, `fee_ledger::tests::*`
- [x] T4 (S4) P0 — ROADMAP/README/doc-comment correction | inline | commit: eba49d8 | structural readback (passive docs)
- [x] T5.1 (S7) P1 — `ports` crate: move `MarketDataStream`, `AsyncMarketDataStream`, `AlertNotifier`, `ExitGateway`; old paths re-export | inline | pure move, no behavior change | RED: `ports_depend_only_on_the_domain` (architecture_contract.rs); previous behavior kept by the unchanged suite | commit: see log
- [x] T5.2 (S8) P1 — `ExecutionVenue` port + `TradingSession` with a fake venue; not wired to live | inline | RED: `session::tests::*` (13 cases) | commit: see git log
- [x] T5.3 (S9) P1 — `BinanceGateway` as `ExecutionVenue` + `live_trading` migrated to the session | inline | RED: `a_futures_order_update_converts_to_a_venue_execution_report`, `the_gateway_acts_as_an_execution_venue_for_entries` | money path: loop wiring has no automated test, needs testnet validation by the user | commit: see git log
- [ ] T6 (S5, S6) P2 — market entries, latency, tape slippage, taker TP, mark-price stop | pending
- [ ] T7 (S6) P3 — funding and liquidation | pending
- [ ] T8 (S6) P4 — warm-up and parity harness | pending
- [ ] T9 (S6) P5 — `StrategyRegistry` and contract suite | pending, can start after T5

## Log
- L5 (T5 closed, 2026-10-16): observable changes of the live runner after the migration: (a) entry and exit prices are the VWAP of all fills instead of the last fill; (b) `exit_reason` is `StopLoss`/`TakeProfit`/`TimeBarrier`/`MonthlyCircuitBreaker`/`ProtectionFailed`/`ExchangeClose` instead of the order status `FILLED`; (c) `slippage_paid` is measured against the signal and trigger prices instead of 0; (d) a signal while a position exists reports `Position already open` before `exits pending`; (e) the order banner shows the client order id only; (f) the emergency-close retry throttle uses trade time instead of wall clock; (g) the Telegram trade summary shows the exit reason instead of `FILLED`. Entry order type is still the LIMIT GTC at the signal price: T6 replaces it by MARKET. `EntryPending` still has no timeout (T6).
- L1 (2026-10-16): "Quiero que hagas una investigación exhaustiva sobre la fidelidad entre el netorno simulado y live. Nosotros buscamos una similitud 1:1 para que las estrategias que evaluamos no arrojen datos sesgados. Se ha conseguido? hay cosas que ajustar? Lo segundo a auditar es que el proyecto este aplicando bien la generalización y esquema hexagonal, que todos los modulos sean estandarizados para qeu cuando cree una nueva estrategia. esta no tenga fricción."
- L2 (2026-10-16): "me parece una idea genial lo de las venues. Voy a seguir tus recomendaciones. si quiero que sea market. Adelante con la refactorización. vamos paso a paso"
- L4 (P0 closed, 2026-10-16): suite, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` green after each task. Notes for later phases: (a) live derives entry price/qty from the last fill only (`last_filled_price`, `cumulative_filled_quantity`); market entries will often fill in several trades, so T6 must use the order's average price; (b) the gateway mock's leverage endpoint always answers 2x, so gateway tests that change notional must keep it near 1000 USDT; (c) `BinanceGatewayConfig` is built by literal in 5 places without `..Default::default()`, so new fields touch them; (d) the engine still keeps its own inline daily-peak logic: T5 should make it use `DailyWatermark` when the session core is extracted; (e) `--risk-pct` is now `Option`, so an unspecified flag means "artifact value" instead of a fixed 1%.
- L3: Audit evidence: live gateway sizes with `RiskPolicy::default()` (live_trading.rs:322) and `Decimal::new(1, 2)` (binance_gateway.rs:1213); the artifact `risk_pct`/`execution_context` only reach run metadata. Live passes a whole-session peak to the breaker while the engine resets daily (engine.rs:189-197).
