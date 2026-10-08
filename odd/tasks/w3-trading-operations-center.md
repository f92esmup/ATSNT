# W3 — Trading Operations Center

> [!NOTE]
> **Status: Decommissioned & Archived (Milestone 5)**
> As part of **Milestone 5 ("Web Decommission & Decoupled Cloud-First Analytics")**, the in-process web application (`crates/web`, W0–W6) was retired. Real-time observational views are now delivered via **Google Cloud BigQuery (`atsnt_bi`) + Looker Studio**, with immediate alerts dispatched to **Telegram Bot**. This task specification is retained solely for historical audit.

Branch: `main` | Status: Archived
Document Version: 1.0.0
Authoritative Architecture: [`docs/architecture/09-read-only-web-workspace.md`](../../docs/architecture/09-read-only-web-workspace.md)
Prerequisite Gates: [`odd/tasks/w0-decision-gates.md`](w0-decision-gates.md) | Shell: [`odd/tasks/w1-shared-shell.md`](w1-shared-shell.md) | Contracts: [`odd/tasks/w2-read-only-contracts.md`](w2-read-only-contracts.md)

---

## 1. Goal & Objectives
The goal of **W3** is to implement the real-time **Trading Operations Center** observation view of the Read-Only Web Workspace (Section 4 and Section 7 of [`docs/architecture/09-read-only-web-workspace.md`](../../docs/architecture/09-read-only-web-workspace.md)), fulfilling all observational contracts, non-duplicating wallet attribution, formula-backed metrics, irregular Dollar Bar visualization, and strict read-only operation journals.

W3 achieves:
1. **Selected-Wallet & Environment Attribution (Deliverable 1):**
   - Seamless, reactive context switching between **Binance Spot** and **USDⓈ-M Futures**.
   - Under Spot: Derivatives metrics (Isolated Margin, Effective Leverage, Funding Rate) are explicitly displayed as `N/A (Spot Cash)` rather than false zeros. `DollarBarsCusum_v1` is explicitly badged as incompatible with Spot.
   - Under Futures: Shows isolated USDT margin, 1x leverage, and perpetual funding rate (+0.0100% / 8h).
2. **State, Mode & Freshness Separation (Deliverable 2):**
   - Distinct, persistent **DEMO / MOCK** warning banner displayed when running with `--mock`, ensuring in-memory synthetic balances are never mistaken for live exchange balances.
   - Separate indicators for Transport status (WebSocket), Engine status, and Telemetry freshness with elapsed-time counters.
   - Highlights that Dollar Bars have irregular volume durations and lack of a new bar does not imply stale data.
3. **Formula-Backed Metric Cards (Deliverable 2):**
   - Accessible info buttons and tooltips detailing mathematical formulas and valuation semantics:
     - Portfolio Equity ($Equity = Cash + uPnL$)
     - Unrealized PnL ($uPnL = (P_{mark} - P_{entry}) \times Qty$)
     - Realized Session PnL ($\sum Net\ PnL$)
     - Session Drawdown ($(HWM - Equity) / HWM \times 100\%$) with a 5.00% Circuit Breaker monitor
     - Capital at Risk ($|P_{entry} - P_{SL}| \times Qty$) with 1% portfolio risk cap
4. **Sourced Dollar Bars with Irregular Durations (Deliverable 3):**
   - Candlestick series rendering finalized Dollar Bars directly from engine events (`BarFormed`), with zero client-side tick aggregation.
   - Header displaying dollar threshold ($50,000 USDT) and irregular duration ($\Delta t = end\_time - start\_time$).
   - Rich chart markers for CUSUM signals (`SignalGenerated`) and simulated fills (`PositionOpened` / `PositionClosed`).
