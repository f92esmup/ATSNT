# Web Presentation & Real-Time Telemetry Dashboard Specification

**Document boundary:** Sections 3 and 5 retain the API/perimeter facts from historical base snapshot `c7ae8f4107b9214a3c7f4fc20193393b65da617c`, reconciled with T2b completed in `43d85bb` (`fix(web): preserve uncertainty across telemetry gaps`). The visual hierarchy and original checklist are design/history, not proof of implemented capabilities. The [Read-Only Web Workspace contract](09-read-only-web-workspace.md) is canonical for the proposed product/UI and W0–W6 plan; it does not replace this current API contract or the T-task tracker.

## 1. Architectural Philosophy: Air-Gapped Read-Only Telemetry

The Web Presentation crate (`crates/web`) implements an **observable, air-gapped monitoring dashboard** for the algorithmic trading engine.

```text
┌────────────────────────────────────────────────────────────────────────┐
│                          Core Engine (Localhost)                       │
│                                                                        │
│  ┌──────────────────────┐               ┌───────────────────────────┐  │
│  │ PaperTradingSession  │               │   storage/reports/*.json  │  │
│  └──────────┬───────────┘               └─────────────┬─────────────┘  │
│             │ tokio::sync::broadcast                  │ File Reader    │
│             ▼                                         ▼                │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │                    Axum Telemetry Server                         │  │
│  │    (HTTP REST: /api/*  &  WebSocket: /ws/telemetry  & Static)    │  │
│  └──────────────────────────────────┬───────────────────────────────┘  │
└─────────────────────────────────────┼──────────────────────────────────┘
                                      │ Localhost Loopback (:3000)
                                      ▼
                   ┌──────────────────────────────────────┐
                   │    Cloudflare Tunnel (cloudflared)   │
                   └──────────────────┬───────────────────┘
                                      │ Zero Trust Perimeter (2FA / Passkey)
                                      ▼
                   ┌──────────────────────────────────────┐
                   │    Web Browser Client (Dashboard)    │
                   │  - TradingView Lightweight Charts    │
                   │  - Multi-Asset & Multi-Strategy      │
                   │  - Zero Execution / Read-Only        │
                   └──────────────────────────────────────┘
```

### Key Principles

1. **Air-Gapped Telemetry (Zero Mutation)**:
   - The web interface has zero order submission, modification, or cancellation endpoints.
   - It cannot mutate strategy parameters or risk allocations at runtime.
   - The web server is strictly a telemetry consumer, not an execution gateway. Read-only access does not make telemetry public or protect against host compromise.

2. **Multi-Asset & Multi-Strategy Attribution**:
   - Configured telemetry producers attach explicit `symbol` and `strategy_id` to the envelope, not to the raw event payload.
   - Multi-asset filtering and aggregated multi-strategy views remain architectural goals; the current snapshot tracks one producer identity.

3. **Single Self-Contained Deployment**:
   - Axum serves the REST API, WebSocket streams, and static web assets from a single compiled binary (`target/release/web`).
   - Zero Node.js runtime or complex external web servers (Nginx/Apache) required on the host VPS.

4. **Zero-Trust Perimeter Integration**:
   - The server defaults to `127.0.0.1:3000` and rejects non-loopback bind addresses.
   - A lightweight `cloudflared` tunnel forwards authenticated traffic through Cloudflare Access with two-factor authentication (2FA); its browser origin must be explicitly allowed.

---

## 2. Technology Stack & Dependencies

| Layer | Technology | Rationale |
| :--- | :--- | :--- |
| **Backend Framework** | `axum` (0.7+) | High-performance, ergonomic async web framework built on `tokio` and `tower`. |
| **Async Concurrency** | `tokio` | Native workspace runtime; shares non-blocking channel abstractions with `backtest`. |
| **Static Serving** | `tower-http` (`ServeDir`, `Compression`, `Cors`) | Embedded or local static file delivery with Gzip/Brotli compression. |
| **Serialization** | `serde`, `serde_json` | Type-safe JSON serialization preserving financial decimal precision. |
| **Chart Engine** | **Lightweight Charts** (TradingView) | Open-source, ~45 KB canvas-rendered financial chart engine running at 60 FPS. |
| **UI Styling** | **Tailwind CSS** (Modern Dark Theme) | High-contrast, dense terminal aesthetic optimized for quantitative metrics. |
| **Frontend Scripting**| **Vanilla JavaScript (ES Modules)** | Zero build steps or heavy node dependencies; modular, lightweight DOM and WS updates. |

