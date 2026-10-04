use domain::Trade;

/// Ingestion port for discrete market trade streams.
///
/// Implemented by historical CSV readers, Parquet files, or live WebSocket clients.
pub trait MarketDataStream {
    type Error;

    /// Fetches the next sequential market transaction, or `None` if EOF or stream ended.
    fn next_trade(&mut self) -> Result<Option<Trade>, Self::Error>;
}
