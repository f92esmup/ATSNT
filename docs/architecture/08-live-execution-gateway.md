# Live Execution Gateway & Pre-Trade Risk Architecture Specification

## 1. Architectural Philosophy: The Real Capital Boundary

**Monitoring & Reporting boundary:** Telemetry and post-trade analytics are decoupled from live execution and streamed to Google Cloud BigQuery for Looker Studio visualization, while critical execution and risk events trigger real-time mobile alerts via Telegram Bot. Live values must be unavailable or explicitly stale until a real supported source exists. This cross-link does not change the gateway profile, guarantees or task status; the [closure tracker](../../odd/tasks/milestone-5-6-closure.md) owns that evidence.

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
   - No order reaches network sockets without clearing the pure domain [`RiskPolicy`](../../crates/domain/src/risk.rs).
   - If account drawdown breaches `max_daily_drawdown_pct`, the engine hard-halts all order dispatch.
   - Sizing is clamped to `max_leverage`, floored to the lot step and rejected below the minimum lot or notional (`OrderBelowMinLotSize`, `OrderBelowMinNotional`). The gateway applies the symbol's real `LOT_SIZE`/`MIN_NOTIONAL` filters (futures) or 1x with the Spot filters.
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
- Located at [`crates/adapters/src/binance_auth.rs`](../../crates/adapters/src/binance_auth.rs).
- Implements `sign(payload: &str) -> String` and `sign_query(query: &str, recv_window: Option<u64>) -> String`.
- Appends current Unix millisecond timestamp and calculates HMAC-SHA256 tag.

### 3.2 Pre-Trade Risk Policy (`RiskPolicy`)
- Located at [`crates/domain/src/risk.rs`](../../crates/domain/src/risk.rs).
- Pure business rule with zero I/O or network dependencies.
- Enforces:
  - `max_order_notional`: Hard ceiling on a single order (e.g. $10,000 USDT).
  - `max_position_notional`: Aggregate account exposure limit (e.g. $50,000 USDT).
  - `max_daily_drawdown_pct`: Circuit breaker halting execution upon drawdown breach (e.g. 5%).
  - `max_leverage`, `min_order_quantity`, `quantity_step_size`, `min_order_notional`: Binance-parity sizing rules (defaults 20x, 0.001, 0.001, 5 USDT), overridden per symbol by the exchange filters.
- **Source of the limits**: the `live_trading` runner builds the policy and `risk_per_trade_pct` from the deployment artifact's `execution_context` (`BacktestConfig::risk_policy()`, the same call the simulator uses), so live gates orders like the simulation that selected the strategy. `--risk-pct` is an explicit override; without an artifact the defaults apply.
- **Drawdown reference**: the breaker measures loss against the marked-equity peak of the current UTC day (`DailyWatermark`), resetting at 00:00:00 UTC exactly like the simulator.
- **Fees**: live PnL accumulates the commissions Binance reports for each fill (`FeeLedger`); the configured taker fee is only a fallback.

### 3.3 Execution Gateway (`BinanceGateway`)
- Located at [`crates/adapters/src/binance_gateway.rs`](../../crates/adapters/src/binance_gateway.rs).
- Methods:
  - `fetch_balance(asset: &str) -> Result<Decimal, GatewayError>`: Queries available balance.
  - `place_order(symbol, intent, quantity, current_pos, drawdown) -> Result<OrderExecutionReport, GatewayError>`: Runs pre-trade risk checks and submits authenticated order.
  - `cancel_order(symbol, order_id) -> Result<OrderExecutionReport, GatewayError>`: Cancels an active exchange order.
  - `create_listen_key() -> Result<String, GatewayError>`: Creates a User Data Stream session key.
  - `keep_alive_listen_key(listen_key) -> Result<(), GatewayError>`: Pings session key every 30 minutes.

For standard Spot orders, `place_order` also fails closed before dispatch unless the symbol is an active Spot pair, the account response has valid free balances, and the order is funded (quote asset for buys, base asset for sells). It loads the account's symbol-specific commission rates, accounts for maker/taker plus side and special/tax rates, and checks the configured discount asset when applicable. Missing, malformed, unpriceable, or insufficient preflight data prevents order submission. USD-M Futures retain the existing risk-policy path.

