# Read-Only Web Workspace: Product Contract & Phased Plan

## 1. Status, Authority & Review Path

**Proposed contract, not implementation authorization.** ATSNT's target is a reusable, data-first analytical workspace, not a CRM or an execution console. This document is canonical for the proposed product/UI contract and W0–W6 workstream. Start with the W0 decision gates, then the screen/data requirements, then the independently trackable phase tasks.

**Historical baseline:** `c7ae8f4107b9214a3c7f4fc20193393b65da617c` remains the base snapshot for this plan, not a claim about current implementation. **Current facts** below incorporate T2b completed in `43d85bb` (`fix(web): preserve uncertainty across telemetry gaps`); T3–T7 have not started. The [milestone closure tracker](../../odd/tasks/milestone-5-6-closure.md) owns T-task status and implementation evidence; this document neither supersedes its completed work nor marks W phases complete. [Dashboard specification](07-web-telemetry-dashboard.md) remains the current API source-of-truth. Confirmed product decisions below constrain the target; implementation remains **proposed**, subject to W0's remaining technical gates. Future capabilities need separate authorization.

### Current implementation and known gaps

- Axum serves GET `/api/health`, `/api/state`, `/api/strategies`, `/api/reports`, `/api/reports/:id`, static assets and `/ws/telemetry`. T2a's five-field envelope is `timestamp`, `strategy_id`, `symbol`, `event_type`, `payload`; raw lifecycle payloads remain externally tagged. Financial decimals serialize as strings; source trade timestamps are Unix milliseconds, not delivery times.
- The state model represents one producer identity and one active position, not a multi-account portfolio. Its initial 10,000 cash/equity defaults are not evidence of connected capital. The strategy catalog is hard-coded, not a runtime-instance registry.
- T2b is complete in `43d85bb`: HTTP state and WebSocket snapshots carry `stale` and the full display projection. The browser restores equity, cash, last price, unrealized PnL, drawdown and full position details, clearing position/price displays when null. Aggregation continues after broadcast lag, but `stale` uncertainty stays sticky despite later events or newer timestamps. Individual lagging clients receive uncertainty snapshots and discard pre-snapshot buffered backlog without marking other clients' shared state stale; shared aggregator uncertainty also notifies clients independently of later producer events. Browser uncertainty persists across reconnects. No producer-authoritative resync source exists: reconnect and HTTP projections are not recovery. This plan consumes that reliability behavior, not an authoritative resync capability.
- The binary offers explicit `--mock` synthetic demo mode, but no real in-process paper-session wiring. Without mock it has no market-event producer; T3 owns that wiring. A paper bridge in a socket test does not demonstrate runtime readiness.
- Report indexing infers types from filenames (`hpo`, `paper`, otherwise `backtest`) and extracts best-effort metrics. Detail returns raw JSON. The browser currently fabricates a two-point initial/final equity curve using current wall-clock times. Its Monte Carlo reader expects `fan_chart_curves`, whereas [the report specification](05-monte-carlo-and-telemetry.md) illustrates `fan_chart_trajectories`. W4 must reconcile producers, stored evidence and consumers; this document does not rename fields or claim a normalized report schema.
- T1 enforces loopback binding and trusted Host/Origin checks. Tunnel authentication is a deployment responsibility, not proved by those checks. “Air-gapped” means no web mutation path here, not physical network isolation or immunity to host compromise. Simulator fee/slippage modeling does not establish general live exchange fidelity.
- Portal and Econoweb were not found in this repository. No functioning placeholder, local module, package or integration is assumed.

Evidence locations: `crates/web/src/{lib,main,state,ws}.rs`, `crates/web/src/handlers/{state,reports}.rs`, and `crates/web/static/app.js`. These are evidence references, not edit targets for this documentation task.

## 2. W0 Decision Register & Remaining Gates

### Confirmed product decisions

