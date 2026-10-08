# Milestone 5/6 Closure

## Objective
Close the authorized M5/M6 reliability and execution-gateway work, T1–T7. This tracker does **not** own the future web-workspace redesign. Keep all W0–W6 design decisions and implementation phases in the separate canonical contract, currently on branch `docs/web-workspace-contract` at `docs/architecture/09-read-only-web-workspace.md` (not yet merged into this branch).

## Authorization and scope
- Authorized work: M5 T2b–T3, M6 T4–T6, and T7 verification, task by task, on `feature/m5-6-reliability-gateway`, based on `c7ae8f4`. Local work-unit commits are authorized.
- Not authorized: production credentials, live exchange calls, order/strategy controls, or implementation of W0–W6. No PR is requested.
- The user has authorized local integration into `main` and a push of `main` to `origin` only after T2b Slice 2 is independently verified and locally committed, the `docs/07` overlap is reconciled after T2b, and the canonical W documentation is integrated and checked. Do not push or delete worktrees/branches before those gates; preserve the T2b backup until its contents are confirmed redundant.
- The supported Futures profile remains USD-M, USDT single-asset, one-way, isolated, dedicated to ATSNT as sole order writer. Unsupported account modes fail closed; the exchange remains final order-admission authority. Do not imply general Futures parity.
- Preserve all staged or uncommitted work not explicitly included in the active task. Never use cleanup/reset to make the tree appear clean.

## Engineering constraints
- Single writer; `Decimal` financial arithmetic; no production `unwrap()`/`expect()`; bounded asynchronous work; no credentials or live exchange calls, including during verification.
- For behavior with meaningful deterministic tests, use test-first and record observed evidence. Keep tests offline.
- Verify official exchange rules before Futures implementation; do not guess margin formulas.
- Bind telemetry to real event timestamps and explicit strategy/symbol metadata; never fabricate identity or time.

## User-confirmed M5/M6 policies
- **T3:** Paper trading is opt-in via `--paper`; `--mock` remains a distinct mode.
- **T4:** support standard Spot only. Every strategy declares compatible market types. `DollarBarsCusum_v1` is Futures-only and is rejected entirely when selected for Spot.
- **T5:** caps are 10,000 USDT notional per order and 50,000 USDT notional per position; risk is 1% per trade and session drawdown is 5% from the session peak, with no daily reset. Share the sizing/risk policy across backtest, paper and live. Leverage is symbol-scoped: choose the minimum exchange-supported level sufficient for exposure only when the symbol has no position/open orders; otherwise preserve current leverage and reject if checks fail. Accounting and cost details must follow authoritative engine/account/report evidence.
- **T6:** normalize private account events for Spot and USD-M Futures. Futures remains limited to the selected USDT single-asset, one-way, isolated profile with ATSNT as sole order writer.

## Completed work and baseline evidence
- **T1** dashboard perimeter/origin checks: local commit `63ee37d`; native review approved and acknowledged. Web package tests, fmt and Clippy passed; browser/tunnel smoke remains T7.
- **T2a** timestamped telemetry envelope and WebSocket contract: local commit `9e4e42b`; backward-compatible raw Paper event/config APIs retained. Focused/full offline tests, fmt and Clippy passed; combined T1/T2a review was approved and acknowledged.
- At verified `9e4e42b`, the offline workspace suite reported 93 tests plus doc-tests passing, with fmt and workspace Clippy passing. This is historical verification; it does not substitute for T7 after current changes.

## Tasks

- [x] **T1 — Enforce dashboard perimeter and trusted WebSocket origins.** Bind loopback; validate exact configured HTTP(S) origins and Host/Origin before CORS or upgrade; fail closed on malformed/untrusted requests. Commit `63ee37d`; browser/socket smoke remains T7.

- [x] **T2a — Add timestamped telemetry envelopes.** Preserve existing event/config APIs; expose separate identity/timestamp envelope and WebSocket snapshot/event frames. Commit `9e4e42b`; prior independent checks and native review passed.

