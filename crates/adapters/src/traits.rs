use domain::Trade;

use crate::error::AdapterError;

/// Ingestion port for discrete market trade streams.
///
/// Implemented by historical CSV readers, Parquet files, or live WebSocket clients.
pub trait MarketDataStream {
    type Error;

    /// Fetches the next sequential market transaction, or `None` if EOF or stream ended.
    fn next_trade(&mut self) -> Result<Option<Trade>, Self::Error>;
}

/// Asynchronous ingestion port for live market trade streams.
///
/// Implemented by WebSocket clients and real-time streaming adapters.
pub trait AsyncMarketDataStream {
    type Error;

    /// Awaits and returns the next sequential market transaction, or `None` if the stream ended.
    #[allow(async_fn_in_trait)]
    async fn next_trade(&mut self) -> Result<Option<Trade>, Self::Error>;
}

/// Outbound alerting port for immediate push notifications (Telegram, Discord, Webhooks).
pub trait AlertNotifier: Send + Sync {
    /// Dispatches an alert message to the configured notification channel.
    #[allow(async_fn_in_trait)]
    async fn send_alert(&self, message: &str) -> Result<(), AdapterError>;
}