| Topic | Required target |
|---|---|
| Topology | Separate Portal, Trading and Econoweb apps with a shared, versioned shell/design system. Portal is an app launcher only, not a cross-product KPI dashboard. |
| Deployment and locale | Local, single-user use; Spanish/English UI; display UTC and local time. No invented multi-user authentication system. |
| Identity and scope | One Binance identity, distinct standard Spot and USDⓈ-M Futures wallets. Global view covers only the selected wallet/environment, never combined wallets. Futures profile: USDT, isolated, one-way; ATSNT is its sole order sender. |
| Valuation and risk | Spot equity uses fresh USDT reference prices with source/age; unreliable valuation means unknown aggregate equity/risk, not Futures available margin. Apply the caps and session policy in Section 4. |
| Modes and bars | Explicit `--paper`, distinct from `--mock`; Mock shares Operations with a persistent DEMO band. Engine supplies formed Dollar Bars; browser does not form them. |
| Market compatibility | Each strategy declares market compatibility; `DollarBarsCusum_v1` is Futures-only and rejected in Spot. T6 private events cover Spot and USD-M Futures. |
| Research | Saved bar-level and engine-event replay; persist formed bars and engine-produced evidence, without requiring raw trade/tick archives for v1. Standard report KPI sets are specified in Section 5. |
| Freshness and safety | Per-source heartbeat thresholds; separate event/bar ages, no stale inference from an unformed bar. Web operations remain read-only, including no session-close/reset or experiment submission. |

### Resolved Technical Gates (W0 Decision Acceptance)
*(Detailed resolution and mathematical proofs in [`odd/tasks/w0-decision-gates.md`](../../odd/tasks/w0-decision-gates.md))*

| Gate | Resolved Decision & Specification |
|---|---|
| Shared package delivery | Vanilla ES6 modules + native CSS Custom Properties in `crates/web/static/` (`css/`, `js/`). Zero Node/npm build dependencies in production runtime; shared tokens exported via `:root`. |
| Source/read-model availability | Explicit Binance Spot vs USD-M Futures (isolated, USDT, one-way) wallet contexts, never aggregated. Zero floating-point rule with decimal strings (`Decimal` in Rust). Preserved T2a 5-field envelope and sticky uncertainty (`stale: true`). |
| Versioned accounting/report evidence | Empirically audited `storage/reports/` schema. Normalization to accept both `fan_chart_curves` and `fan_chart_trajectories`. Zero-Fiction Rule: missing equity curves show "Series not available" rather than fabricated lines. |
| Heartbeat contracts | Multi-tier freshness: transport WebSocket warning at >5s, reconnect at >15s. Separate trade event age from Dollar Bar age, acknowledging volume-clock irregular duration. |
| Visual/accessibility | W3C-validated color tokens meeting WCAG 2.2 AA (text ratios 17.19:1 AAA and 6.92:1 AA; active controls 4.82:1 UI non-text). Desktop dual-rail, tablet collapsible rail, and mobile keyboard-accessible drawer. Bilingual ES/EN with dual UTC/local clock. |
| Deployment perimeter | Strict loopback bind (`127.0.0.1`), validated Host/Origin headers, zero exchange trading credentials exposed to browser, read-only GET and `/ws/telemetry` endpoints only. |

All six technical gates are formally resolved and documented. Unverified assumptions are eliminated, unlocking **W1 (Shared Shell & Design System)**.

## 3. Proposed Information Architecture & Visual Contract

A shared **global product rail** links the separate Portal / Trading / Econoweb apps; Portal launches apps only. Within Trading, a **persistent module rail** offers Overview, Operations, Laboratory, Strategies, Data & Market, System. No hover-only navigation or third sidebar. Rails collapse responsively; on mobile use one drawer with clearly separated product and module levels, focus return and keyboard dismissal. Unowned destinations must not masquerade as working pages. Support Spanish and English throughout; show UTC and local time with explicit timezone labels for local single-user use.

