# Real-Time Paper Trading & Asynchronous Execution Specification

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
│             BacktestEngine             │ ──► Fills with friction (fees + slip)
└────────────────────────────────────────┘     Triple Barrier Exits (SL, TP, Time)
     │
     ▼
┌────────────────────────────────────────┐
│ tokio::sync::broadcast (Capacity 10k)  │ ──► PaperTradingEvent Stream
└────────────────────────────────────────┘     (Web /ws/telemetry via envelope)
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

---

## 3. Real-Time Order Matching & Friction Modeling

ATSNT enforces the **Zero-Toy-Assumption Principle**:

1. **Entry Friction**:
   - Market order entries pay **Taker Fee** ($0.05\%$ default on Binance Futures).
   - Latency slippage penalty ($0.05\%$ default) shifts the fill price away from the signal price:
     $$\text{Fill}_{\text{Buy}} = P \cdot (1 + \text{Slippage}), \quad \text{Fill}_{\text{Sell}} = P \cdot (1 - \text{Slippage})$$

2. **Triple Barrier Exits**:
   - **Stop Loss**: Triggered when bar price breaches the stop loss level. Executed as a taker market order with slippage.
   - **Take Profit**: Resting limit order hit when bar price touches the target level. Executed as a maker order ($0.02\%$ fee) with zero slippage.
   - **Time Barrier**: Forced market exit after holding a position for $N$ consecutive Dollar Bars (e.g. 15 bars). Executed with taker fee and slippage.

3. **Mark-to-Market Telemetry**:
   - On every incoming trade tick, if a position is active, the session computes:
     $$\text{Unrealized PnL} = (\text{Mark Price} - \text{Entry Price}) \cdot \text{Quantity} \cdot \text{Direction}$$
     $$\text{Total Equity} = \text{Cash Equity} + \text{Unrealized PnL}$$
     $$\text{Drawdown} = \frac{\text{Peak Equity} - \text{Total Equity}}{\text{Peak Equity}}$$

---

## 4. Event Streaming Contract (`PaperTradingEvent`)

The raw event/config APIs below remain compatible. The current web endpoint is `/ws/telemetry`, consuming T2a's separate timestamped, symbol/strategy-aware envelope; see the [current envelope and snapshot contract](07-web-telemetry-dashboard.md#32-websocket-streaming-wstelemetry). The web binary's real paper-session wiring remains T3 pending at base HEAD `c7ae8f4107b9214a3c7f4fc20193393b65da617c`; this diagram is not evidence of that integration. The [proposed workspace](09-read-only-web-workspace.md) consumes T2b/T3 through W2 without duplicating them.

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
}
```

---

## 5. Audit Telemetry Schema (`storage/reports/paper_trading_<timestamp>.json`)

Upon receiving an interruption signal (`SIGINT` / `Ctrl+C`), the session liquidates any open position at the current mark price and serializes complete quantitative attribution metrics to disk:

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
    "max_daily_drawdown_pct": "0.05"
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