5. **Tabbed Operational Journal with Time Filters (Deliverable 4):**
   - Sub-panels with tab navigation:
     - **Active Position:** Detailed open position breakdown with barrier lines (Entry, Mark, SL, TP, uPnL, Margin).
     - **Live Orders:** Open/pending orders (SL and TP barriers) with zero cancel buttons.
     - **Fills & Executions:** Historical fills with simulated drag and fees.
     - **Closed Trades:** Completed round-trips with realized PnL and exit reasons (`StopLoss`, `TakeProfit`, `TimeBarrier`).
     - **Telemetry Journal:** Streaming log of raw engine events.
   - Time filters: `Todo (All)`, `1h`, `24h`, and `Sesión Actual (Session)`.
6. **Strict Read-Only Guarantee:**
   - 100% observational: zero order submission forms, cancel buttons, liquidation triggers, or mutation routes.

---

## 2. Specifications & Acceptance

### S1 — Risk & Session Policy Ribbon
- Displays persistent immutable policy constraints:
  - Max Notional per Order: **10,000 USDT**
  - Max Notional per Position: **50,000 USDT**
  - Max Risk per Trade: **1.00%**
  - Session Drawdown Limit: **5.00% HWM** (no daily reset, persisted across restarts until explicit close)
  - Circuit Breaker Badge: `LÍMITES ACTIVOS` (or `CIRCUIT BREAKER ACTIVADO` if Drawdown reaches 5%).

### S2 — Backend Telemetry State & Health (`crates/web`)
- Enriched `TelemetryState` with:
  - `pub execution_mode: String`
  - `pub recent_closed_trades: Vec<ClosedTradeSummary>` (bounded at 50)
  - `pub recent_fills: Vec<FillSummary>` (bounded at 50)
- Enriched `HealthResponse` with:
  - `pub execution_mode: &'static str` (`"mock"`, `"paper"`, or `"idle"`)
- `AppState` methods:
  - `with_execution_mode(mode: &'static str)`
  - Background listener records fills and closed trades on `PositionOpened` and `PositionClosed`.

### S3 — Frontend Operations Center (`crates/web/static`)
- `index.html`: Fully restructured `#view-operations` with DEMO banner, policy ribbon, 8 formula-backed metric cards, Dollar Bars header, side position/strategy column, and 5 operational journal tabs with time filters.
- `components.css`: Component classes for `.demo-banner`, `.risk-policy-ribbon`, `.operational-nav-bar`, `.op-tab-btn`, `.time-filter-btn`, and `.formula-info-btn`.
- `i18n.js`: Complete bilingual ES/EN translations for all W3 terms.
- `app.js`: Reactive controller managing context changes (`atsnt:wallet-changed`), irregular bar durations, formula calculations, tab/filter switching, and live chart markers.

---

## 3. Verification Evidence

- [x] **Workspace Suite:** `cargo test --workspace --offline` passed all 167 tests (0 failures).
- [x] **API & Integration Tests:** `cargo test -p web --test api_tests` passed all 30 tests, including:
  - `test_health_endpoint` (verifying `execution_mode: "idle"`).
  - `test_telemetry_state_session_fills_and_closed_trades` (verifying session fills and closed trades recording and serialization).
  - `mock_startup_identity_is_configured_before_producer_events`.
  - `test_read_only_negative_mutation_routes`.
- [x] **Code Quality:** `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --offline -- -D warnings` passed with 0 warnings.
- [x] **Whitespace & Diff Integrity:** `git diff --check` passed cleanly with 0 errors.
- [x] **Runtime Smoke Tests:** `verify_w3_runtime.py` executed against both `--mock` (port 3100) and `--paper` (port 3101):
  - Verified `GET /api/health` reports exact `execution_mode` (`mock` vs `paper`).
  - Verified `GET /api/state` reports exact `execution_mode`.
  - Verified static assets (`/index.html`, `/css/tokens.css`, `/css/shell.css`, `/css/components.css`, `/js/i18n.js`, `/js/shell.js`, `/app.js`) return HTTP 200 OK.
  - Verified WebSocket upgrade `/ws/telemetry` receives `InitialSnapshot` with correct `execution_mode` and clean server shutdown.
