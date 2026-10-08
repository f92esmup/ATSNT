# W2 — Stabilize Read-Only Data and Report Contracts

> [!NOTE]
> **Status: Decommissioned & Archived (Milestone 5)**
> As part of **Milestone 5 ("Web Decommission & Decoupled Cloud-First Analytics")**, the in-process web application (`crates/web`, W0–W6) was retired. Read-only data contracts are now fulfilled via **Google Cloud BigQuery (`atsnt_bi`) + Looker Studio**, and real-time alerts via **Telegram Bot**. This task specification is retained solely for historical audit.

Branch: `main` | Status: Archived
Document Version: 1.0.0
Authoritative Architecture: [`docs/architecture/09-read-only-web-workspace.md`](../../docs/architecture/09-read-only-web-workspace.md)
Prerequisite Gates: [`odd/tasks/w0-decision-gates.md`](w0-decision-gates.md) | Shell: [`odd/tasks/w1-shared-shell.md`](w1-shared-shell.md)

---

## 1. Goal & Objectives
The goal of **W2** is to stabilize and formalize the versioned **Read Models (DTOs), Data Contracts, and Report Schemas** for the Read-Only Web Workspace, ensuring that data is fully attributable, resilient across gaps, strictly read-only, and non-conflating between Spot and Futures.

W2 achieves:
1. **Authoritative Account Contexts Model (`GET /api/contexts`):**
   - Exposes supported Binance wallet contexts (`spot` vs `usdm_futures`) with explicit margin modes (`cash` vs `isolated`), position modes (`cash` vs `one_way`), and ATSNT's status as sole order writer for Futures.
   - Prevents the frontend from guessing or conflating account modes.
2. **Strategy Market Compatibility (`GET /api/strategies`):**
   - Extends strategy metadata with immutable versioning (`version: "1.0.0"`), declared compatible markets (`compatible_markets: ["usdm_futures"]`), and the strict restriction flag (`is_futures_only: true`), formalizing the T4/T5 rule that `DollarBarsCusum_v1` rejects Spot.
3. **Telemetry & Reconnect Resilience (Consuming T2b/T3):**
   - Retains the 5-field envelope and sticky uncertainty (`stale: true`) across reconnects or broadcaster lag until an authoritative resync snapshot arrives.
   - Discards buffered backlog upon client reconnect so stale history is never rendered as current.
4. **Report Schema Normalization (Consuming W0/Gate 3):**
   - Reconciles Monte Carlo trajectories by normalizing both `fan_chart_curves` and `fan_chart_trajectories` dynamically in report handlers without mutating on-disk report files.
   - Ignores corrupt or malformed report files gracefully with warnings, avoiding server panics.
5. **Strict Read-Only Invariant:**
   - Enforces and automatedly tests that all mutation methods (`POST`, `PUT`, `DELETE`, `PATCH`) to web routes return `405 Method Not Allowed` or `404 Not Found`.

---

## 2. Specifications & Acceptance

### S1 — Account Contexts DTO (`crates/web/src/handlers/contexts.rs`)
- **Model:**
  ```rust
  pub struct AccountContextMetadata {
      pub id: &'static str,
      pub name: &'static str,
      pub base_currency: &'static str,
      pub margin_mode: &'static str,
      pub position_mode: &'static str,
      pub is_supported: bool,
      pub sole_order_writer: bool,
      pub description: &'static str,
  }
  ```
- **Contexts Exposed:**
  - `spot`: Binance Spot Standard (`margin_mode`: "cash", `position_mode`: "cash", `sole_order_writer`: false).
  - `usdm_futures`: Binance USDⓈ-M Futures (`margin_mode`: "isolated", `position_mode`: "one_way", `sole_order_writer`: true).

### S2 — Strategy Market Compatibility Contract (`crates/web/src/handlers/state.rs`)
- **Metadata Extension:**
  - `id`: `"dollar_bars_cusum"`
  - `name`: `"Dollar Bars Symmetric CUSUM Breakout"`
  - `symbol`: `"BTCUSDT"`
  - `status`: `"ACTIVE"`
  - `description`: `"Information-driven volume sampling with symmetric CUSUM filter and Triple Barrier exits"`
  - `version`: `"1.0.0"`
  - `compatible_markets`: `["usdm_futures"]`
  - `is_futures_only`: `true`

### S3 — Monte Carlo Schema Normalization (`crates/web/src/handlers/reports.rs`)
- Automatically aliases `fan_chart_curves` and `fan_chart_trajectories` when serving report JSON.
- Malformed JSON reports log a tracing warning and are safely skipped during catalog indexing.

### S4 — Read-Only Verification Invariant (`crates/web/tests/api_tests.rs`)
- Automated tests verify:
  - `POST /api/state` returns `405 Method Not Allowed`.
  - `POST /api/orders` returns `404 Not Found` / `405 Method Not Allowed`.
  - `DELETE /api/positions` returns `404 Not Found` / `405 Method Not Allowed`.
  - Zero execution affordances exist in web routes.

---

## 3. Verification Evidence

- [x] **Workspace Suite:** `cargo test --workspace --offline` passed all 166 tests (0 failures).
- [x] **Code Quality:** `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --offline -- -D warnings` passed with 0 warnings.
- [x] **Whitespace & Diff Integrity:** `git diff --check` passed cleanly.
- [x] **Smoke Tests:** `scratch/smoke_test.py` validated both `--mock` (port 3100) and `--paper` (port 3101) loopback servers serving `/api/contexts`, `/api/strategies`, `/api/health`, `/api/state`, `/index.html`, and WebSocket upgrades with clean SIGTERM shutdowns.