**Entry orders**: USD-M entries are `MARKET` (taker) orders sized at the signal price. A resting limit at the signal bar's close would fill mainly when the signal is wrong, which the simulation cannot reproduce. Binance acknowledges a market order with `price` 0 and may omit `cumQuote`; the fills arrive on the private stream. If an entry ends (cancelled, expired) after a partial fill, the session opens and protects the executed quantity; if it ends without a fill, the session returns to flat. Spot entries keep their limit order.

### 3.3.1 Exchange-Held Exits and Emergency Close
The live session keeps its exits on the exchange so they survive a crash of the bot:

- After the entry order is `FILLED`, `BinanceGateway::place_protective_exits` places a `STOP_MARKET` and a `TAKE_PROFIT_MARKET` conditional order through `POST /fapi/v1/algoOrder` (`algoType=CONDITIONAL`, `closePosition=true`, `workingType=MARK_PRICE`, opposite side to the entry). The legacy `/fapi/v1/order` rejects these types. Trigger prices are aligned to the symbol tick toward the entry price.
- `ExitManager` ([`crates/backtest/src/live_exits.rs`](../../crates/backtest/src/live_exits.rs)) owns the lifecycle with bounded retries (3 immediate attempts, then retried on the next bar or tick):
  - If either protective order cannot be placed, the position is closed at market (`MARKET`, `reduceOnly`) and all orders cancelled; a critical alert is raised. A position is never left unprotected silently.
  - When the exchange closes the position (SL or TP fill), `cancel_exit_orders` removes the leftover orders (Binance Futures has no OCO). The entry preflight rejects when conditional orders exist, so cleanup is mandatory.
  - Time barrier: not expressible on the exchange, so the bot closes at market after `max_bars_hold` bars (close first, cancel afterwards). Only this barrier depends on the process being alive; SL and TP do not.
  - Monthly kill-switch (`MonthlyGuard`): `Running -> Halting -> Halted`. The flatten sequence is close, cancel, then re-read the position to confirm flat (a pending entry that fills in between is closed too). It reports `Halted` only after the exchange confirms a flat position; otherwise it stays `Halting`, alerts and retries every 5 s.
- The private stream accepts `ALGO_UPDATE` events by event time only; the resulting fills arrive as regular `ORDER_TRADE_UPDATE` events.