| Section | Read-only responsibility |
|---|---|
| Overview | Global summary, source coverage, freshness and links to scoped analysis. |
| Operations | Account/asset/instance observation, metrics, Dollar Bars, positions and history. |
| Laboratory | Existing backtest, HPO/walk-forward and Monte Carlo report catalog, detail, comparison and recorded replay. |
| Strategies | Definitions, versions, configured parameters, eligible assets and runtime-instance status, without commands. |
| Data & Market | Dataset/source provenance, ranges, bar types/thresholds, gaps and freshness. |
| System | Server, producer and exchange connection states; engine status, event age, lag, errors and alert history. |

### Exact initial token proposal — pending W0 contrast/accessibility review

| Token/element | Proposed value |
|---|---|
| Background / surface / elevated | `#0B1120` / `#111827` / `#1E293B` |
| Primary text / secondary text / border | `#F1F5F9` / `#94A3B8` / `#334155` |
| Accent / positive / negative / warning | `#3B82F6` / `#22C55E` / `#F87171` / `#FBBF24` |
| Typography | Inter or system sans; tabular numerals for metrics; monospace for technical IDs |
| Spacing | 4 / 8 / 12 / 16 / 24 / 32 px |
| Radii | Controls 6 px; panels 8 px |
| Controls | Desktop minimum 36 px; touch targets 44 px |
| Layout | Header 56 px; global rail expanded 208 px / collapsed 64 px; module rail 224 px |
| Motion | 120–180 ms; respect reduced-motion preferences |
| Accessibility | WCAG 2.2 AA target; visible focus, semantic labels, keyboard support; never color alone |

Provisional widths: below 768 px use the drawer and stacked panels; 768–1199 px use a collapsed global rail and collapsible module rail; from 1200 px allow both expanded rails. W0 must validate content fit, zoom and target devices rather than treating these breakpoints as approved.

Share a single token source and reusable shell, context strip, metric cards, panels, filters, tables, tabs, badges, tooltips and state components. Avoid copied CSS across products. W0 determines package ownership. Retain current static frontend conventions unless an integration need justifies a build/framework change; appearance alone is not a reason to choose a JavaScript framework.

Loading, empty, error, disconnected, stale, unknown and not-applicable are distinct states. Retain last-known data with source/time/stale labels, not a green connection badge implying validity. Charts need accessible textual summaries or equivalent tables; status uses text/icons as well as color. Focus and context must survive navigation, collapse and filter changes.

## 4. Proposed Operations Contract

**Observation only:** filters, chart inspection, comparison and recorded replay are allowed. No submit/modify/cancel/close orders, pause/stop strategies, edit risk or runtime parameters, session-close/reset controls, experiment submission, or hidden execution controls. Replay pause/step controls affect only the viewer, never the engine.

Select the Binance wallet (standard Spot or USDⓈ-M Futures) and environment first; its global view then drills into asset and strategy instance with scope always visible. Never combine wallets or count wallet equity once per strategy. Distinguish a strategy **definition**, immutable **version** and running **instance**; attributed values must reconcile to sourced wallet totals. The agreed Futures profile is USDT, isolated, one-way, with ATSNT as sole order sender; Spot funds are not its isolated margin. Each strategy declares market compatibility; reject Futures-only `DollarBarsCusum_v1` in Spot.

Show mode, source/server/exchange connections, engine status and freshness separately. `--paper` is explicit and distinct from `--mock`; Mock uses this same panel with a persistent DEMO band, never presenting synthetic values as exchange balances. Show source/connection heartbeat separately from last-event and last-formed-bar ages, using configurable source-specific thresholds. No new Dollar Bar alone does not imply staleness: bar formation has irregular duration. Unsupported live values are unavailable; previously sourced values may be explicitly stale. A connected WebSocket alone means neither an active engine nor live readiness.

### Metrics and chart