---

## 3. Communication Protocols & API Contracts

### 3.1 REST API Specification

| Method | Endpoint | Description | Response Schema |
| :--- | :--- | :--- | :--- |
| `GET` | `/api/health` | Healthcheck and uptime probe | `status`, `uptime_secs`, `version`, `connected_ws_clients` |
| `GET` | `/api/state` | Current single-producer telemetry snapshot | `TelemetryState` fields listed in Section 3.2; not proof of a connected live session |
| `GET` | `/api/strategies` | Hard-coded strategy catalog | `id`, `name`, `symbol`, `status`, `description`; no allocated-capital or runtime-instance model |
| `GET` | `/api/reports` | Best-effort JSON report index | `filename`, filename-inferred `report_type`, `size_bytes`, `modified_timestamp`, optional `symbol`, `net_profit`, `win_rate`, `sortino_ratio`, `total_trades` |
| `GET` | `/api/reports/:id` | Raw stored report JSON | No normalized guarantee of equity curves, trades or Monte Carlo fields |

### 3.2 WebSocket Streaming (`/ws/telemetry`)

The endpoint performs a real HTTP WebSocket upgrade, sends an `InitialSnapshot`, then forwards serialized envelopes from the `tokio::sync::broadcast` bus as text frames. The T1 Host/Origin policy still applies before upgrade.

#### Event Envelope Schema
Every message has exactly five top-level fields. Lifecycle payloads preserve the raw `PaperTradingEvent` enum's externally tagged JSON:

```json
{
  "timestamp": 1704067200000,
  "strategy_id": "paper-17",
  "symbol": "ETHUSDT",
  "event_type": "BarFormed",
  "payload": {
    "BarFormed": {
      "start_time": 1704067199000,
      "end_time": 1704067200000,
      "open": "100", "high": "101", "low": "100", "close": "101",
      "volume": "1", "dollar_volume": "101", "trade_count": 1
    }
  }
}
```

`timestamp` is Unix milliseconds: paper events use the originating `Trade.timestamp`; all mock events from one synthetic tick share that tick's observed wall-clock timestamp. Neither WebSocket delivery nor snapshot creation invents a new event time. Financial decimals serialize as strings.

#### Emitted Event Types
For lifecycle events, `payload` is `{ "<event_type>": { ... } }`.

| `event_type` | Payload fields / meaning |
| :--- | :--- |
| `BarFormed` | Finalized `DollarBar`: `start_time`, `end_time`, `open`, `high`, `low`, `close`, `volume`, `dollar_volume`, `trade_count`. |
| `SignalGenerated` | `OrderIntent`: `timestamp`, `side` (`Buy`/`Sell`), `price`, `stop_loss`, `take_profit`, `max_bars_hold`; emitted with a newly opened paper position. |
| `PositionOpened` | `side` (`Long`/`Short`), `entry_price`, `quantity`, `stop_loss`, `take_profit`. |
| `PositionClosed` | `exit_reason`, `exit_price`, `net_pnl`, `total_equity`. Paper barrier exits use `StopLoss`, `TakeProfit`, or `TimeBarrier`, with an `Unknown` fallback; `finish()` does not emit a `SessionFinish` event. |
| `MarkToMarket` | `current_price`, `unrealized_pnl`, `total_equity`, `drawdown_pct`; paper ticks emit it while a position is active. |
| `InitialSnapshot` | Untagged `TelemetryState`: `timestamp`, `portfolio_value`, `cash_balance`, `unrealized_pnl`, `active_position`, `active_symbol`, `active_strategy`, `last_price`, `drawdown_pct`, `stale`. |

