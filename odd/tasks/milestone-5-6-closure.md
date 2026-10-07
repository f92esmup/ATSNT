# Milestone 5/6 Closure

## Objective
Close and verify the identified dashboard and live gateway gaps, then push reviewable feature/tracker branches. Do not implement Milestone 7 or merge main.

## Authorization and scope
User authorized fixes, verification and push. Delivery: auto-chain, feature-branch-chain; no PR creation or merge. Futures scope explicitly selected: USD-M, USDT single-asset, one-way, isolated margin, dedicated account with ATSNT as sole order writer. Unsupported account modes must fail closed; exchange remains final order admission authority. Complete protection within this supported profile must not be misrepresented as general Futures parity.

## Constraints
Single writer; Decimal financial arithmetic; no production unwrap/expect; bounded asynchronous work; no credentials or live exchange calls in tests. Deterministic offline tests first where meaningful: observe RED, GREEN, refactor. Confirm current official exchange documentation before Futures implementation. Loopback default with explicit allowed origins for tunnel deployment. Existing event timestamps and explicit strategy/symbol metadata rather than invented clock values.

## Baseline evidence
Read-only verifier: cargo test --locked --offline --workspace passed 68 tests (adapters 17, backtest 16, domain 27, strategies 3, web 5); cargo fmt --check and cargo clippy --locked --offline --all-targets -- -D warnings passed. Git clean. Chromium and Playwright CLI available; browser launch not yet tested.

## Delivery and review
Tracker branch: feature/milestone-5-6-closure; base f9c748b. Ordered work-unit slices will branch from the last verified boundary, with the tracker advanced by fast-forward only. Forecast initially 450-750 authored lines, increased by full supported-profile margin modeling; revised estimate 900-1600, to be refined after source verification. Generated/vendor assets excluded. Actual authored line count pending. No size-driven compression or omission of tests. RDD enabled by global user switch; candidate risk/consent and native review follow provider authority per commit or bounded slice, never per checklist update. Push only verified branches at closure.

## Tasks
- [x] T1: Enforce dashboard perimeter and WebSocket origins.
  - Route: delegated `gentle-ai-worker` (multi-file/preparation triggers); independent `gentle-ai-verify` fallback-high verification.
  - Accept: met. Literal loopback listener only; exact immutable HTTP(S) origin allowlist; explicit tunnel origins; Host and Origin validation before CORS/upgrade; fails closed on malformed/untrusted browser requests.
  - TDD: worker observed 8 intended integration RED failures and CLI validation RED failures before changes; final GREEN.
  - Checks: `cargo test --locked --offline -p web --test api_tests` PASS (19); `cargo test --locked --offline -p web` PASS (26 total: 3 library, 4 binary, 19 integration); `cargo fmt --check` PASS; `cargo clippy --locked --offline -p web --all-targets -- -D warnings` PASS. Independent verifier reran suite/fmt/Clippy successfully.
  - Native review: approved and acknowledgement burned for commit `63ee37df9810c6d7d2cdfeed4779983d1c3f8ff3` against `f9c748be065896b4fa772ba8fd1987bf477438fa`. Initial assessment was unassessable due untracked tracker files; selected committed candidate via inspect and native review then completed. Four lenses submitted.
  - Commit: `63ee37df9810c6d7d2cdfeed4779983d1c3f8ff3` (`fix(web): enforce loopback and trusted dashboard origins`), branch `fix/dashboard-perimeter`.
  - Follow-ups, non-blocking reviewer advisories: informational hexadecimal-origin alias at `crates/web/src/security.rs:88`; rejection observability at `crates/web/src/lib.rs:74-77`; readability at `security.rs:217`. Review did not require correction. Real socket/browser/tunnel smoke remains T7.
