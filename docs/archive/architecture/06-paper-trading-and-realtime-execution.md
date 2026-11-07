# Real-Time Paper Trading & Asynchronous Execution Specification

> **Status (2026-10-31):** describes the Binance paper session as it was before the swarm-only refactor; the strategy names here are historical. It is rewritten with the Kraken simulation (WP-15).

## 1. Architectural Philosophy: The Paper Trading Bridge

Paper Trading is the critical intermediate proving ground between **historical discrete-event backtesting** (Milestones 1 & 2) and **authenticated live capital execution** (Milestone 6).

```text
[ Live Market Data Stream ]
   (Binance WebSocket)
            │  aggTrade ticks
            ▼
┌────────────────────────┐
│  DollarBarAggregator   │ ──► Continuous dollar volume accumulation
└────────────────────────┘
            │  Finalized DollarBar
            ▼
┌────────────────────────┐
│  PaperTradingSession   │ ──► On every tick: Mark-to-market PnL
└────────────────────────┘
     │              │
     │ On bar       ▼
     │        ┌─────────────────────────┐
     │        │ DollarBarsCusumStrategy │ ──► CUSUM + Rolling Z-Score signal
     │        └─────────────────────────┘
     │                      │ OrderIntent
     ▼                      ▼
┌────────────────────────────────────────┐
│  TradingSession + SimulatedVenue       │ ──► Latency, tape-walk market fills,
└────────────────────────────────────────┘     taker fees, tape-armed SL/TP exits
     │
     ▼
┌────────────────────────────────────────┐
│ tokio::sync::broadcast (Capacity 10k)  │ ──► PaperTradingEvent Stream
└────────────────────────────────────────┘     (Retired web endpoint)          
```

---

## 2. Ingestion & Port Contracts

### 2.1 Asynchronous Stream Port (`AsyncMarketDataStream`)

While historical readers (`BinanceCsvReader`, `BinanceParquetReader`) implement the synchronous `MarketDataStream` port, live streaming requires asynchronous non-blocking polling:

```rust
pub trait AsyncMarketDataStream {
    type Error;

    /// Fetches the next sequential trade tick from the network stream.
    async fn next_trade(&mut self) -> Result<Option<domain::Trade>, Self::Error>;
}
```

### 2.2 Binance WebSocket Adapter (`BinanceWebSocketStream`)

- Endpoint: `wss://fstream.binance.com/ws/{symbol}@aggTrade` (Futures) or `wss://stream.binance.com:9443/ws/{symbol}@aggTrade` (Spot).
- Strategy compatibility is checked before the stream connects. `DollarBarsCusum_v1` supports USD-M Futures only; `paper_trading --spot` is rejected rather than reinterpreting its signals as Spot orders.
- Deserialization: Strict fixed-point decimal parsing via `rust_decimal::Decimal` (zero floating-point math).
- Aggressor resolution: `is_buyer_maker == true` $\rightarrow$ `Side::Sell`, `false` $\rightarrow$ `Side::Buy`.
- Resilience: Background task with exponential backoff reconnection, periodic Ping/Pong heartbeats, and bounded MPSC backpressure buffer (`capacity: 10,000`).

### 2.3 Decision Scope & Product Gate
- **Decision Models**: The engine unifies execution under `Decision::Entry` (triple-barrier signals with entry price, stop loss, take profit, max bars) and `Decision::Target` (target allocation levels).
- **Product Gate**: Strategies declaring `Strategy::emits_targets() == true` (e.g. `SwarmConsensusStrategy`) are rejected offline by `paper_trading` prior to establishing any WebSocket connection:
  `strategy '<id>' emits target-position decisions, which paper_trading and live_trading do not accept yet`.
- **Venue Abstraction**: Venues implement `ExecutionVenue::submit_order` for explicit market orders with `reduce_only` protection, and `ExitGateway::place_exits` takes `take_profit: Option<Decimal>` allowing single-leg stop exits.

---

## 3. Real-Time Order Matching & Friction Modeling

ATSNT enforces the **Zero-Toy-Assumption Principle**:

1. **Entry Friction**:
   - A strategy signal becomes a **market order** sized at the signal price (the same sizing the live gateway uses). It cannot fill on the bar that produced the signal, because that bar has already closed: it fills at the **next bar's open**.
   - Market entries pay the **Taker Fee** ($0.05\%$ default on Binance Futures) and a slippage penalty ($0.05\%$ default) that moves the fill away from that open:
     $$\text{Fill}_{\text{Buy}} = O_{k+1} \cdot (1 + \text{Slippage}), \quad \text{Fill}_{\text{Sell}} = O_{k+1} \cdot (1 - \text{Slippage})$$
   - The strategy sees **every** bar, also while a position is open, exactly like the live session, so its rolling state (windows, accumulators) is identical in both. Signals only turn into orders while flat; a bar whose time barrier just closed a position cannot open a new one (the close is still settling live).

