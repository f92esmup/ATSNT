# Live Execution Gateway & Pre-Trade Risk Architecture Specification

## 1. Architectural Philosophy: The Real Capital Boundary

The Live Execution Gateway (`crates/adapters`) represents the final frontier: transitioning algorithmic strategies from simulated paper trading to **live financial execution** with real exchange capital.

```text
┌────────────────────────────────────────────────────────────────────────┐
│                         Core Strategy Engine                           │
│                                                                        │
│   ┌─────────────────────────┐          ┌───────────────────────────┐   │
│   │ DollarBarsCusumStrategy │          │   DollarBarAggregator     │   │
│   └────────────┬────────────┘          └─────────────▲─────────────┘   │
│                │ OrderIntent                         │ aggTrade ticks  │
│                ▼                                     │                 │
│   ┌──────────────────────────────────────────────────┴─────────────┐   │
│   │                    Pre-Trade Risk Manager                      │   │
│   │   - Single Order Notional Limit    - Circuit Breaker (Max DD)  │   │
│   │   - Position Exposure Cap          - Zero-Float Decimal Check  │   │
│   └────────────────────────────┬───────────────────────────────────┘   │
└────────────────────────────────┼───────────────────────────────────────┘
                                 │ Authorized Order
                                 ▼
┌────────────────────────────────────────────────────────────────────────┐
│                     Binance Execution Gateway                          │
│                                                                        │
│   ┌───────────────────────────┐      ┌─────────────────────────────┐   │
│   │       BinanceAuth         │      │      REST Order API         │   │
│   │   (HMAC-SHA256 Signer)    │─────►│  (POST /v3/order, balances) │   │
│   └───────────────────────────┘      └──────────────┬──────────────┘   │
│                                                     │                  │
│   ┌───────────────────────────┐      ┌──────────────▼──────────────┐   │
│   │   BinanceUserDataStream   │◄─────┤   WebSocket Execution Stream│   │
│   │   (ListenKey Reconciler)  │      │  (Fills, cancels, fees)     │   │
│   └───────────────────────────┘      └─────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────────┘
```

### Safety Guarantees

1. **Pre-Trade Gatekeeping (The Circuit Breaker)**:
   - No order reaches network sockets without clearing the pure domain [`RiskPolicy`](file:///home/f92esmup/Projects/ATSNT/crates/domain/src/risk.rs).
   - If account drawdown breaches `max_daily_drawdown_pct`, the engine hard-halts all order dispatch.
   - If order size exceeds `max_order_notional` or position exposure exceeds `max_position_notional`, the order is rejected in-memory in zero microseconds.

2. **Cryptographic Integrity**:
   - Every private request is timestamped and signed with hex-encoded **HMAC-SHA256** using `ring` (the audited standard in Rust cryptography).
   - Signatures are strictly validated against official Binance API test vectors.

3. **Zero Floating-Point Financial Arithmetic**:
   - All order prices, quantities, balances, and commission fees strictly use fixed-point `rust_decimal::Decimal`.

4. **Bi-directional Reconciliation**:
   - REST endpoints submit and cancel orders.
   - The WebSocket User Data Stream reconciles fills, partial fills, slippage, and commissions in real time.

---

## 2. Environments: Testnet vs Production

The gateway seamlessly switches between testnet and production with a single configuration flag:

| Parameter | Binance Testnet (Default / Staging) | Binance Production (Live Capital) |
| :--- | :--- | :--- |
| **Capital Risk** | **0 € (Free virtual funds)** | Real exchange capital |
| **KYC Requirement** | None (Sign in with GitHub) | Full Identity Verification |
| **Spot Base URL** | `https://testnet.binance.vision` | `https://api.binance.com` |
| **Futures Base URL** | `https://testnet.binancefuture.com` | `https://fapi.binance.com` |
| **Spot WS Stream** | `wss://testnet.binance.vision/ws/{listenKey}` | `wss://stream.binance.com:9443/ws/{listenKey}` |
| **Futures WS Stream** | `wss://stream.binancefuture.com/ws/{listenKey}` | `wss://fstream.binance.com/ws/{listenKey}` |

---

## 3. Component Reference

### 3.1 Cryptographic Signer (`BinanceAuth`)
- Located at [`crates/adapters/src/binance_auth.rs`](file:///home/f92esmup/Projects/ATSNT/crates/adapters/src/binance_auth.rs).
- Implements `sign(payload: &str) -> String` and `sign_query(query: &str, recv_window: Option<u64>) -> String`.
- Appends current Unix millisecond timestamp and calculates HMAC-SHA256 tag.

### 3.2 Pre-Trade Risk Policy (`RiskPolicy`)
- Located at [`crates/domain/src/risk.rs`](file:///home/f92esmup/Projects/ATSNT/crates/domain/src/risk.rs).
- Pure business rule with zero I/O or network dependencies.
- Enforces:
  - `max_order_notional`: Hard ceiling on a single order (e.g. $10,000 USDT).
  - `max_position_notional`: Aggregate account exposure limit (e.g. $50,000 USDT).
  - `max_daily_drawdown_pct`: Circuit breaker halting execution upon drawdown breach (e.g. 5%).

### 3.3 Execution Gateway (`BinanceGateway`)
- Located at [`crates/adapters/src/binance_gateway.rs`](file:///home/f92esmup/Projects/ATSNT/crates/adapters/src/binance_gateway.rs).
- Methods:
  - `fetch_balance(asset: &str) -> Result<Decimal, GatewayError>`: Queries available balance.
  - `place_order(symbol, intent, quantity, current_pos, drawdown) -> Result<OrderExecutionReport, GatewayError>`: Runs pre-trade risk checks and submits authenticated order.
  - `cancel_order(symbol, order_id) -> Result<OrderExecutionReport, GatewayError>`: Cancels an active exchange order.
  - `create_listen_key() -> Result<String, GatewayError>`: Creates a User Data Stream session key.
  - `keep_alive_listen_key(listen_key) -> Result<(), GatewayError>`: Pings session key every 30 minutes.

### 3.4 User Data Stream Listener (`BinanceUserDataStream`)
- Located at [`crates/adapters/src/binance_user_stream.rs`](file:///home/f92esmup/Projects/ATSNT/crates/adapters/src/binance_user_stream.rs).
- Connects to WebSocket using `listenKey`.
- Asynchronously processes `executionReport` events into normalized [`ExecutionUpdate`](file:///home/f92esmup/Projects/ATSNT/crates/adapters/src/binance_user_stream.rs#L16).

---

## 4. Production Deployment & Security Best Practices

When deploying to a remote Linux VPS:

1. **IP Whitelisting**:
   - In the Binance API Management console, bind your API key strictly to the static public IP of your VPS.
   - If an API key is ever leaked, orders cannot be executed from unauthorized IP addresses.
2. **Permission Scope**:
   - Enable **"Enable Spot & Margin Trading"** or **"Enable Futures"**.
   - **NEVER** enable "Enable Withdrawals". The trading engine has zero withdrawal capabilities.
3. **Environment Secrets**:
   - Store credentials in environment variables (`BINANCE_API_KEY`, `BINANCE_SECRET_KEY`) or encrypted secrets manager; never commit keys to Git.