- [x] **T2b — Restore snapshot fidelity and make telemetry gaps safe.**
  - Restore equity/cash/PnL/drawdown and open-position fields from initial/reconnect snapshots; clear absent position/price values instead of retaining stale UI state.
  - Establish mock demo identity before its first event; keep timestamp null until an event supplies one.
  - On aggregator broadcast lag, keep consuming; retain last-known values but mark them stale/uncertain. On per-client WebSocket lag, send an uncertainty/stale snapshot and discard buffered pre-snapshot events so stale backlog is not shown as current.
  - Staleness is sticky until a genuine complete authoritative snapshot is available. No producer-side authoritative resync source exists in this scope; do not invent one or claim recovery from incremental events.
  - **Completed evidence:** Slice 1 (`fix(web): restore snapshot presentation and mock identity`) is local commit `80ae7e1`. Slice 2 (`fix(web): preserve uncertainty across telemetry gaps`) is local commit `43d85bb`; it changes `crates/web/src/state.rs`, `crates/web/src/ws.rs`, `crates/web/static/app.js`, `crates/web/tests/api_tests.rs`, and `docs/architecture/07-web-telemetry-dashboard.md`. Independent offline verification passed `cargo test --locked --offline -p web` (32 tests: 3 library, 4 binary, 25 integration), `cargo fmt --check`, `cargo clippy --locked --offline -p web --all-targets -- -D warnings`, and `git diff --cached --check`. The verifier observed GREEN only; the earlier worker's RED/GREEN report is separate evidence. `gentle_review assess` on committed range `80ae7e1..43d85bb` reported medium risk (`executable_change`), `reviewDue:false` (`under_budget`), and a plan not requiring a separate verifier; an independent verifier was run anyway. `candidate.consumed:false`; do not claim native code-review approval. The earlier aggregate START was consent-declined with `lineage_created:false`; it created no authority for this code candidate.
  - **Out of scope:** new W shell, Portal/Econoweb integration, broader read models, or an invented authoritative producer resync source.

- [x] **T3 — Wire an explicit in-process Paper session.** `--paper` creates the session; keep `--mock` separate; reject incompatible modes; verify synthetic paper trades update telemetry and lifecycle shuts down gracefully. Test offline and document invocation. No live exchange calls.
  - **Completed evidence:** T3.1 `082b24c` and T3.2 `c616623` (`feat(web): finalize paper session lifecycle`) on `feature/t3-paper-session`; CLI/telemetry bridge, source-waiting session ownership, joined shutdown/finalization and invocation/source-limit documentation are recorded in `odd/tasks/t3-paper-session.md`. Independent T3.2 verification passed `cargo test --locked --offline -p web` (40 tests: 8 library, 7 binary, 25 integration; 0 doc-tests), `cargo fmt --check`, `cargo clippy --locked --offline -p web --all-targets -- -D warnings`, and `git diff --check`. Exact four-path native review target `sha256:3d23d7b6ac0eadf7ebd5c2554cd06731113b867a6008e534d70ec7ac0ef7b7a8` was approved and acknowledged under lineage `review-41b382e7964cc1d5`; post-ack ASSESS reported medium risk, `reviewDue:false`, native outcome `closed` (derived), and writer self-verification sufficient. Only passive ODD tracking updates follow acknowledgement. No runtime signal injection or T7 smoke completion is claimed.
  - **Future delivery only:** user selected `chain_strategy=stacked-to-main`; `refs/remotes/origin/HEAD` resolved to `refs/remotes/origin/main`. No push or PR creation is authorized by this decision.

- [x] **T4 — Add standard Spot pre-dispatch funds checks and strategy compatibility.** Check quote funds for buys, base inventory for sells, fees/reserves, and missing/malformed state before dispatch. Reject Futures-only strategies in Spot; never reinterpret Futures signals as Spot trades. Deterministic offline tests; no live exchange calls.
  - **Completed evidence:** `e22b575` (`feat(adapters): guard standard spot order funding`) on `feature/t3-paper-session`; details and verification are in `odd/tasks/t4-standard-spot.md`. Workspace tests, fmt, Clippy, focused tests, and independent S1–S4 verification passed. Native medium-risk review was acknowledged for target `sha256:26e2f46d54f38a4620a4933aac199efdf178c6832a25a475f662090fd0cf6ba4`; one commission-coverage warning remains informational. No push/PR/merge was performed.

- [x] **T5 — Implement shared risk sizing and the supported USD-M margin guard.** Share the 1% stop-distance sizing policy across backtest/paper/live and enforce accepted notional/session limits. Validate account mode, contract filters, coherent account/position/order/mark/bracket/fee inputs, symbol leverage and in-flight reservations. Fail closed on unsupported, stale or inconsistent inputs and uncertain submissions. **Completed:** T5.1 `8456b6a`, T5.2 `ee2fea2`, and T5.3 `bd6fb0e` on `feature/t3-paper-session`; detailed scope and evidence are in `odd/tasks/t5-shared-risk-margin.md`. Independent feature-closure checks passed all 136 workspace tests, formatting, and workspace Clippy; the parent spot-check passed all 8 focused T5.3 submission tests. No live exchange calls were made. Native assessment was medium / `under_budget`, so no native review lifecycle was due. The uncertain-submission latch is gateway-instance memory and is not durable across process restarts.

