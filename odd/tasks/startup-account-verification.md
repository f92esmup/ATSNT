# Startup Account Verification for the Live Bot

Objective: before the live bot opens any stream, verify (and where safe fix) the Binance USD-M account profile the strategy needs, and keep verifying while it runs.
Branch: `feat/startup-account-verification` (from `main`) | Delivery: single branch, work-unit commits | Test runner: `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`.

## Specs

- **S1 — Account profile.** User: "si la estrategia está preparada para una cuenta aislada usdt futures ... que se compruebe todo eso antes de empezar". Required: isolated margin on the symbol, one-way position mode, single-asset (USDT) mode, USDT-only collateral, auto-add-margin off, and a flat account (no position, no regular or conditional orders) at startup.
- **S2 — Verify at startup.** `BinanceGateway::verify_account(symbol, auto_fix, allow_exposure)` reads the real configuration through the API before any stream is opened and returns a report of findings (`Ok`, `Fixed(from -> to)`, `Blocking(reason, remedy)`).
- **S3 — Fix only what is safe.** User: "lo permito" (automatic change of the margin type). The only automatic change is the symbol margin type to `ISOLATED` (`POST /fapi/v1/marginType`), and only when the account is flat with no orders; it is re-read to confirm. `-4046` (already set) counts as success; `-4047`/`-4048` stay blocking.
- **S4 — Stop with a clear error otherwise.** Hedge mode, multi-asset mode, non-USDT collateral, auto-add margin, pre-existing positions or orders, leverage not viable: the bot stops and prints what to change and where.
- **S5 — Leverage is an output, not an input.** User: "hemos dicho que la estrategia establece el apalancamiento para cada operación? si ese es el comportamiento ... hay que respetarlo". The verification never sets a leverage. It only checks that the exchange allows what sizing may need (bracket maximum covers the feasible leverage for the largest order). The per-order preflight keeps choosing the minimum sufficient leverage as today.
- **S6 — Keep verifying.** Re-verify periodically and after a stream reconnect or listen-key expiry. Flat: fix. With a position: never touch anything; on drift raise a critical alert and block new entries.
- **S7 — Operator controls.** `--verify-only` prints the report and exits without changing anything; `--no-auto-fix` verifies and stops on any mismatch.

## Tasks

- [x] T1 — S1, S2, S4, S5 — inline — read-only verifier and report
- [x] T2 — S3 — inline — margin type fix with re-read
- [x] T3 — S2, S6, S7 — inline — wire the live bot (startup, re-verification, flags)
- [ ] T4 — all — testnet harness subcommand, docs, independent verifier

## Log

- L1 (2026-10-16): "adelante, comienza la implementación" after the plan; margin auto-fix allowed ("lo permito"); "hemos dicho que la estrategia establece el apalancamiento para cada operación? ... hay que respetarlo. El resto si estoy de acuerdo". Verified: `OrderIntent` has no leverage field; memory #109 and strategy doc §4 say leverage is derived from risk sizing.
