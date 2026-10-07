# Milestone 5/6 Closure

## Objective
Close and verify the identified dashboard and live-gateway gaps, while defining a reusable, read-only web workspace for ATSNT and future products. Keep the operational dashboard separate from order/strategy control. The redesign phases below are a plan; they do not authorize implementing the new UI.

## Authorization and scope
The current user authorized a read-only audit, full offline workspace verification, completion and acknowledgement of the already-started T1/T2a native review, reconciliation of this task document, a phased redesign plan, and local-only consolidation of verified work to `main` followed by deletion of merged local branch names. This does not authorize pushing, changing `origin/main`, creating a PR, live-exchange calls, production credentials, order/strategy controls, or implementation of the future redesign phases. Open product decisions below remain gates to UI implementation.

The previously selected Futures scope remains USD-M, USDT single-asset, one-way, isolated margin, dedicated account with ATSNT as sole order writer. Unsupported account modes must fail closed; the exchange remains final order-admission authority. Complete protection within this supported profile must not be misrepresented as general Futures parity.

## Constraints
Single writer; Decimal financial arithmetic; no production unwrap/expect; bounded asynchronous work; no credentials or live exchange calls in tests. Deterministic offline tests first where meaningful: observe RED, GREEN, refactor. Confirm current official exchange documentation before Futures implementation. Loopback default with explicit allowed origins for tunnel deployment. Existing event timestamps and explicit strategy/symbol metadata rather than invented clock values.

## Baseline and current verification evidence
At the original baseline, `cargo test --locked --offline --workspace` passed 68 tests (adapters 17, backtest 16, domain 27, strategies 3, web 5), with `cargo fmt --check` and workspace Clippy passing. Chromium and Playwright CLI were available, but browser launch had not been tested.

At verified HEAD `9e4e42b`, the independent verifier ran `cargo test --locked --offline --workspace` (93 tests passed, zero failed; doc-tests passed), `cargo fmt --check`, and `cargo clippy --locked --offline --all-targets -- -D warnings`; all exited 0. `git status --short --branch` was unchanged before and after. These checks do not replace the still-pending browser/mock/paper end-to-end smoke test under T7.

## Delivery, branch state and review
The earlier delivery forecast was 450–750 authored lines, later revised to 900–1,600 for the broader supported Futures profile; it remains historical planning evidence, not authorization to push. No size-driven compression or omission of tests is permitted. Native review evidence is candidate-scoped and never grants delivery authority.

Before local consolidation, refs were: `main` and `feature/milestone-5-6-closure` at `f9c748b`; `fix/dashboard-perimeter` at `63ee37d`; `fix/telemetry-envelope` at `9e4e42b`; and `origin/main` at `f9c748b`. The fix branches form a stack. The user-directed terminal state is one local branch, `main`, with the approved work preserved by fast-forward. Delete local refs only after their commits are reachable from `main`, using safe non-forced deletion. Leave `origin/main` unchanged; no remote branches beyond `origin/main` were present, and no push is authorized.

T1 was natively approved and acknowledged for commit `63ee37d`. The combined T1/T2a candidate was independently reviewed as high risk (1,565 changed lines) under lineage `review-863923a69b04544e`, approved, and acknowledged; its authority is consumed. The four reviewer lenses submitted were risk, resilience, readability and reliability. The review opened no correction. Its non-blocking follow-ups are recorded under T2a and must not be used to reopen that candidate.

