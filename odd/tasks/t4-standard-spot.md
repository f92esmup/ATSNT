# T4 — Standard Spot Funds Guard
Objective: add explicit market compatibility and fail-closed Spot pre-dispatch balance/commission checks on `feature/t3-paper-session`.
Delivery: local work-unit commit only; no push/PR. Test runner: Cargo offline.

## Specs

- **S1 — Strategy compatibility:** “Every strategy declares compatible market types. `DollarBarsCusum_v1` is Futures-only and is rejected entirely when selected for Spot.”
- **S2 — Spot funds preflight:** “Check quote funds for buys, base inventory for sells, fees/reserves, and missing/malformed state before dispatch. Reject Futures-only strategies in Spot; never reinterpret Futures signals as Spot trades.”
- **S3 — Commission assets:** the user selected the recommended policy: “consultar las tasas por símbolo y comprobar también el saldo del activo en que pueda cobrarse la comisión; rechazar si faltan datos.” Resolve market and commission assets from authoritative symbol/account data; fail closed on missing, malformed, unpriceable, or insufficient required inputs. Use the conservative taker rate for a limit order that may execute immediately, and account for standard, special, and tax commission components and the documented discount behavior.
- **S4 — Offline verification:** implement deterministic tests against local/fake HTTP responses only; do not use credentials or make live exchange calls.

## Tasks

- [x] **T4.1 — Implement Spot compatibility and funds/commission preflight.** Route: inline. Status: complete. Commit: `e22b575`.

## Log

- **L1 (2026-10-08):** “Implement the plan.”
- **L2 (2026-10-08):** User confirmed the recommended commission-asset-aware policy: “quiero apliiicar el recomendado”.
- **L3 (2026-10-08):** Baseline is clean at `feature/t3-paper-session`, HEAD `0f4bff9`; adapter offline suite passes (17 tests). The current gateway's Spot `fetch_balance` treats missing asset/fields as zero, and order placement does not check Spot funds before posting. Official Binance docs expose symbol-specific commission data and explain maker/taker, buyer/seller, standard/special/tax, and optional BNB-discount behavior. No live exchange requests were made.
- **L4 (2026-10-08):** Engram mirror is pending because this runtime did not provide an authoritative session identity; do not issue an unattributed memory write.
- **L5 (2026-10-08):** Test-first RED observed. The local mock-gateway test failed because an underfunded Spot buy was accepted and posted. Strategy compatibility tests fail to compile because `MarketType`/`compatible_market_types` do not exist; the runner rejection test fails to compile because no market-validation entrypoint exists. Sandbox initially denied loopback binding; the exact offline test was rerun with approval and made no external requests.
- **L6 (2026-10-08):** Implemented `MarketType` compatibility (`DollarBarsCusum_v1` is USD-M-only), rejected `--spot` before WebSocket setup, and added fail-closed standard Spot symbol/account/commission/asset preflight before POST. Commission upper bound sums standard, special, tax, and side rates; a funded discount asset is checked using a fresh direct/inverse ticker price, otherwise fees must fit the received asset. Updated architecture docs and CLI help.
- **L7 (2026-10-08):** All focused tests, `cargo test --locked --offline --workspace --no-fail-fast`, `cargo fmt --all -- --check`, `cargo clippy --locked --offline --workspace --all-targets -- -D warnings`, and `git diff --check` passed. Independent verifier passed S1-S4; first identified stale `--spot` help, corrected the wording, and independently rechecked `cargo run --locked --offline -p backtest --bin paper_trading -- --help` (exit 0, correct help). The offline adapter/workspace tests required approved local loopback; no exchange access occurred.
- **L8 (2026-10-08):** Risk: item 3 (public `Strategy` contract and Spot order-dispatch guard). RDD is on; native review is pending after the work-unit commit. No commit, review consent, or delivery approval is implied yet.
- **L9 (2026-10-08):** Work-unit commit `e22b575` was reviewed at medium risk and acknowledged under lineage `review-c49c5bd38cd0a9ae`; native approval was burned for this exact target. No blocking finding remained. One informational warning (`R3-commission-coverage`, reliability lens, `binance_gateway.rs:543`) remains a separate follow-up; it did not open correction or reopen review. The review does not authorize push, PR, merge, or release.