#### Snapshot completeness and uncertainty (T2b complete in `43d85bb`)
- HTTP state and WebSocket snapshots carry all displayed financial values and full open-position details. The browser restores equity, cash, unrealized PnL, drawdown and position details; a null position clears every position field, and a null last price clears the price display.
- `stale` defaults to false and becomes sticky when the aggregator misses broadcast events. Aggregation continues after lag, retaining last-known values and consuming later events; incremental updates, including newer timestamps, cannot restore certainty.
- An individual lagging WebSocket receiver gets an `InitialSnapshot` with `stale: true` and discards its pre-snapshot buffered backlog before forwarding future events. This client-local gap does not mark the shared state stale for other clients. Raw lifecycle envelopes remain unchanged.
- Shared aggregator uncertainty notifies connected clients independently of later producer events. Each notified client receives a stale snapshot and discards its pre-snapshot backlog; initial/reconnect snapshots retain shared uncertainty. The browser keeps uncertainty sticky, including across reconnects.
- Reconnects, HTTP refreshes and `InitialSnapshot` delivery expose last-known projections, not authoritative recovery. No producer-authoritative resync source exists in T2b; these projections cannot clear uncertainty.
- T2b's Rust regressions cover complete snapshots, flat positions, continued aggregation after gaps and isolated client lag through the socket receiver path. Direct DOM restoration remains for T7 browser E2E; this documentation reconciliation reruns no tests.

#### Identity, snapshots, and compatibility
- `TelemetryConfig { symbol, strategy_id }` explicitly identifies a producer; anonymous trades cannot supply these identities. `AppState::with_telemetry` can set identity before the first event. Without configuration or an observed envelope, identity strings are empty and the timestamp is `null`.
- The state updater retains the latest observed envelope's timestamp and identity. `/api/state` exposes that state; each WebSocket `InitialSnapshot` uses the same latest event timestamp, or `null` before observation, even when identity was preconfigured.
- `PaperTradingConfig` struct literals and the raw `PaperTradingEvent` API remain backward compatible: `new`, `subscribe`, `event_sender`, and `process_trade` retain raw-event behavior. `new_with_telemetry` adds a separate envelope channel via `subscribe_telemetry` (which returns `None` for raw-only sessions). The web application consumes envelopes; its JavaScript reads the existing tagged lifecycle payloads and uses envelope identity/time for attribution, logs, and position markers.
- In `crates/web/tests/api_tests.rs`, `websocket_delivers_configured_paper_event_and_snapshot_envelopes` exercises a loopback HTTP 101 upgrade, text-frame delivery of all five paper event types, exact envelope/payload equality, and latest-time snapshots. `websocket_delivers_mock_metadata_and_observed_tick_timestamp` checks mock identity and tick time against the delivered bar's `end_time`. These are socket delivery tests, not just upgrade-extractor checks; they were inspected, not rerun for this documentation update.

**Runtime boundary (T3 pending):** the web binary does not yet wire a real `PaperTradingSession` into its process. `--mock` is explicit opt-in, demo-only synthetic data (`BTCUSDT` / `SyntheticDemo` by default), not paper-session execution. Without it, the binary starts without a market-event producer; identity remains empty and timestamp `null` until events arrive. The paper-to-web bridge in the contract test does not complete T3 runtime wiring.

---

## 4. UI Dashboard Architecture & Visual Hierarchy

**Original design target, not a current capability inventory.** The current frontend uses live/backtest/HPO tabs, but does not implement every feature below. Its backtest chart currently synthesizes two endpoints rather than using an actual stored series; the Monte Carlo reader expects `fan_chart_curves`, unlike the illustrative `fan_chart_trajectories` schema in document 05. W4 owns evidence-backed reconciliation. T2b is complete in `43d85bb`; W2 consumes its existing snapshot fidelity and sticky-uncertainty behavior, while T3 runtime wiring remains pending; W3 builds the proposed Operations view only after W1/W2. A “LIVE” label in this diagram does not demonstrate a live source.

The frontend is structured around three primary views with a permanent top KPI ribbon:

```text
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ [ATSNT TERMINAL]  ● LIVE  |  Portfolio: $10,450.20 (+4.50%) | Cash: 75% | MaxDD: -2.1% │
├────────────────────────────────────────────────────────────────────────────────────────┤
│ Ticker: [ BTCUSDT ▼ ]  Strategy: [ All Strategies ▼ ]   Views: [ LIVE | BACKTEST | HPO]│
├────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                        │
│  ┌────────────────────────────────────────────────────────┐  ┌──────────────────────┐  │
│  │                                                        │  │ Active Position      │  │
│  │               Lightweight Candlestick Chart            │  │ LONG 0.15 BTC        │  │
│  │                     (Dollar Bars)                      │  │ Entry: $64,200.00    │  │
│  │                                                        │  │ Mark:  $64,850.00    │  │
│  │           ▲ Buy Signal             ▼ Exit (TP)         │  │ uPnL:  +$97.50 (+1%) │  │
│  │                                                        │  │ SL: $63,558 | TP:... │  │
│  └────────────────────────────────────────────────────────┘  └──────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────────────────────────────┐  │
│  │ Real-Time Order Stream & Event Journal (Fills, Signals, Latency, Bar Rollover)   │  │
│  └──────────────────────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

### 4.1 Permanent Top KPI Ribbon
- **Total Portfolio Value**: Sum of cash balance plus current unrealized equity.
- **Portfolio Allocation Bar**: Visual bar showing `% CASH`, `% BTCUSDT`, `% ETHUSDT`.
- **Live Unrealized PnL**: Real-time ticker colored green/red reflecting instant mark price moves.
- **System Status Indicator**: WebSocket connection heartbeat latency indicator.

### 4.2 View 1: Real-Time Live Monitor (`/live`)
- **Interactive Candlestick Chart**:
  - Displays finalized [`DollarBar`](../../crates/domain/src/aggregator.rs) candles.
  - Plots Buy/Sell entry arrows and Stop-Loss/Take-Profit target levels dynamically.
- **Active Position Card**:
  - Displays direction (`LONG` / `SHORT`), average entry price, liquidation barrier levels, and current ROI.
- **Live Event Journal**:
  - High-density scrolling table logging every trade execution, signal event, and system notice.

### 4.3 View 2: Historical Backtest Explorer (`/backtest`)
- **Report Selector**: Dropdown listing all runs found in `storage/reports/`.
- **Equity Curve & Underwater Drawdown Chart**: Dual line chart comparing Strategy vs Buy & Hold.
- **Quantitative Scorecard**:
  - Annualized Return, Sharpe Ratio, Sortino Ratio, Calmar Ratio, Expectancy, Profit Factor, Win Rate.
- **Trade Sequence Breakdown**: Tabular log of historical simulated trades.

### 4.4 View 3: HPO & Monte Carlo Stress-Testing (`/hpo`)
- **Walk-Forward Fold Visualizer**: Train/Test/Embargo temporal partition diagrams.
- **Parameter Stability Plateau**: Visual inspection of parameter neighborhoods ($\pm \delta$).
- **Monte Carlo Fan Chart**:
  - 50-curve bootstrap visualization showing $p_1, p_{50}, p_{99}$ drawdown cones.
  - Probability of Ruin ($P_{\text{ruin}}$) threshold alert card.

---

## 5. Security & Deployment Model

### 5.1 Local browser access

The binary defaults to `127.0.0.1:3000`. `--host` accepts only literal loopback IPs
(e.g. `127.0.0.1` or `::1`), never hostnames, wildcard addresses, or public/private
network interfaces. `--port` accepts 1–65535; port zero is deliberately unsupported
because default browser origins require a stable port.

```sh
cargo run --locked --offline -p web -- --mock
# Open http://localhost:3000 or http://127.0.0.1:3000
```

The default allowed origins are `http://localhost:<port>`,
`http://127.0.0.1:<port>`, and `http://[::1]:<port>`. The selected loopback bind IP
is also allowed. Changing `--port` changes these defaults; it does not leave port
3000 trusted. An IPv6 listener can be selected with `--host ::1` (browse to
`http://[::1]:3000`). These aliases do not cause additional listeners to be bound.

### 5.2 Explicit tunnel origins

Keep the listener on loopback and configure the **browser page's origin**, not a
`ws://`/`wss://` URL or the tunnel's local upstream address:

```sh
cargo run --locked --offline -p web -- --mock \
  --allowed-origin https://dashboard.example.com
# Additional origins require repeated flags, for example:
# --allowed-origin https://dashboard.example.com:8443
```

Configure `cloudflared` to forward to `http://127.0.0.1:3000` and enforce
Cloudflare Access authentication (SSO/OTP plus 2FA) before publishing the hostname.
Public inbound ports on the host remain blocked. The proxy may preserve the
explicitly allowed public Host or rewrite it to the trusted local upstream Host;
it must forward the original browser Origin for WebSocket handshakes. Changing
`--allowed-origin` does not relax the loopback-only bind rule.

Allowed values must contain only an `http` or `https` scheme, a hostname or IP,
and an optional port. Paths (including a trailing `/`), credentials, wildcards,
queries, fragments, whitespace, malformed/out-of-range ports, and opaque `null`
origins are rejected. DNS names use ASCII labels (punycode for IDNs), without a
trailing dot. Scheme/host/effective-port tuples are matched: DNS case and IPv6
notation are normalized, and omitted HTTP 80 / HTTPS 443 equals an explicit
80 / 443. Other ports, subdomains, and schemes are distinct. Invalid configured
origins fail startup before any mock task or listener starts.

### 5.3 HTTP, CORS, and WebSocket policy

- **Host/DNS-rebinding protection on all routes:** REST, static assets, and
  WebSockets require a Host (or HTTP/2 URI authority) matching a local default or
  configured origin authority. Repeated/malformed/untrusted Host values are
  rejected with 403. An absolute URI authority is also checked. Trust is never
  derived from arbitrary incoming Host, `Forwarded`, or `X-Forwarded-*` headers.
  This also protects same-origin REST fetches that omit Origin.
- **Explicit CORS only:** when Origin is present it must be one trusted origin.
  CORS echoes only that permitted origin, allows only GET, and does not grant
  credentialed access or wildcard origins/methods/headers. Rejected requests and
  preflights return 403 without CORS permission.
- **WebSockets require Origin:** `/ws/telemetry` rejects missing, `null`, malformed,
  repeated, or untrusted Origin headers with 403 before the upgrade extractor or
  event subscription. The browser dashboard supplies its page origin naturally.
- **Deliberate native-client policy:** native WebSocket clients must send one
  allowed Origin too; there is no missing-origin exemption. Native HTTP health
  probes and REST clients may omit Origin but still need a trusted Host/authority.
  Origins and Host are browser perimeter controls, not native-client authentication:
  a native client can forge both. Tunnel authentication remains essential.

The library's compatibility `create_router` uses the secure port-3000 localhost
policy. Embedders using another port or a tunnel should construct
`DashboardSecurity::new(listener_address, additional_origins)` and pass it to
`create_router_with_security`. The embedder remains responsible for actually
binding that validated loopback listener.

The perimeter controls above cover closure task T1. Section 3.2 documents the T2
telemetry contract; real paper-session process wiring remains T3 pending. Neither
this documentation update nor the checklist below declares Milestone 5 complete.

---

## 6. Implementation Checklist

This original checklist is retained as historical planning, not current task status. REST/static/WebSocket implementations exist at the base; real paper-session binary wiring remains T3 pending. Use the [closure tracker](../../odd/tasks/milestone-5-6-closure.md) for T-task evidence and [canonical W plan](09-read-only-web-workspace.md#7-independent-w0w6-tasks) for the proposed redesign.

- [ ] Add `axum`, `tower-http`, `tokio-stream`, and WebSocket dependencies to [`crates/web/Cargo.toml`](../../crates/web/Cargo.toml).
- [ ] Implement REST handlers in `crates/web/src/handlers/` (`health`, `reports`, `state`).
- [ ] Implement WebSocket upgrade and broadcast subscription in `crates/web/src/ws.rs`.
- [ ] Create frontend static assets in `crates/web/static/` (`index.html`, `app.js`, `styles.css`).
- [ ] Embed or serve Lightweight Charts from local static asset directory for 100% offline capability.
- [ ] Connect `PaperTradingSession` broadcast sender to Axum application state.
- [ ] Write integration test validating REST endpoints and WebSocket message flow.