## Tasks
- [x] T1: Enforce dashboard perimeter and WebSocket origins.
  - Route: delegated `gentle-ai-worker` (multi-file/preparation triggers); independent `gentle-ai-verify` fallback-high verification.
  - Accept: met. Literal loopback listener only; exact immutable HTTP(S) origin allowlist; explicit tunnel origins; Host and Origin validation before CORS/upgrade; fails closed on malformed/untrusted browser requests.
  - TDD: worker observed 8 intended integration RED failures and CLI validation RED failures before changes; final GREEN.
  - Checks: `cargo test --locked --offline -p web --test api_tests` PASS (19); `cargo test --locked --offline -p web` PASS (26 total: 3 library, 4 binary, 19 integration); `cargo fmt --check` PASS; `cargo clippy --locked --offline -p web --all-targets -- -D warnings` PASS. Independent verifier reran suite/fmt/Clippy successfully.
  - Native review: approved and acknowledgement burned for commit `63ee37df9810c6d7d2cdfeed4779983d1c3f8ff3` against `f9c748be065896b4fa772ba8fd1987bf477438fa`. Initial assessment was unassessable due untracked tracker files; selected committed candidate via inspect and native review then completed. Four lenses submitted.
  - Commit: `63ee37df9810c6d7d2cdfeed4779983d1c3f8ff3` (`fix(web): enforce loopback and trusted dashboard origins`), branch `fix/dashboard-perimeter`.
  - Follow-ups, non-blocking reviewer advisories: informational hexadecimal-origin alias at `crates/web/src/security.rs:88`; rejection observability at `crates/web/src/lib.rs:74-77`; readability at `security.rs:217`. Review did not require correction. Real socket/browser/tunnel smoke remains T7.
- [ ] T2: Complete timestamped, symbol/strategy-aware telemetry and reliable dashboard snapshots. **IN PROGRESS because T2b remains pending**
  - User selected backward-compatible option B: retain raw `PaperTradingEvent` and `PaperTradingConfig` APIs; add separate telemetry configuration/envelope subscription, event timestamps from `Trade.timestamp`, and explicit session identity.
  - **[x] T2a — Envelope API and WebSocket contract:** implemented and documented. Commit `9e4e42b` (`feat(telemetry): add timestamped paper event envelopes`) is on `fix/telemetry-envelope`, after T1 commit `63ee37d`. The focused backtest/web suites and the full workspace checks passed; loopback HTTP 101 plus event/snapshot frames are covered. The combined T1/T2a native review (`review-863923a69b04544e`) was approved and acknowledged; no correction was required. The task doc's former “commit/review pending” entry is superseded by this evidence.
  - **[ ] T2b — Snapshot fidelity and resilience:** pending. Restore equity/cash/PnL/drawdown/open-position UI from initial/reconnect snapshots; keep the state aggregator alive on broadcast lag and expose uncertainty rather than presenting possibly stale state as authoritative; configure mock demo identity before its first event while retaining a null timestamp. Add deterministic regression tests and update the architecture contract as needed. Current aggregator receive loop exits on a broadcast receive error, including lag; this is a tracked reliability gap, not a passing behavior.
  - T2a and T2b remain separate work units: the API/wire contract is complete; snapshot/state resilience has not been implemented. No T2b source correction is included in the reviewed candidate.
  - Non-blocking native-review advisories for separate follow-up only: `R2-duplicate-channel-capacity-default` (`crates/backtest/src/paper.rs:189`, SUGGESTION) and `R3-frontend-assertions` (`crates/web/static/app.js:274-275`, WARNING); both were informational, opened no correction, and do not invalidate the approved candidate.
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
- [ ] T7: Reconcile milestone documentation and verify end-to-end locally; do not push.
  - Route: delegated writer for multi-file documentation changes if needed; verifier for full suite and browser.
  - Accept: roadmap and architecture reflect the actual supported scope; full offline suite/fmt/Clippy; local browser smoke against explicit mock and real in-process paper session. Record skipped checks and limitations. Keep UI observation-only. Local branch consolidation is covered separately below; push/PR/remote changes require new authorization.
  - Checks: `cargo test --locked --offline --workspace`; `cargo fmt --check`; `cargo clippy --locked --offline --all-targets -- -D warnings`; Chromium/Playwright local smoke; clean worktree and local/remote branch inventory.
  - Evidence/commit/review: pending; the 93-test suite, fmt and Clippy already passed at T2a, but end-to-end browser verification remains pending.

## Shared web workspace redesign (proposed workstream; implementation not authorized by this plan)

### Product definition and boundaries
Build a modular, data-first workspace with a common shell, not a CRM. The shell has global product navigation and, inside Trading, a persistent module navigation. The operational area is strictly read-only: filters, chart inspection, comparison and replay are allowed; sending/canceling/closing orders, pausing/stopping strategies, changing risk parameters, or launching experiments are not. Job submission would require a separate future authorization and security design.

Portal and Econoweb were not found in this repository. Do not assume they are existing packages or local modules. The shared-shell integration choice is an explicit W0 gate. Until that decision is made, design ATSNT's module boundaries and a reusable visual contract without inventing integrations.