Known limits: there is no reconciliation on restart (SL/TP stay on the exchange, but a new process has no tracker or time barrier and could open another position, so check the account before restarting); a fill without its order intent triggers an emergency halt that lasts until restart; an entry that stays `NEW` without any execution report (a silent stream) has no timeout; and an actual SL/TP trigger fill (as opposed to the bot's own market close) has not been observed on the exchange.

**Testnet validation (executed 2026-10-16 on Binance USD-M testnet with `testnet_exit_check`)**: 19/19 checks passed against the real exchange.
- SL and TP are accepted as `closePosition=true` algo orders (2 pending after protection); the time-barrier close leaves nothing behind; cleanup after a close leaves no algo orders; the emergency halt reports `Halted` only with `positionAmt = 0` and zero orders; a stop that would trigger immediately is rejected with `-2021` and the position is closed rather than left naked.
- Crash survival: after the process exited with no cleanup, the position and both algo orders were still on the exchange.
- `closePosition` algo orders switch to `EXPIRED` by themselves once the account is flat, so the explicit cancel is a safety net.
- Real events observed: `ALGO_UPDATE` (statuses `NEW` and `EXPIRED`, with `E`), `TRADE_LITE` and `ORDER_TRADE_UPDATE`. `TRADE_LITE` was unknown to the parser and **closed the stream at the first fill** before this fix; it is now an explicit ignorable event. Real payloads are kept as fixtures in `crates/adapters/tests/fixtures/binance_private_events/real_testnet_*.json`.
- `/fapi/v3/account` does not return `canTrade`; the entry preflight used to reject every account for that reason and now rejects only when the field is present and false.
- The default testnet account is cross-margin and holds non-USDT collateral. The entry preflight and the private-stream parser refuse that profile **by design** (isolated, one-way, USDT-only). The harness therefore opens its own `MARKET` entries and tests the exits through the real gateway and `ExitManager`. A real bot run needs an isolated, USDT-only account.

Harness: `GCP_PROJECT_ID=<project> cargo run -p backtest --bin testnet_exit_check -- scenario|leave-open|inspect|cleanup|capture-events`. It only targets testnet.

### 3.3.2 Account Profile Verification
Before any stream is opened, `live_trading` calls `BinanceGateway::verify_account` and prints a report (`[OK]`, `[FIXED] from -> to`, `[BLOCKING] reason. To fix: ...`). It checks one-way position mode, single-asset (USDT) mode, USDT-only collateral, a positive USDT balance, auto-add margin off, isolated margin on the symbol, a flat account (no position, regular or conditional orders) and **leverage viability**: the exchange bracket must allow the leverage the largest order may need. Any blocking finding stops the bot with an error that says what to change.

- **The only automatic change** is the symbol margin type to `ISOLATED` (`POST /fapi/v1/marginType`), and only when the account is flat with no orders. The result is re-read before it is reported as fixed; `-4046` ("no need to change") is not trusted unless the re-read agrees, and `-4047`/`-4048` (orders or position exist) stay blocking.
- **Leverage is never set.** It is an output of risk sizing: the per-order preflight keeps choosing the minimum sufficient leverage for each operation.
- **Re-verification** runs every 10 minutes and right after a listen-key expiry. While a position or order exists it is read-only; on drift it raises a critical alert (Telegram) and blocks new entries until the account matches again.
- **Flags**: `--verify-only` prints the report and exits without changing anything; `--no-auto-fix` verifies and stops on any mismatch.
- Manual check against the testnet: `testnet_exit_check verify-account`. Validated read-only on 2026-10-16 against the real Binance USD-M testnet: all eight endpoints parse, and the report flags the default testnet account correctly (non-USDT collateral and cross margin). The `marginType` write itself has only been exercised against a simulated exchange; confirm it on an account that has only USDT before relying on it.

### 3.4 User Data Stream Listener (`BinanceUserDataStream`)
- Located at [`crates/adapters/src/binance_user_stream.rs`](../../crates/adapters/src/binance_user_stream.rs).
- Connects to WebSocket using `listenKey`.
- Asynchronously processes `executionReport` events into normalized [`ExecutionUpdate`](../../crates/adapters/src/binance_user_stream.rs#L16).

---

### 3.5 Trading Session and Execution Venue (`TradingSession`, `ExecutionVenue`)
- Located at [`crates/backtest/src/session.rs`](../../crates/backtest/src/session.rs); the contract is [`ports::ExecutionVenue`](../../crates/ports/src/execution.rs).
- The session is the venue-independent lifecycle: strategy signal -> gates (account profile, monthly breaker, emergency halt, pending entry or exits) -> entry through the venue -> exchange-held exits on the fill -> time barrier -> accounting of the closed trade. It performs no I/O: callers receive `SessionEvent`s to log, alert and persist.
- Inputs are `on_trade`, `on_bar` and `on_execution` (an `ExecutionReport` per fill). Partial fills price the position at their volume-weighted average; commissions come from the reports (`FeeLedger`); the daily breaker reference is the UTC-day peak (`DailyWatermark`); trades carry the same exit labels as the simulator (`StopLoss`, `TakeProfit`, `TimeBarrier`, `MonthlyCircuitBreaker`) and the measured slippage versus the signal and trigger prices.
- A venue implements `ExecutionVenue` (which extends `ExitGateway`). `BinanceGateway` is the live venue and the `live_trading` runner drives the session with it (trades, bars and private-stream fills in; `EventSink` turns the events into console output, Telegram alerts and BigQuery rows); a simulated venue will plug into the same session so backtest, paper and live share one lifecycle. The unit tests drive the session with a fake venue and no network.

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
