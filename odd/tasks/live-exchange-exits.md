# Live Exchange-Side Exits and Monthly Emergency Close

Objective: give the live session exits that survive a process crash (SL/TP held by the exchange) and a real monthly emergency close.
Branch: `feat/live-exchange-exits` (from `main` + A/B branch commits) | Delivery: single branch, work-unit commits, no merge before user-approved testnet validation | Test runner: `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`.

## Specs

- **S1 — Exchange-held SL/TP.** User: "Las salidas en live se gestionan por el exchange para que asi sobreviva". After the entry fills, the stop loss and take profit are placed as USD-M conditional orders through `POST /fapi/v1/algoOrder` (`algoType=CONDITIONAL`, `STOP_MARKET` / `TAKE_PROFIT_MARKET`, `closePosition=true`, `workingType=MARK_PRICE`, opposite side to the entry; the legacy `/fapi/v1/order` rejects these types). Trigger prices are aligned to the symbol tick size toward the entry price.
- **S2 — Never unprotected.** If either protective order cannot be confirmed, the open position is closed at market and every pending order cancelled, and an alert is raised.
- **S3 — Cancel on exit.** When the position is closed by any exit fill, the remaining protective and entry orders are cancelled (Binance Futures has no OCO).
- **S4 — Time barrier.** Not expressible on the exchange: the bot closes at market after `max_bars_hold` bars. Documented limit: only this barrier depends on the process being alive.
- **S5 — Monthly emergency close.** On `MonthlyGuard` trip: cancel all orders, close the position with a `MARKET` `reduceOnly` order, bounded retries, state `Running -> Halting -> Halted`; failure leaves `Halting` with a critical alert, never a false `Halted`.
- **S6 — Testnet only until approved.** No call against Binance (testnet or production) is made without explicit user authorization. User authorized the testnet run (L3-L5); production was never touched.

- **S7 — Verified on testnet.** `testnet_exit_check scenario` passes 19/19 against Binance USD-M testnet; crash survival confirmed with `leave-open`. Findings fixed on the way: `TRADE_LITE` closed the private stream, and `canTrade` is absent from `/fapi/v3/account`.

## Tasks

- [x] T1 — S1, S3, S5 — inline — gateway primitives (protective exits, cancel, close at market, position amount)
- [x] T2 — S2, S3, S4, S5 — inline — `ExitManager` state machine over a gateway trait, mock-tested
- [x] T3 — S1-S5 — inline — wire the live loop, docs, runbook
- [x] T4 (verifier: no blockers; advisories fixed or documented) — all — independent verifier

## Log

- L1 (2026-10-16): "Las salidas en live se gestionan por el exchange para que asi sobreviva. Adelante con los cambios".
- L2: Binance docs (New Algo Order, developers.binance.com): conditional types moved to `/fapi/v1/algoOrder`; `closePosition` cannot be combined with `quantity` or `reduceOnly`. The preflight already reads `/fapi/v1/openAlgoOrders` and rejects when conditional orders exist, so cleanup after every exit is mandatory.
- L3 (2026-10-16): "Si, realiza la ejecución". Credentials: env vars, else GCP `mi-facturador-bot03`. L4: "Es el único proyecto existente en mi cuenta. Úsalo" (that project returned permission denied; the active gcloud project was `mi-facturador-bot-01`). L5: "Si" to using `mi-facturador-bot-01`.
- L6: Testnet run: 19/19 checks passed; crash survival confirmed. Fixed `TRADE_LITE` closing the private stream and the missing `canTrade` in `/fapi/v3/account`; real payloads kept as fixtures.