### Open product decisions that block implementation
- Decide whether Portal and Econoweb are separate applications consuming a versioned shared design package, or modules in one application with one router/session. Determine which repositories own the package and how versions are released.
- Clarify “Dollar Bars que yo estoy creando manualmente”: does the user configure the dollar-volume threshold, import manually prepared bars, or build bars from imported trades? Current Rust aggregation constructs Dollar Bars from trades; the web UI has no manual data-entry workflow.
- Define which paper/live data sources are actually supported. A `live` label is not proof that a live engine is connected; show mode, source connection and data freshness as separate values. Do not fabricate a live-state view while the live pipeline is absent.
- Define account, portfolio, strategy version and strategy-instance identity; specify capital/PnL attribution when several instances share one account.
- Approve metric formulas and applicability: equity, available balance, realized/unrealized PnL, drawdown, exposure, capital-at-risk, margin, fees/funding, daily boundary and timezone. Unsupported values are `not applicable` or `unknown`, never zero by default.

### Information architecture
- **Global rail:** Portal, Trading, Econoweb. Collapsible to icons on wide screens; becomes one mobile drawer. Do not render placeholder destinations as functioning modules before their owners exist.
- **Trading rail:** Overview, Operations, Laboratory, Strategies, Data & Market, System. Keep this as a second persistent navigation level, not a hover menu or third sidebar.
- **Operations:** global-by-default context; filters for account/environment, asset and strategy instance; connection/freshness strip; equity/balance/PnL/drawdown/exposure cards; Dollar Bars chart; open positions; read-only order/fill/trade history; event/alert journal. “Global” aggregates only distinct account capital and must not double-count shared positions.
- **Laboratory:** Backtests, HPO and Monte Carlo history/detail/comparison. Show run configuration, dataset/version, methodology and actual time-series outputs. Replay means inspect recorded events unless an explicitly separate rerun capability is later designed.
- **Strategies:** catalog, version, configured parameters, eligible assets and read-only instance status. **Data & Market:** datasets, source, bar type/threshold, ranges and freshness. **System:** server/producer/exchange connectivity, lag, last event, errors and alert history. No operational command buttons.

### Reusable visual contract (initial proposal; verify contrast before adoption)
| Token/element | Contract |
|---|---|
| App background / surface / elevated surface | `#0B1120` / `#111827` / `#1E293B` |
| Primary / secondary text / border | `#F1F5F9` / `#94A3B8` / `#334155` |
| Accent / positive / negative / warning | `#3B82F6` / `#22C55E` / `#F87171` / `#FBBF24` |
| Typography | Inter when available, otherwise system sans-serif; tabular numerals for metrics; monospace only for IDs/technical values |
| Spacing scale | 4, 8, 12, 16, 24, 32 px |
| Radius | 6 px controls, 8 px panels |
| Controls | 36 px desktop minimum; 44 px touch target |
| Layout | 56 px header; global rail 208 px expanded / 64 px collapsed; module rail 224 px |
| Motion | 120–180 ms; honor reduced-motion preferences |
| Accessibility | WCAG 2.2 AA target; visible focus, keyboard operation, semantic labels; color is never the only state cue |

Use shared tokens/components rather than copy-pasted CSS. Whether those live in a versioned cross-project package or in the current static frontend is decided at W0. Keep the current vanilla HTML/CSS/ES modules unless the integration decision demonstrates a framework/build-system need; do not add a framework for visual reasons alone.

