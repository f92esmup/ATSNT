# Market Data Adapters & Binance Ingestion Specification

## 1. Architectural Role
In accordance with Hexagonal Architecture (Ports and Adapters), the adapters crate (`crates/adapters`) contains concrete implementations of external interfaces. The core trading engine and domain logic remain completely decoupled from network protocols, file formats, and broker-specific APIs.

---

## 2. Ingestion Ports: `MarketDataStream` & `AsyncMarketDataStream`

The domain and adapter layer define market data expectations through abstract ports:

### 2.1 Synchronous Port (`MarketDataStream`)
Used by batch readers (`BinanceCsvReader`, `BinanceParquetReader`):
```rust
pub trait MarketDataStream {
    type Error;
    fn next_trade(&mut self) -> Result<Option<domain::Trade>, Self::Error>;
}
```

### 2.2 Asynchronous Streaming Port (`AsyncMarketDataStream`)
Used by real-time reactive network feeds (`BinanceWebSocketStream`):
```rust
pub trait AsyncMarketDataStream {
    type Error;
    async fn next_trade(&mut self) -> Result<Option<domain::Trade>, Self::Error>;
}
```

Any source—a historical CSV, an optimized columnar Parquet file, or an asynchronous WebSocket stream—plugs into these ports by emitting normalized `domain::Trade` items.

---

## 3. Binance `aggTrades` Standard & Zero-Interpolation Principle

### 3.1 Why Aggregate Trades (`aggTrades`)?
In high-frequency crypto trading, large taker orders are matched against multiple resting limit orders, generating multiple sub-millisecond execution ticks for a single market order. Binance groups these simultaneous fills into `aggTrades`.

Using `aggTrades`:
- Preserves the true sequence of market aggression.
- Eliminates the need for artificial tick interpolation.
- Reduces network and I/O bandwidth without discarding volume precision.

### 3.2 Field Mapping Specification

Both historical CSVs from `data.binance.vision` and live WebSocket streams from `wss://fstream.binance.com/ws/{symbol}@aggTrade` map identically:

| Binance Field | Historical CSV Header | Live WebSocket Key | Domain `Trade` Target | Interpretation |
| :--- | :--- | :--- | :--- | :--- |
| Aggregate Trade ID | `agg_trade_id` | `a` | *(Internal/Audit)* | Sequential identifier |
| Price | `price` | `p` | `trade.price` (`Decimal`) | Execution price |
| Quantity | `quantity` | `q` | `trade.quantity` (`Decimal`) | Transacted base asset volume |
| Timestamp | `transact_time` | `T` | `trade.timestamp` (`i64`) | Execution timestamp (Unix ms) |
| Buyer Maker Flag | `is_buyer_maker` | `m` | `trade.side` (`Side`) | `true` = Market Sell / `false` = Market Buy |

#### Rule for Aggressor Side:
- If `is_buyer_maker == true`: The buyer was the passive maker; the active aggressor was a seller $\rightarrow$ `Side::Sell`.
- If `is_buyer_maker == false`: The seller was the passive maker; the active aggressor was a buyer $\rightarrow$ `Side::Buy`.

---

## 4. The Three Operational Modes

```text
                        ┌─────────────────────────────────────┐
                        │      PORT: MarketDataStream         │
                        └──────────────────▲──────────────────┘
                                           │
         ┌─────────────────────────────────┼─────────────────────────────────┐
         │                                 │                                 │
   [ ADAPTER 1: Historical ]      [ ADAPTER 2: Paper Trading ]      [ ADAPTER 3: Live Trading ]
   - Binance aggTrades CSV/ZIP     - Live WebSocket stream           - Live WebSocket stream
   - High-throughput batch read    - In-memory simulated execution   - Authenticated REST / WebSocket
   - Deterministic event replay    - Real market ticks / no money    - Signed API keys / Real orders
```

---

## 5. Automated Historical Market Data ETL (`fetch_data`)

The `adapters` crate provides the official ETL CLI binary [`fetch_data`](crates/adapters/src/bin/fetch_data.rs):

### 5.1 Architecture & Pipeline Mechanics
1. **Direct Public Ingestion**: Downloads official Binance Futures USDT-M `aggTrades` archives from `data.binance.vision` with zero authenticated API rate limits.
2. **Streaming Conversion to Columnar Parquet**: Streams ZIP archives directly through memory, parsing CSV lines and writing Snappy-compressed Arrow `RecordBatch` chunks using [`BinanceParquetWriter`]. Memory consumption remains constant (< 100 MB) even on multi-gigabyte monthly archives.
3. **Native Year-to-Date (YTD) Intelligence**: Automatically handles the active ongoing year (e.g., 2026):
   - Ingests closed months as comprehensive monthly archives.
   - For the current month in progress, automatically discovers and converts daily archives (`day 1..N`) up to today.
   - Gracefully finalizes without error if today's ongoing session has not yet closed on Binance Vision.
4. **Isolated Partitioning**: Stores Parquet datasets cleanly partitioned by year and symbol (e.g. `data/historical_2026/BTCUSDT/*.parquet`), enabling chronological multi-file streaming via [`BinanceParquetReader`].