- Show equity, available balance, realized and unrealized PnL, drawdown, exposure and capital-at-risk. Margin, funding and leverage appear only for applicable sourced account/instrument profiles.
- Every KPI exposes definition, unit/currency, precision, valuation time, source, costs included and applicability. Unknown or not-applicable is **not zero**. Exact PnL, fees, funding and risk accounting follow versioned engine/report evidence, not browser-invented formulas or floating-point accounting.
- Main chart uses already formed Dollar Bars supplied by the engine, never browser aggregation from trades, with threshold, source/dataset, instrument and quote unit. Inspect OHLCV, dollar volume, start/end and duration. Dollar Bars have **irregular time duration**, not fixed clock intervals.
- Distinguish finalized **formed** bars from provisional **forming** bars, missing intervals/gaps, strategy signals, and actual versus simulated fills using labels/shapes as well as color. If forming bars or fill evidence are absent, say unavailable rather than deriving fictional events.
- Bottom read-only views: positions, orders, fills, closed trades and events/alerts, with time filters, stable IDs, source timestamps and selected scope. No execution affordances, including hidden close/cancel/stop/pause actions.

### Wallet valuation and risk policy

- Value all Spot holdings in USDT with fresh reference prices; expose price source and age. Unreliable price evidence makes aggregate equity/risk **unknown**. Spot valuation is not USD-M Futures available margin.
- Caps: **10,000 USDT order notional**, **50,000 USDT position notional**, and **1% risk per trade** from current selected-wallet equity.
- Session drawdown limit: **5% from the session equity high-watermark**, computed from authoritative session-equity evidence. Persist the watermark across process restarts until explicit session close; **no daily reset**. The web UI cannot close/reset it.
- Strategies share risk budgets within the same wallet/environment. Backtest, Paper and Live do not pool session state. Versioned engine/report evidence governs exact PnL, fee and funding semantics; the browser displays, not redefines, them.

## 5. Proposed Laboratory Contract

Read existing reports only: backtests, HPO/walk-forward and Monte Carlo. Catalog, inspect and compare compatible runs; replay only recorded evidence. **Replay is not a rerun.** Job submission, experiment launching and strategy/engine mutation are future work outside W0–W6.

Require immutable provenance: run ID/type, strategy definition/version, parameters, instrument(s), dataset ID/version/hash, date range, engine version, cost/slippage configuration, seed, method and schema version. Missing legacy metadata stays explicitly unavailable; normalization must retain raw provenance.

Use actual stored equity, drawdown and trade series with their real axes and timestamps. Never fabricate two-point curves or substitute today's dates. If series are absent, display an unavailable-series state while retaining valid summaries. KPIs need formulas, periods, units, cost treatment and applicability. Comparison checks dataset/range, currency, strategy/version, engine/cost configuration and methodology; incompatible runs are rejected or prominently qualified rather than silently combined.

### Standard quantitative summaries

| Report | Required KPI set, when backed by report evidence |
|---|---|
| Backtest | Net return, max drawdown, Sharpe/Sortino, profit factor, expectancy, win rate, trade count and costs. |
| HPO | Objective, fold/out-of-sample results and parameter stability. |
| Monte Carlo | Ending-equity and drawdown percentiles; ruin probability only with a defined threshold/method in report evidence. |

Exact formulas and annualization come from versioned engine/report evidence; the browser must not recompute incompatible metrics. Missing evidence is unavailable, not fabricated.

Saved replay supports bar-level and engine-event views. Persist formed bars and engine events: signals, order/fill lifecycle and resulting position/equity evidence. V1 does not require a raw trade/tick archive. Display only evidence actually present in saved reports; playback never launches or recomputes experiments.

Expose HPO objective, search space, parameter stability and out-of-sample walk-forward folds including train/test/embargo boundaries. Expose Monte Carlo method, scenario count, seed, block configuration when relevant, distribution/percentile definitions and exact ruin threshold/definition. Stored synthetic scenario paths are valid research evidence when labeled as such, not actual executions. Do not assume the illustrative fan-chart schema is the implemented schema.