### Data and API contract principles
- Current API facts: Axum serves read-only REST (`/api/health`, `/api/state`, `/api/strategies`, `/api/reports`, `/api/reports/:id`) and `/ws/telemetry`; T2a adds an identified five-field envelope (`timestamp`, `strategy_id`, `symbol`, `event_type`, `payload`). Preserve that envelope for compatibility; any version, sequence or identity expansion requires an explicit versioned migration.
- Target records need stable `account_id`, `environment`, `symbol`, `strategy_id`, `strategy_version`, `instance_id`, dataset/report IDs, source timestamps and freshness. Mode (`paper`/`live`), connection (`connected`/`disconnected`), engine state and data age are distinct dimensions.
- Keep financial values as decimal strings in JSON; retain `Decimal` in Rust. Define units, quote currency, precision, realized vs unrealized PnL, fees and valuation time. Never infer accounting in browser floating point.
- Every reconnect starts from a complete authoritative snapshot; streaming gaps/lag are observable and trigger resynchronization. A last-known snapshot must show its timestamp and stale/unknown state.
- Only read endpoints and telemetry streams belong in the web contract. Orders/executions may be displayed as history; there are no POST/PUT/DELETE order or strategy-control routes from the UI.
- Normalize immutable report metadata: report/run ID, type, strategy/version, parameters, symbol(s), period, dataset/version/hash, engine version, cost/slippage configuration, seed/method and schema version. Backtest detail must expose actual equity/drawdown series and trades; do not synthesize a curve from only initial/final equity. HPO exposes objective, search space and walk-forward folds; Monte Carlo exposes method, scenario count, seed, percentile curves and the exact ruin definition. The browser does not launch jobs in this scope.
- Candidate read models/route names are to be frozen in W2 after existing DTOs and report JSON are reconciled; do not treat endpoint sketches in discussion as an approved public API.

### Phased task plan

- [ ] **W0 — Approve the product, visual and data contract.**
  - Record the verified baseline: T1/T2a complete; T2b/T3 pending; current API and report limitations above.
  - Resolve Portal/Econoweb topology, ownership of the shared design package, and route/session boundaries.
  - Resolve the manual Dollar Bars input workflow and supported data sources for paper/live.
  - Freeze entity identity and aggregation semantics for account, asset, strategy version and strategy instance.
  - Approve formulas, currency/precision, stale-data behavior, timezone and accessibility target.
  - Approve the two-level navigation, no-execution boundary and visual tokens.
  - **Accept when:** decisions and explicit non-goals are recorded; no unknown product decision is silently embedded in UI/API contracts. No code is written in this phase.

- [ ] **W1 — Build the shared shell and design system.** Depends on W0.
  - Implement the global rail, Trading module rail, header/context strip, content region, breadcrumbs/routes and collapsible/responsive behavior.
  - Implement tokenized colors, typography, spacing, panels, buttons/filters, tables, tabs, badges, tooltips, empty/loading/error/stale states.
  - Define keyboard focus, semantic names, reduced motion, contrast checks and mobile drawer behavior.
  - Keep Portal/Econoweb as registered integrations only after their topology and owners are confirmed.
  - **Accept when:** shell routes and collapses consistently at supported widths, is keyboard accessible, uses the approved token source, and has no fake product pages or execution controls.

- [ ] **W2 — Stabilize read-only data and report contracts.** Depends on W0 and the existing M5 T2b/T3 work; do not duplicate or bypass those tasks.
  - Complete M5 T2b snapshot restoration, broadcast-lag recovery and freshness signaling with deterministic tests.
  - Complete M5 T3 wiring of a real in-process paper session while retaining explicit mock/demo mode.
  - Define versioned multi-account/asset/strategy-instance read models; preserve T2a raw-event compatibility and clearly version any envelope change.
  - Define complete reconnect snapshots, source/event timestamps, stale/unknown state and missing-data semantics.
  - Add read-only tests proving no order/strategy mutation endpoints are exposed.
  - **Accept when:** paper data is real and attributable, multiple contexts cannot be conflated, reconnect/lag behavior is explicit, financial values remain decimal strings, and unsupported live data is shown as unavailable.

- [ ] **W3 — Build the Trading operations center.** Depends on W1 and W2.
  - Make the default view global; allow scoped account, asset and strategy-instance drill-down without losing context.
  - Show paper/live mode separately from server/exchange connectivity and data age.
  - Add formula-backed equity, balance, realized/unrealized PnL, drawdown, exposure and applicable margin/risk cards.
  - Render real Dollar Bars with threshold/source metadata, signals and simulated/actual fills visually distinguished.
  - Add read-only open-position, order, fill, closed-trade and event/alert tables with time-range filters.
  - **Accept when:** global totals reconcile without double counting; every metric shows units/time/applicability; no order submission, cancellation, close, pause or stop action exists.