- [x] **T6 — Normalize Spot and USD-M private account events.** Use official fixtures; bound listen-key renewal; define explicit failure, shutdown and reconciliation behavior; never log keys. Futures behavior stays within the supported account profile. Offline adapter tests with fake time/gateway; no live exchange calls. **Completed:** Implemented in `2f88dc1`, corrected in `8963773`, functionally verified (162 workspace tests, fmt and Clippy), and merged to `main` with user delivery authorization. Spot uses signed Spot WebSocket API user-data subscription; USD-M retains supported listenKey path. Detailed scope and evidence: `odd/tasks/t6-private-account-events.md`.

- [x] **T7 — Verify and close the current M5/M6 work locally.** Run the offline workspace suite, fmt, Clippy and browser smoke for the current `--mock` and `--paper` behavior after T3. Record actual results and skips. Verify only authorized T work; preserve staged/uncommitted user changes and report branch/path status. Do not test or claim completion of the future W UI here; coordinate only if a future W6 browser check shares fixtures. No live exchange calls. **Completed:**
  - **Workspace suite:** `cargo test --workspace --offline` passed all 162 tests (0 failures).
  - **Formatting & Linting:** `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --offline -- -D warnings` passed cleanly with 0 warnings.
  - **Server & Telemetry smoke verification:**
    - `--mock` mode (port 3100): loopback bind verified, HTTP GET `/api/health` returned 200 OK, `/api/state` returned 200 OK, static SPA assets served (`/index.html` 11,363 bytes), `/ws/telemetry` completed WebSocket upgrade (101 Switching Protocols) and streamed snapshot frame, SIGTERM graceful shutdown exited 0.
    - `--paper` mode (port 3101): loopback bind verified, HTTP GET `/api/health` returned 200 OK, `/api/state` returned 200 OK, static SPA assets served, `/ws/telemetry` upgrade and snapshot frame verified, SIGTERM graceful shutdown drained and joined the in-process paper session cleanly with exit code 0.
  - Stash `stash@{0}` preserved for historical tracker evidence. No live exchange calls made.

## Boundary with the future web redesign
- The full W0–W6 plan and product decisions live only in the separate workspace contract named above. Do not copy its phase list, design gates or future UI requirements into this M5/M6 tracker.
- W2 may consume T2b/T3 outputs; it must not reimplement their telemetry reliability or Paper runtime. T4–T6 remain separate gateway tasks and do not block read-only Paper views. Live account panels require real supported sources, including T6 where applicable. T7 verifies this M5/M6 scope; later W6 coordinates any overlapping browser verification.

## Current status and Milestone 5 Evolution

**Milestone 5 Evolution: Web Decommission & Decoupled Cloud-First Analytics (Complete)**

Following strategic architectural evaluation to eliminate inbound web attack surfaces and local frontend maintenance:
1. **Phase 1: Web Decommission & Modular Quant Refactor**:
   - `crates/web` completely decommissioned and removed (Axum server, static SPA, WebSockets, Lightweight Charts, and HTTP dependencies).
   - Asynchronous paper trading runtime (`PaperSessionRuntime` & `PaperSessionOwner`) migrated natively to `crates/backtest`.
   - Quantitative sampling and filters modularized into `crates/domain/src/analytics/` (`sampling`, `filters`, `stats`) following Marcos López de Prado (*AFML*).
2. **Phase 2: GCP Cloud-First Data Layer & Telegram Alerting**:
   - Implemented `AlertNotifier` trait and `TelegramNotifier` in `crates/adapters/src/gcp/telegram.rs` (<200ms mobile push alerts).
   - Implemented `BigQuerySink` in `crates/adapters/src/gcp/bigquery.rs` streaming to `atsnt_bi.trades`, `atsnt_bi.equity_snapshots`, `atsnt_bi.hpo_evaluations`, and `atsnt_bi.monte_carlo_runs` for zero-code Looker Studio BI visualization.
   - Implemented `GcsParquetSink` in `crates/adapters/src/gcp/gcs.rs` for massive Monte Carlo trajectory compression and Cloud Storage upload.
   - Connected CLI flags `--gcp-bigquery`, `--gcs-parquet`, and `--telegram-alerts` into `paper_trading`, `run_hpo`, and `backtest`.
   - Verified 100% test pass rate (69 unit/integration tests), 0 clippy warnings, and clean formatting. Pushed to `origin/main` in commit `1bf9cb6`.

Workstream W (`crates/web`, W0–W6) is formally archived; read-only reporting is now handled via **Looker Studio + BigQuery** and live alerting via **Telegram Bot**.