## 6. Proposed Data & Read-Only API Principles

- Stable IDs for account, environment, instrument, strategy definition, strategy version, runtime instance, run, dataset, order, fill and position. Retain `symbol` compatibility; a symbol alone is not sufficient account or instrument identity.
- Financial JSON uses decimal strings and Rust uses `Decimal`. Specify units, currency, precision and source timestamps; distinguish event, valuation and observation/delivery times. Rendering adapters may need numeric chart coordinates but cannot become accounting authorities.
- Use versioned engine/report definitions for realized/unrealized PnL and fee/slippage/funding attribution without double counting. Preserve the confirmed Spot USDT valuation policy; exact formulas and schema availability remain evidence gates.
- Each reconnect requires a complete authoritative snapshot for the selected scope, followed by a coherent stream. Detect event gaps/lag and expose uncertainty/resynchronization; show snapshot time and last-known stale/unknown state. This complete source-authoritative reconnect snapshot is a future requirement/target, not a current capability: T2b supplies last-known projections with sticky uncertainty, not authoritative resynchronization.
- Preserve T2a raw `PaperTradingEvent` and `PaperTradingConfig` APIs and five-field envelope unless explicitly versioned. Freeze read-model DTOs/routes in W2 after evidence reconciliation; this proposal adds no approved endpoint names.
- The browser contract is GET plus telemetry subscription only. Order/fill history is data, not permission to execute. Never send exchange trading credentials or listen keys to the browser. Target local single-user deployment; no multi-user auth is implied. Any future tunnel perimeter needs separate approval; no live calls belong in offline verification.

## 7. Independent W0–W6 Tasks

Every phase below is proposed and separately trackable. Checks describe future phase acceptance, not commands run by this docs-only work unit. Exact runners and authorization must be established when each implementation task is opened.

### W0 — Approve product, visual and data decisions

**Objective:** eliminate silent product assumptions before code. **Entry/dependencies:** baseline evidence and stakeholder decisions; no implementation dependency.

**Numbered deliverables (Completed in [`odd/tasks/w0-decision-gates.md`](../../odd/tasks/w0-decision-gates.md)):**
1. [x] Record historical baseline API/runtime/report gaps and confirm 100% completion of M5/M6 (T1–T7).
2. [x] Preserve Section 2's confirmed decisions and resolve all 6 technical gates (Vanilla ES6 modules + CSS variables, explicit Spot/Futures context models, zero Node/npm in prod).
3. [x] Standardize report schema reconciliation (handling both `fan_chart_curves` and `fan_chart_trajectories`), enforce Zero-Fiction rule, and define multi-tier heartbeat contracts.
4. [x] Formally validate visual tokens against WCAG 2.2 AA (text contrast 17.19:1 / 6.92:1, UI non-text 4.82:1), responsive layout rules, bilingual ES/EN dictionary, and dual UTC/local time display.
5. [x] Reaffirm air-gapped read-only perimeter (loopback bind `127.0.0.1`, zero credential exposure, GET/WS only).

**Observable acceptance:** All 6 technical gates are resolved; no assumptions remain unverified; W1 entry criteria are fully satisfied.

**Scope fence:** decisions/docs only; no code, new source integration or implied Portal/Econoweb readiness.

**Applicable checks:** decision traceability, source evidence, and mathematical token contrast evaluation verified.

### W1 — Shared shell and design system

**Objective:** reusable, accessible navigation and components. **Entry/dependencies:** W0 approved topology, tokens and navigation.

**Numbered deliverables:**
1. Build global and persistent Trading rails, header/context strip, routes/breadcrumbs and content region.
2. Implement collapsible/responsive behavior and mobile drawer with keyboard/focus handling.
3. Centralize tokens and shared panels, filters, tables, tabs, badges, tooltips and all data-state components.
4. Register other products only with confirmed owners/topology; verify contrast and reduced motion.