- [ ] T2: Complete timestamped, symbol/strategy-aware telemetry and reliable dashboard snapshots. **IN PROGRESS**
  - User selected backward-compatible option B: retain raw `PaperTradingEvent` and `PaperTradingConfig` APIs; add separate telemetry configuration/envelope subscription, event timestamps from `Trade.timestamp`, and explicit session identity.
  - **T2a — Envelope API and WebSocket contract:** implementation and architecture docs are in place; independent verifier passed `cargo test --locked --offline -p backtest` (18 tests), `cargo test --locked --offline -p web` (28 tests), `cargo fmt --check`, and affected-crate Clippy. Actual loopback HTTP 101 and event/snapshot frames are exercised. RED was reported by writer before implementation. Pending work-unit commit and native review. Verifier found the initial snapshot JSON does not yet restore all UI state; addressed separately by T2b.
  - **T2b — Snapshot fidelity and resilience:** pending. Restore equity/cash/PnL/drawdown/open-position UI from initial/reconnect snapshots; keep the state aggregator alive on broadcast lag and expose uncertainty rather than presenting possibly stale state as authoritative; configure the mock demo identity before its first event while retaining a null timestamp. Add deterministic regression tests and update the architecture contract as needed.
  - T2a and T2b are split to keep distinct API/wire-contract and UI/state-resilience review candidates. No source corrections for T2b have been written yet.
  - Evidence/commit/review for T2a: checks passed; commit and native review pending.
- [ ] T3: Wire a real paper session into the dashboard process.
  - Route: delegated worker; preparation and multi-file trigger.
  - Accept: explicit in-process session mode, retained mock mode, synthetic trades update dashboard state, graceful lifecycle and documented invocation.
  - Checks: web/backtest offline tests and executable help, format/Clippy.
  - Evidence/commit/review: pending.
- [ ] T4: Add and wire Spot pre-dispatch funds/reserve checks.
  - Route: delegated worker; domain/adapter multi-file trigger.
  - Accept: correct base/quote balances by side, conservative fees/reserves, malformed/missing data rejection, funds checked before dispatch with offline tests.
  - Checks: domain/adapters tests, format/Clippy.
  - Evidence/commit/review: pending.
- [ ] T5: Implement complete margin guard for the selected Futures account profile.
  - Route: delegated worker after official documentation verification; high-consequence domain/adapter trigger.
  - Accept: validate account modes and contract filters, fresh coherent account/position/order/mark/bracket/fee inputs, leverage/exposure/margin/cost reserves and in-flight reservations; fail closed on missing/stale/inconsistent data, retain uncertain submission reservations. Explicit supported scope and exchange-authority limitation documented.
  - Checks: deterministic Decimal fixtures including boundaries, adverse exposure, pending orders, failures and concurrent submission; offline domain/adapters tests, format/Clippy.
  - Blocker: official documentation retrieval/review pending (research task muxsi4pm-5-ltxg).
  - Evidence/commit/review: pending.
- [ ] T6: Normalize Futures user events and maintain listen-key lifecycle.
  - Route: delegated worker; multi-file trigger.
  - Accept: official Spot/Futures fixtures, bounded renewal with explicit failure behavior, shutdown/reconciliation safeguards, no raw listen-key logging.
  - Checks: adapters tests with fake clock/gateway, format/Clippy.
  - Evidence/commit/review: pending.
- [ ] T7: Reconcile milestone docs, verify end-to-end locally, and push verified chain.
  - Route: delegated writer for docs if needed; verifier for full suite and browser.
  - Accept: roadmap and architecture checklists reflect actual supported scope; full offline suite/fmt/Clippy; local browser smoke against mock plus synthetic paper integration. Record all skipped checks and limitations. All work-unit candidates resolved before authorized push; no merge or PR.
  - Checks: cargo test --locked --offline --workspace; cargo fmt --check; cargo clippy --locked --offline --all-targets -- -D warnings; Chromium/Playwright local smoke; clean git and remote branch identities.
  - Evidence/commit/review: pending.

## Progress and next step
T1 is complete and natively approved/acknowledged. T2a envelope implementation, WebSocket tests, and documentation are present on `fix/telemetry-envelope`; independent package tests, format, and Clippy passed. T2a still needs its commit and native review. The verifier identified snapshot UI restoration and aggregator lag resilience gaps; these are tracked as T2b before proceeding to T3. Official Futures evidence remains a write blocker only for T5. No push, PR, merge, live exchange call, or production credential use has occurred. Close each split work unit with observed checks and its own commit/review; update this document, Engram mirror, and visible todo after transitions. Tracker changes are recorded in the next cohesive work-unit commit to avoid an unreviewed standalone scope.