- [ ] **W4 — Normalize research history and detail views.** Depends on W0/W1; it may proceed independently of paper runtime once report contracts are agreed.
  - Reconcile current `storage/reports` backtest, HPO and Monte Carlo JSON against the UI DTOs and preserve raw provenance.
  - Build a filterable immutable run catalog and details with strategy/version, parameters, symbol, date range, dataset and execution-cost methodology.
  - Use actual equity, drawdown and trade series; show KPI definitions and mark unavailable metrics as unavailable.
  - Show HPO objectives/search spaces/folds and Monte Carlo method/seed/scenario-count/percentile metadata.
  - Add versioned fixtures/tests for every report type and schema evolution.
  - **Accept when:** each listed report opens correctly from its real JSON; no filename-only type inference or fabricated chart series is required.

- [ ] **W5 — Add interactive analysis and reproducible replay.** Depends on W4.
  - Add chart zoom/crosshair/range selection, trade/signal overlays, metric drill-down and compatible-run comparison.
  - Visualize HPO fold and parameter-stability results; show Monte Carlo distribution and percentile bands with methodology.
  - Implement replay only over recorded event/time-series evidence, with pause/step/speed and source/version labels. Distinguish replay from a new simulation run.
  - Keep experiment/job execution out of scope; no controls may mutate strategy or engine state.
  - **Accept when:** every visual is derived from stored, traceable data and replay can be tied to the exact report/dataset/strategy version.

- [ ] **W6 — Add supporting sections, integrate and retire the old UI safely.** Depends on W1–W5 and W0's cross-project decision.
  - Add the read-only strategy catalog, data/market provenance and system-health/event pages.
  - Integrate Portal/Econoweb using the agreed shell contract; if they are external, validate the shared package/version contract rather than inventing local modules.
  - Exercise responsive layout, keyboard/accessibility, browser reconnection/staleness, mock mode and in-process paper mode.
  - Verify GET/WebSocket-only web access, trusted origins, no leaked exchange credentials and no live exchange calls in tests.
  - Run workspace tests, format, Clippy, frontend/browser checks; record failures/skips and supported behavior.
  - Keep the old page available until parity and acceptance are demonstrated; remove it only after rollback/restore path and parity checks pass.
  - **Accept when:** user tasks work end-to-end in the new shell, all screens remain read-only, checks pass, and old UI retirement is reversible and evidence-based.

### Workstream boundaries and terminal repository state
M5 T4 (Spot funds), T5 (Futures margin) and T6 (Futures user events) remain separate execution-gateway tasks. They are not prerequisites for a read-only paper dashboard; future live metrics must remain unavailable/explicitly stale until their sources exist. M5 T5 remains blocked only on official exchange documentation. M5 T7 still owns its browser and full milestone closure evidence.

The user-directed local cleanup preserves the approved stack: after this documentation work unit is recorded and assessed, fast-forward local `main` to the reviewed feature tip, then delete the merged local names `feature/milestone-5-6-closure`, `fix/dashboard-perimeter` and `fix/telemetry-envelope` with non-forced deletion. Verify clean status and exactly one local branch (`main`). Keep `origin/main` untouched and do not push. Never delete an unmerged branch or the native review's managed snapshot/worktree as a substitute for normal lifecycle cleanup.

## Progress and next step
T1 and T2a are complete, committed and natively approved/acknowledged. The full offline workspace suite (93 tests plus doc-tests), `cargo fmt --check` and workspace Clippy passed at `9e4e42b`; no browser smoke was run. T2b (snapshot restoration/lag resilience) and T3 (real paper-session wiring) remain the prerequisites for the operational redesign. T4–T6 are separate gateway work. Two review advisories are informational follow-ups, not blockers or permission to reopen the consumed candidate. The integrated plan is a proposal; Portal/Econoweb topology, manual Dollar Bars workflow and metric definitions remain open W0 decisions. The integrated-plan documentation work unit is commit `4878d182487a04de4dcd9ba8fea684c2d17b875d` (`docs(roadmap): integrate read-only workspace phases`); native assessment classified the committed delta as passive, and structural readback plus `git diff --check` passed. Local consolidation is complete: the approved stack was fast-forwarded to `main` at `0804b81282f9ce36a0f772b910696cdb3f1a1bef`; the three merged local feature branches were deleted without force. The worktree is clean with only local `main`; `origin/main` remains at `f9c748b` and no push occurred. Next: obtain the W0 product decisions before implementing the redesigned UI.