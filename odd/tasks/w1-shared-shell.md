# W1 — Shared Shell & Design System

> [!NOTE]
> **Status: Decommissioned & Archived (Milestone 5)**
> As part of **Milestone 5 ("Web Decommission & Decoupled Cloud-First Analytics")**, the in-process web application (`crates/web`, W0–W6) was retired. Read-only visualization and BI reporting are now fulfilled via **Google Cloud BigQuery (`atsnt_bi`) + Looker Studio**, and real-time alerts via **Telegram Bot**. This task specification is retained solely for historical audit.

Branch: `main` | Status: Archived
Document Version: 1.0.0
Authoritative Architecture: [`docs/architecture/09-read-only-web-workspace.md`](../../docs/architecture/09-read-only-web-workspace.md)
Prerequisite Gates: [`odd/tasks/w0-decision-gates.md`](w0-decision-gates.md)

---

## 1. Goal & Objectives
The goal of **W1** is to author the reusable, accessible navigation shell and foundational design system for the **Read-Only Web Workspace**, consuming the contracts and tokens established in **W0**.

W1 establishes:
1. A **Dual-Rail Navigation Shell**:
   - A compact **Global Product Rail** (Portal / Trading / Econoweb) with 64px collapsed width (expandable to 208px via toggle).
   - A persistent **Trading Module Rail** (224px) providing access to the 6 analytical sections (Overview, Operations, Laboratory, Strategies, Data & Market, System).
2. A **Header & Context Strip** (56px) featuring:
   - Binance wallet selector (**Spot Standard** vs **USDⓈ-M Futures Isolated USDT**).
   - Analytical **Dual Clock** (primary financial UTC + local browser time).
   - Mode indicators (persistent **DEMO** badge for `--mock` vs **PAPER TRADING** for `--paper`).
   - Bilingual toggle (Spanish / English).
3. A **Modular Vanilla CSS Architecture**:
   - `css/tokens.css`: Core design tokens validated for WCAG 2.2 AA contrast.
   - `css/shell.css`: Shell layouts, dual rails, header, and content regions.
   - `css/components.css`: Metric cards, badges, buttons, breadcrumbs, data tables.
4. An **Accessible Mobile Drawer** (<768px):
   - Sliding modal drawer with keyboard navigation, focus trap, and `Escape` key dismissal.
5. **Zero-Regression Compatibility**:
   - Existing charts and live telemetry streaming continue operating without disruption under the Operations view.
   - `test_static_asset_serving()` and all workspace integration suites pass.

---

## 2. Specifications & Acceptance

### S1 — Centralized Design Tokens (`css/tokens.css`)
- **Tokens Implemented:**
  - Backgrounds: `--bg-base` (`#0B1120`), `--bg-surface` (`#111827`), `--bg-elevated` (`#1E293B`).
  - Typography & Text: `--text-primary` (`#F1F5F9`, 17.19:1 ratio AAA), `--text-secondary` (`#94A3B8`, 6.92:1 ratio AA).
  - Accents & Semantics: `--accent-blue` (`#3B82F6`), `--positive-green` (`#22C55E`), `--negative-red` (`#F87171`), `--warning-amber` (`#FBBF24`).
  - Spacing scale: 4px, 8px, 12px, 16px, 20px, 24px, 32px.
  - Radii: 6px controls, 8px panels, 9999px pills.
- **Accessibility:**
  - `@media (prefers-reduced-motion: reduce)` resets layout and transition animations to 0ms.
  - Interactive focus rings use `--border-focus` (`#3B82F6` with 4.82:1 ratio, exceeding WCAG 2.1/2.2 Non-text Contrast 3.0:1).

### S2 — Dual-Rail Navigation & Responsive Drawer (`css/shell.css`, `js/shell.js`)
- **Global Rail:**
  - 64px compact icon view with pure CSS tooltips.
  - Expandable to 208px via `#btn-toggle-rail` with state persistence in `localStorage`.
  - Links clearly identify Portal and Econoweb as external applications.
- **Module Rail:**
  - Distinct navigation for Overview, Operations, Laboratory, Strategies, Data & Market, and System.
  - Active module highlighted with blue accent border and pill styling.
- **Mobile Drawer (<768px):**
  - Triggered via `#btn-open-drawer`.
  - Accessible dialog (`role="dialog"`, `aria-modal="true"`) with backdrop blur.
  - Dismissible via `#btn-close-drawer`, clicking backdrop, or pressing the `Escape` key.
  - Focus returns to the opening button upon closing.

### S3 — Header, Context Strip, Dual Clock & i18n (`js/i18n.js`)
- **Context Strip:**
  - `#wallet-select` provides explicit toggle between `spot` and `futures`.
  - Emits `atsnt:wallet-changed` custom event for downstream view consumption.
- **Dual Clock:**
  - Real-time updates every 1,000ms.
  - UTC clock formatted as `HH:MM:SS` UTC.
  - Local clock formatted as `HH:MM:SS` with automatic browser timezone name resolution.
- **Internationalization (`i18n.js`):**
  - Spanish dictionary (default) and English dictionary.
  - Attribute-based DOM updater (`data-i18n`, `data-i18n-tooltip`).
  - `#btn-lang-toggle` switches locale and persists selection in `localStorage`.

### S4 — Content Modularization & Backward Compatibility (`index.html`, `styles.css`)
- **Semantic Structure:**
  - `<div class="app-shell">` wraps `<aside>`, `<nav>`, `<div class="workspace-main">`.
  - Content divided into `<section class="view-section">` containers for modular views.
  - `#view-operations` hosts the existing live TradingView Lightweight Charts, active position cards, order fills table, and streaming telemetry journal.
- **Styles Entrypoint:**
  - `styles.css` imports `tokens.css`, `shell.css`, and `components.css`.
  - Variables aliased to maintain 100% compatibility with earlier chart classes.

---

## 3. Verification Evidence

- [x] **Workspace Test Suite:** `cargo test --workspace --offline` passed all 162 tests, including `test_static_asset_serving()`.
- [x] **Code Quality:** `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --offline -- -D warnings` passed with 0 warnings.
- [x] **Whitespace & Diff Integrity:** `git diff --check` reported 0 trailing whitespace errors.
- [x] **Server Smoke Verification:**
  - In loopback `--mock` mode (port 3100): `atsnt-web` served the updated `index.html` (29,156 bytes) and all static assets over HTTP 200, accepted WebSocket telemetry upgrades, and shut down gracefully with exit code 0.
  - In loopback `--paper` mode (port 3101): `atsnt-web` served `index.html` over HTTP 200, accepted WebSocket connections, and cleanly joined the paper session upon termination with exit code 0.