**Observable acceptance:** supported widths and zoom preserve usable navigation; keyboard paths and visible focus work; components use one approved token source; no fake destinations or execution controls.

**Scope fence:** shell only, no invented integrations, trading writes or framework adoption for appearance.

**Applicable checks:** component/route fixtures, responsive browser checks, contrast, keyboard and reduced-motion checks.

### W2 — Stabilize read-only data and report contracts

**Objective:** attributable, resilient read models. **Entry/dependencies:** W0 plus T2b snapshot fidelity/sticky-uncertainty resilience (complete in `43d85bb`) and T3 real paper-session wiring (pending). **Consume the existing T2b behavior and future T3 results; do not duplicate or bypass their implementation tasks.**

**Numbered deliverables:**
1. Validate T2b snapshot restoration, continued aggregation after lag and sticky uncertainty as existing input evidence; validate T3 explicit paper/mock lifecycle when available. Do not treat T2b projections as authoritative recovery.
2. Define versioned account/asset/strategy-instance models with distinct Spot/USDⓈ-M Futures contexts, declared strategy market compatibility, stable IDs and Decimal serialization, preserving T2a compatibility. Consume T6 private account evidence for both markets when available; do not imply live readiness.
3. Specify complete reconnect snapshots, coherent event ordering/gaps, timestamps and missing/stale semantics.
4. Agree report contract boundaries for W4 and test that web routes remain read-only.

**Observable acceptance:** real paper data is attributable; contexts cannot be conflated; reconnect/lag uncertainty is observable; decimal strings are preserved; unsupported live state is unavailable rather than fabricated.

**Scope fence:** no T2b/T3 reimplementation, trading controls or gateway readiness claims.

**Applicable checks:** deterministic identity/account/serialization fixtures, snapshot/reconnect/lag integration evidence and negative mutation-route checks.

### W3 — Trading operations center

**Objective:** implement the observation contract in Section 4. **Entry/dependencies:** W1 and W2 accepted.

**Numbered deliverables:**
1. Implement selected-wallet/environment global views and asset/instance drill-down with non-duplicating attribution, Spot USDT valuation and the confirmed risk/session policy.
2. Separate mode, connections, engine state and freshness; add formula-backed metric cards including capital-at-risk and applicable margin/funding/leverage.
3. Render sourced Dollar Bars, forming/formed states, gaps, signals and actual/simulated fill evidence.
4. Add read-only positions/orders/fills/closed-trades/events views and time filters.

**Observable acceptance:** shared-account fixtures reconcile totals once; metrics expose units/time/source/applicability; irregular bar duration and fill provenance are visible; no execution or strategy-write affordances exist.

**Scope fence:** observation only; unsupported data remains unavailable, not zero or a synthetic live view.

**Applicable checks:** accounting reconciliation fixtures, bar/marker/gap fixtures, stale/unknown states and browser/keyboard inspection of all views and absence of write controls.

### W4 — Normalize research history and detail

**Objective:** truthful existing-report exploration. **Entry/dependencies:** W0/W1 and agreed report contracts; may proceed independently of paper runtime, T2b and T3.

**Numbered deliverables:**
1. Reconcile real `storage/reports` producers/JSON with DTOs and UI, preserving raw metadata and documenting schema mismatches.
2. Build immutable typed catalog/detail views with provenance and versioned schema handling.
3. Render actual equity/drawdown/trade series and defined KPIs; label unavailable legacy data.
4. Expose HPO search/folds/stability and Monte Carlo method/scenarios/seed/percentiles/ruin definition.

**Observable acceptance:** real fixtures for each supported report type open correctly; type is not inferred solely from filename; no fabricated chart series; schema gaps are explicit, not silently renamed.

**Scope fence:** read existing reports only; no jobs, invented provenance or schema rewrite without source evidence.

**Applicable checks:** versioned report fixtures, missing/malformed/legacy-schema cases, series/provenance reconciliation and detail-view browser checks.

