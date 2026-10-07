# Web Presentation & Real-Time Telemetry Dashboard Specification

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
   - Every telemetry event, order fill, and bar payload explicitly carries `symbol` and `strategy_id`.
   - The dashboard supports filtering by specific strategy or inspecting the aggregated multi-asset portfolio.

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
| `GET` | `/api/health` | Healthcheck and uptime probe | `{"status": "ok", "uptime_secs": u64, "version": str}` |
| `GET` | `/api/state` | Current paper/live session state | Active positions, cash balance, equity, active symbol, strategy |
| `GET` | `/api/strategies` | Catalog of configured strategies | List of strategies with allocated capital and status (`ACTIVE`/`PAUSED`) |
| `GET` | `/api/reports` | Index of historical backtests & HPOs | List of filenames, strategy, symbol, date, Sharpe, and Sortino |
| `GET` | `/api/reports/:id` | Full quantitative telemetry report | Complete JSON manifest (equity curve, trade list, Monte Carlo metrics) |

### 3.2 WebSocket Streaming (`/ws/telemetry`)

The WebSocket endpoint upgrades client connections and pushes real-time events published to the `tokio::sync::broadcast` bus.

#### Event Envelope Schema
```json
{
  "timestamp": 1704067200000,
  "strategy_id": "cusum_breakout_v1",
  "symbol": "BTCUSDT",
  "event_type": "DollarBarFinalized",
  "payload": { ... }
}
```

#### Event Types
1. **`DollarBarFinalized`**: Emitted when a Dollar Bar hits its threshold.
   - `bar_id`, `open`, `high`, `low`, `close`, `volume`, `dollar_volume`, `trades_count`.
2. **`SignalTriggered`**: Emitted when CUSUM or Z-Score triggers an order intent.
   - `indicator`: `"CUSUM"` | `"Z_SCORE"`, `direction`: `"Long"` | `"Short"`, `threshold`, `value`.
3. **`OrderFilled`**: Emitted upon order state transition to `Filled` or `PartiallyFilled`.
   - `order_id`, `side`, `price`, `quantity`, `fee`, `slippage`.
4. **`MarkToMarketUpdate`**: Emitted on trade ticks to report live unrealized PnL.
   - `mark_price`, `unrealized_pnl`, `realized_pnl`, `current_equity`, `drawdown_pct`.
5. **`PositionLiquidated`**: Emitted when a Triple Barrier (SL/TP/Time) or manual session stop closes a position.
   - `exit_reason`: `"StopLoss"` | `"TakeProfit"` | `"TimeBarrier"` | `"SessionFinish"`, `exit_price`, `net_pnl`.

---

## 4. UI Dashboard Architecture & Visual Hierarchy

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
  - Displays finalized [`DollarBar`](file:///home/f92esmup/Projects/ATSNT/crates/domain/src/aggregator.rs) candles.
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

This perimeter change covers closure task T1 only. Timestamp/strategy/symbol
telemetry and real paper-session wiring remain separate tasks; it does not declare
Milestone 5 complete.

---

## 6. Implementation Checklist

- [ ] Add `axum`, `tower-http`, `tokio-stream`, and WebSocket dependencies to [`crates/web/Cargo.toml`](file:///home/f92esmup/Projects/ATSNT/crates/web/Cargo.toml).
- [ ] Implement REST handlers in `crates/web/src/handlers/` (`health`, `reports`, `state`).
- [ ] Implement WebSocket upgrade and broadcast subscription in `crates/web/src/ws.rs`.
- [ ] Create frontend static assets in `crates/web/static/` (`index.html`, `app.js`, `styles.css`).
- [ ] Embed or serve Lightweight Charts from local static asset directory for 100% offline capability.
- [ ] Connect `PaperTradingSession` broadcast sender to Axum application state.
- [ ] Write integration test validating REST endpoints and WebSocket message flow.