2. **Perpetual funding and isolated margin** (bar-level engine; the tick-level venue applies the same rules from `domain::futures`):
   - A position pays $\text{qty} \cdot \text{mark} \cdot \text{rate}$ at every 00:00/08:00/16:00 UTC settlement a bar crosses (marked at that bar's open); longs pay a positive rate. It is deducted from cash and reported as `funding_paid` inside the trade's net PnL. The rate is `funding_rate_pct` (0.01% baseline).
   - The account runs each position at the lowest leverage its balance can margin (as the live gateway does). When the mark reaches the liquidation price before the stop does, or a bar gaps through it, the exchange closes at the bankruptcy price and the whole margin is lost (`Liquidation`). Maintenance margin is one flat `maintenance_margin_rate` (0.4%), not the exchange's notional brackets.
3. **Triple Barrier Exits** (all are market orders: taker fee plus slippage against the position):
   - **Stop Loss**: Triggered when the bar breaches the stop level. Fills at the stop or, if the bar opened beyond it (a gap), at the open, minus slippage.
   - **Take Profit**: Triggered when the bar touches the target (`TAKE_PROFIT_MARKET` live). Fills at the target or, on a favorable gap, at the open, minus slippage. When one bar touches both barriers the stop wins.
   - **Time Barrier**: Forced market exit after holding a position for $N$ consecutive Dollar Bars (e.g. 15 bars; the fill bar counts as the first). Executed at the bar close with taker fee and slippage.

4. **Mark-to-Market Telemetry**:
   - On every incoming trade tick, if a position is active, the session computes:
     $$\text{Unrealized PnL} = (\text{Mark Price} - \text{Entry Price}) \cdot \text{Quantity} \cdot \text{Direction}$$
     $$\text{Total Equity} = \text{Cash Equity} + \text{Unrealized PnL}$$
     $$\text{Drawdown} = \frac{\text{Peak Equity} - \text{Total Equity}}{\text{Peak Equity}}$$

---

## 4. Event Streaming Contract (`PaperTradingEvent`)

The raw event/config APIs below remain compatible. The former web endpoint `/ws/telemetry` and the web binary were retired with the in-process web server in Milestone 5; paper-session telemetry now streams to BigQuery (see [the telemetry specification](07-web-telemetry-dashboard.md)).

All live state mutations are broadcast over an asynchronous `tokio::sync::broadcast` channel, decoupled from the presentation layer:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaperTradingEvent {
    /// A new Dollar Bar has finalized and rolled over.
    BarFormed(DollarBar),
    /// The quantitative strategy generated an actionable order intent.
    SignalGenerated(OrderIntent),
    /// A simulated order was filled and a new position was opened.
    PositionOpened {
        side: PositionSide,
        entry_price: Decimal,
        quantity: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
    },
    /// An active position was closed by Stop Loss, Take Profit, or Time Barrier.
    PositionClosed {
        exit_reason: String,
        exit_price: Decimal,
        net_pnl: Decimal,
        total_equity: Decimal,
    },
    /// Continuous mark-to-market equity and drawdown update.
    MarkToMarket {
        current_price: Decimal,
        unrealized_pnl: Decimal,
        total_equity: Decimal,
        drawdown_pct: Decimal,
    },
    /// The monthly kill-switch closed the position and halted the session.
    CircuitBreakerTripped {
        scope: String,
        drawdown_pct: Decimal,
    },
}
```

---

## 5. Audit Telemetry Schema (`storage/reports/paper_trading_<timestamp>.json`)

Upon receiving an interruption signal (`SIGINT` / `Ctrl+C`), the session leaves any open position open (it is not liquidated and not part of the metrics) and serializes complete quantitative attribution metrics to disk:

```json
{
  "timestamp": 1791199893,
  "symbol": "BTCUSDT",
  "stream_mode": "spot",
  "dollar_bar_threshold": "50000",
  "initial_capital": "10000.00",
  "final_equity": "10045.20",
  "raw_ticks_processed": 14250,
  "dollar_bars_formed": 18,
  "strategy_config": {
    "rolling_window_len": 20,
    "cusum_vol_multiplier": "2.0",
    "z_entry_threshold": "2.0",
    "z_stop_threshold": "3.5",
    "time_barrier_bars": 15
  },
  "backtest_config": {
    "maker_fee_pct": "0.0002",
    "taker_fee_pct": "0.0005",
    "slippage_pct": "0.0005",
    "risk_per_trade_pct": "0.01",
    "max_daily_drawdown_pct": "0.05",
    "max_monthly_drawdown_pct": "0.15",
    "max_leverage": "20",
    "min_order_quantity": "0.001",
    "quantity_step_size": "0.001",
    "min_order_notional": "5"
  },
  "metrics": {
    "total_trades": 4,
    "winning_trades": 3,
    "losing_trades": 1,
    "win_rate": "0.75",
    "profit_factor": "2.40",
    "payoff_ratio": "1.80",
    "mathematical_expectancy": "11.30",
    "gross_profit": "60.00",
    "gross_loss": "-10.00",
    "net_profit": "45.20",
    "total_fees": "4.10",
    "total_slippage": "0.70",
    "max_drawdown_amount": "12.50",
    "max_drawdown_pct": "0.0012",
    "sortino_ratio": "3.15"
  },
  "closed_trades": [
    {
      "exit_time": 1791199880000,
      "pnl_gross": "25.00",
      "fees_paid": "1.20",
      "slippage_paid": "0.20",
      "pnl_net": "23.60",
      "return_pct": "0.0236"
    }
  ]
}
```