### W5 — Interactive analysis and reproducible replay

**Objective:** deepen analysis without rerunning engines. **Entry/dependencies:** W4 accepted.

**Numbered deliverables:**
1. Add zoom/crosshair/range selection, recorded trade/signal overlays and metric drill-down.
2. Compare compatible runs with explicit compatibility rules and exceptions.
3. Visualize HPO fold/stability results and Monte Carlo distributions/percentile bands with methodology.
4. Add saved bar-level and engine-event replay pause/step/speed with exact source/run/dataset/strategy-version labels; use persisted formed bars and engine evidence, without requiring raw trade/tick archives.

**Observable acceptance:** every visual traces to stored evidence; replay is reproducible for the same record; absent evidence blocks replay; incompatible comparisons are rejected or qualified.

**Scope fence:** viewer controls only; no reruns, job submission or engine/strategy mutation.

**Applicable checks:** deterministic replay fixtures, incompatible/missing-data negative cases, overlay alignment and accessible chart interaction checks.

### W6 — Supporting sections, integration and reversible retirement

**Objective:** complete the workspace without losing supported old-page behavior. **Entry/dependencies:** W1–W5 acceptance and W0 topology decision; coordinate overlapping milestone closure/browser evidence with T7.

**Numbered deliverables:**
1. Add read-only strategy/version/instance, data/market provenance and system-health/event sections.
2. Integrate confirmed Portal/Econoweb owners via W0's shell/package contract; external apps are not invented local modules.
3. Verify responsive/accessibility behavior, reconnect/staleness, explicit mock and real in-process paper modes.
4. Verify GET/subscription-only access, trusted origins and credential isolation with offline fixtures; record checks, failures and skips with T7 ownership.
5. Keep the old page until parity is accepted; retire only with an evidenced rollback/restore path.

**Observable acceptance:** supported user tasks work end-to-end; all views remain read-only; phase checks pass with limitations recorded; old UI retirement is reversible and parity-backed.

**Scope fence:** no unowned product integration, live-readiness claim, live exchange calls or premature old-UI removal.

**Applicable checks:** authorized workspace tests/format/Clippy and frontend/browser checks in the future implementation task; mock/paper smoke, security negatives, keyboard/responsive and rollback/parity checks. This documentation task runs none of those suites.

## 8. T/W Crosswalk & Future Boundary

| Existing closure task | Relationship to proposed W work |
|---|---|
| T1 / T2a | Recorded complete at the base; retain perimeter and raw/envelope compatibility, not reopen or replace them. |
| T2b | Complete in `43d85bb`: snapshot fidelity, continued aggregation after broadcast lag and sticky uncertainty; no producer-authoritative resync source. W2 consumes its existing behavior and acceptance evidence. |
| T3 | Pending: owns real paper-session process wiring; W2 consumes it, W3 uses the resulting source. |
| T4–T6 | Separate Spot/Futures gateway tasks; T6 private account events cover standard Spot and USDⓈ-M Futures, **not prerequisites for read-only paper views**. No dashboard layout proves live execution readiness. |
| T7 | Owns milestone documentation/full closure and browser evidence; W6 intersects and coordinates, rather than declaring T7 complete. |

W3 depends on W1/W2; W4 can proceed independently of paper runtime; W5 depends on W4; W6 integrates W1–W5. Historical roadmap checkmarks are not evidence that later T closure gaps have disappeared.

**Remaining technical gates:** shared-package ownership/release flow, actual source/report/replay schema coverage, versioned accounting/metric formulas and annualization, report-defined ruin threshold/method, numeric per-source heartbeat thresholds, reconnect/attribution evidence and contrast/accessibility verification. Section 2's confirmed product decisions are settled; implementation acceptance is not. Live metrics must remain unavailable or explicitly stale until a real supported source exists. Job execution, trading controls, broader exchange parity and new product implementations are future work outside this contract.
