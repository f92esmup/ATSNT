# hardening-phase0
Objective: close the code gaps left by `sim-live-fidelity` so testnet, HPO and production can be run on a clean slate ("NO se tolera errores por la naturaleza del proyecto").
Branch: feat/sim-live-fidelity (continues) | Delivery: ask-on-risk | Test runner: `cargo test --workspace --no-fail-fast` (+ `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`)

## Specs
- S1: The live runner and the tick simulation drive one `TradingSession` through the same code path (market ticks, bars, executions, wallet), so the loop order that backtests validate is the one live runs, and it is unit-tested.
- S2: A market entry that never reaches a terminal status cannot leave the session waiting forever: after a timeout it cancels what is working, protects whatever filled, or returns to flat, and reports it.
- S3: The engine's per-UTC-day peak logic is `DailyWatermark`, the same type live uses.
- S4: The analytics data model is final before BigQuery is purged: trades carry `funding_paid`, runs carry the fill model, and tick-level runs can be recorded.
- S5: The commits that touch the real-money path get the native review (needs the user's consent).
- S6 (assumption): the unbuilt `Clock` and `MarketData` venue ports are dropped from the roadmap unless the user wants them; no code needs them.

## Tasks
- [x] T1 (S1) — `SessionDriver` in `backtest`, used by `TickSimulation` and `live_trading` | inline | RED: `driver::tests::*`
- [x] T2 (S2) — entry timeout in `TradingSession` | inline | RED: `session::tests::an_entry_that_never_terminates_*`
- [ ] T3 (S3) — engine uses `DailyWatermark` | inline | previous behavior pinned by the existing engine breaker tests
- [ ] T4 (S4) — `funding_paid` column and fill-model tag in the run model and BigQuery schema; tick runs recordable | inline | RED: recording contract tests
- [ ] T5 (S5) — native review of the real-money commits | needs consent
- [ ] T6 (S6) — roadmap wording for the dropped ports | structural readback

## Log
- L1 (2026-10-17): "si empiza con la fase 0" after the plan: Phase 0 = make the live loop testable, bound the pending-entry wait, unify the engine's daily peak with `DailyWatermark`, finalize the BigQuery data model, run the native review. "NO se tolera errores por la naturaleza del proyecto." BigQuery data may be purged and the VM executions stopped and deleted, in Phase 1 and only with explicit approval of project, VM, datasets and commands.
