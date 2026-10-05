//! Binance WebSocket streaming market data adapter.
//!
//! Provides live aggregate trade ingestion from the Binance Futures WebSocket stream
//! with automatic reconnection, exponential backoff, and heartbeat pings.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::sync::watch;

use domain::{Side, Trade};

use crate::error::AdapterError;
use crate::traits::AsyncMarketDataStream;

/// Configuration parameters for the Binance WebSocket client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinanceWsConfig {
    /// WebSocket base endpoint URL. Default: `"wss://fstream.binance.com/ws/{symbol}@aggTrade"`.
    pub base_url: String,
    /// Target trading symbol in lowercase (e.g., `"btcusdt"`).
    pub symbol: String,
    /// Bounded MPSC channel capacity for buffering ingested trades. Default: 10,000.
    pub channel_capacity: usize,
    /// Initial backoff interval in milliseconds when reconnecting. Default: 500 ms.
    pub initial_backoff_ms: u64,
    /// Maximum backoff interval in milliseconds when reconnecting. Default: 30,000 ms.
    pub max_backoff_ms: u64,
    /// Interval in seconds between WebSocket ping heartbeat frames. Default: 30 seconds.
    pub ping_interval_secs: u64,
}

impl Default for BinanceWsConfig {
    fn default() -> Self {
        Self {
            base_url: "wss://fstream.binance.com/ws/{symbol}@aggTrade".to_string(),
            symbol: "btcusdt".to_string(),
            channel_capacity: 10_000,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
            ping_interval_secs: 30,
        }
    }
}

impl BinanceWsConfig {
    /// Creates a configuration for a specific market symbol with default parameters.
    pub fn for_symbol(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into().to_ascii_lowercase(),
            ..Self::default()
        }
    }

    /// Creates a configuration for Binance Futures (`fstream.binance.com`).
    pub fn futures(symbol: impl Into<String>) -> Self {
        Self {
            base_url: "wss://fstream.binance.com/ws/{symbol}@aggTrade".to_string(),
            symbol: symbol.into().to_ascii_lowercase(),
            ..Self::default()
        }
    }

    /// Creates a configuration for Binance Spot (`stream.binance.com:9443`).
    pub fn spot(symbol: impl Into<String>) -> Self {
        Self {
            base_url: "wss://stream.binance.com:9443/ws/{symbol}@aggTrade".to_string(),
            symbol: symbol.into().to_ascii_lowercase(),
            ..Self::default()
        }
    }

    /// Resolves the concrete WebSocket stream URL for this configuration.
    pub fn stream_url(&self) -> String {
        let sym = self.symbol.to_ascii_lowercase();
        if self.base_url.contains("{symbol}") {
            self.base_url.replace("{symbol}", &sym)
        } else if self.base_url.ends_with('/') {
            format!("{}{sym}@aggTrade", self.base_url)
        } else {
            format!("{}/{sym}@aggTrade", self.base_url)
        }
    }
}

/// Raw aggregate trade payload deserialized from Binance WebSocket JSON.
///
/// Invariant: Prices and quantities are parsed strictly as `rust_decimal::Decimal`
/// to guarantee zero floating-point imprecision.
#[allow(non_snake_case)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BinanceAggTradePayload {
    /// Aggregate trade ID (`a`).
    #[serde(rename = "a")]
    pub a: u64,
    /// Execution price (`p`).
    #[serde(rename = "p")]
    pub p: Decimal,
    /// Executed trade quantity (`q`).
    #[serde(rename = "q")]
    pub q: Decimal,
    /// Transact time in milliseconds since Unix epoch (`T`).
    #[serde(rename = "T")]
    pub T: i64,
    /// Buyer maker flag (`m`).
    ///
    /// If `true`, the buyer was maker (passive bid), meaning aggressor/taker was seller (`Side::Sell`).
    /// If `false`, the seller was maker (passive ask), meaning aggressor/taker was buyer (`Side::Buy`).
    #[serde(rename = "m")]
    pub m: bool,
}

impl BinanceAggTradePayload {
    /// Returns the aggressor (taker) order side according to market mechanics.
    #[inline]
    pub fn side(&self) -> Side {
        if self.m {
            Side::Sell
        } else {
            Side::Buy
        }
    }

    /// Converts this payload into a validated domain [`Trade`].
    pub fn into_trade(self) -> Result<Trade, domain::DomainError> {
        let side = self.side();
        Trade::new(self.T, self.p, self.q, side)
    }
}

impl TryFrom<BinanceAggTradePayload> for Trade {
    type Error = domain::DomainError;

    fn try_from(payload: BinanceAggTradePayload) -> Result<Self, Self::Error> {
        payload.into_trade()
    }
}

/// Asynchronous market data stream ingesting real-time trades from Binance WebSocket.
pub struct BinanceWebSocketStream {
    receiver: mpsc::Receiver<Trade>,
    shutdown_tx: Option<watch::Sender<bool>>,
    task_handle: tokio::task::JoinHandle<()>,
}

impl BinanceWebSocketStream {
    /// Connects to the Binance WebSocket feed using the provided configuration,
    /// spawning a background resilience manager task for automatic reconnection and ping heartbeats.
    pub fn connect(config: BinanceWsConfig) -> Result<Self, AdapterError> {
        if config.channel_capacity == 0 {
            return Err(AdapterError::General(
                "channel_capacity must be greater than 0".to_string(),
            ));
        }
        if config.symbol.trim().is_empty() {
            return Err(AdapterError::General("symbol cannot be empty".to_string()));
        }
        if config.ping_interval_secs == 0 {
            return Err(AdapterError::General(
                "ping_interval_secs must be greater than 0".to_string(),
            ));
        }
        if config.initial_backoff_ms == 0 {
            return Err(AdapterError::General(
                "initial_backoff_ms must be greater than 0".to_string(),
            ));
        }
        if config.max_backoff_ms == 0 {
            return Err(AdapterError::General(
                "max_backoff_ms must be greater than 0".to_string(),
            ));
        }

        let (trade_tx, trade_rx) = mpsc::channel(config.channel_capacity);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let task_handle = tokio::spawn(async move {
            run_connection_manager(config, trade_tx, shutdown_rx).await;
        });

        Ok(Self {
            receiver: trade_rx,
            shutdown_tx: Some(shutdown_tx),
            task_handle,
        })
    }

    /// Explicitly shuts down the streaming background task and disconnects from the feed.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        self.task_handle.abort();
    }
}

impl Drop for BinanceWebSocketStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl AsyncMarketDataStream for BinanceWebSocketStream {
    type Error = AdapterError;

    async fn next_trade(&mut self) -> Result<Option<Trade>, Self::Error> {
        Ok(self.receiver.recv().await)
    }
}

async fn run_connection_manager(
    config: BinanceWsConfig,
    trade_tx: mpsc::Sender<Trade>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut current_backoff = Duration::from_millis(config.initial_backoff_ms);
    let max_backoff = Duration::from_millis(config.max_backoff_ms);
    let ping_interval = Duration::from_secs(config.ping_interval_secs);
    let url = config.stream_url();

    loop {
        if *shutdown_rx.borrow() || trade_tx.is_closed() {
            tracing::info!("Binance WebSocket stream shutdown requested.");
            break;
        }

        tracing::info!(stream_url = %url, "Connecting to Binance WebSocket...");

        let connect_future = tokio_tungstenite::connect_async(&url);
        let ws_stream = tokio::select! {
            biased;
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    break;
                }
                continue;
            }
            res = connect_future => {
                match res {
                    Ok((stream, response)) => {
                        tracing::info!(status = %response.status(), "Connected to Binance WebSocket");
                        current_backoff = Duration::from_millis(config.initial_backoff_ms);
                        stream
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            backoff_ms = current_backoff.as_millis(),
                            "WebSocket connection failed, backing off..."
                        );
                        tokio::select! {
                            biased;
                            res = shutdown_rx.changed() => {
                                if res.is_err() || *shutdown_rx.borrow() {
                                    return;
                                }
                            }
                            _ = tokio::time::sleep(current_backoff) => {}
                        }
                        current_backoff = (current_backoff * 2).min(max_backoff);
                        continue;
                    }
                }
            }
        };

        let (mut ws_sink, mut ws_stream) = ws_stream.split();
        let mut ping_ticker = tokio::time::interval(ping_interval);
        // Advance past immediate first tick
        ping_ticker.tick().await;

        let mut clean_exit = false;

        loop {
            tokio::select! {
                biased;
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        tracing::info!("Shutdown signal received during active stream.");
                        let _ = ws_sink.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                        clean_exit = true;
                        break;
                    }
                }
                _ = ping_ticker.tick() => {
                    tracing::trace!("Sending WebSocket ping heartbeat");
                    if let Err(err) = ws_sink.send(tokio_tungstenite::tungstenite::Message::Ping(Default::default())).await {
                        tracing::warn!(error = %err, "Failed to send WebSocket ping frame");
                        break;
                    }
                }
                msg = ws_stream.next() => {
                    match msg {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            match serde_json::from_str::<BinanceAggTradePayload>(&text) {
                                Ok(payload) => {
                                    match Trade::try_from(payload) {
                                        Ok(trade) => {
                                            if trade_tx.send(trade).await.is_err() {
                                                tracing::info!("Trade receiver dropped; shutting down stream manager.");
                                                clean_exit = true;
                                                break;
                                            }
                                        }
                                        Err(domain_err) => {
                                            tracing::warn!(error = %domain_err, "Invalid domain trade payload received");
                                        }
                                    }
                                }
                                Err(parse_err) => {
                                    tracing::debug!(error = %parse_err, text = %text, "Non-trade WebSocket message received");
                                }
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes))) => {
                            if let Ok(text) = std::str::from_utf8(&bytes) {
                                if let Ok(payload) = serde_json::from_str::<BinanceAggTradePayload>(text) {
                                    if let Ok(trade) = Trade::try_from(payload) {
                                        if trade_tx.send(trade).await.is_err() {
                                            clean_exit = true;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                            let _ = ws_sink.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await;
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(_))) => {
                            tracing::trace!("Received WebSocket pong heartbeat acknowledgment");
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => {
                            tracing::warn!(frame = ?frame, "WebSocket received close frame; reconnecting...");
                            break;
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Frame(_))) => {}
                        Some(Err(err)) => {
                            tracing::warn!(error = %err, "WebSocket read error; reconnecting...");
                            break;
                        }
                        None => {
                            tracing::warn!("WebSocket stream ended unexpectedly; reconnecting...");
                            break;
                        }
                    }
                }
            }
        }

        if clean_exit || *shutdown_rx.borrow() || trade_tx.is_closed() {
            break;
        }

        tracing::info!(
            backoff_ms = current_backoff.as_millis(),
            "Waiting before reconnecting..."
        );
        tokio::select! {
            biased;
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(current_backoff) => {}
        }
        current_backoff = (current_backoff * 2).min(max_backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_deserialize_valid_agg_trade_payload() {
        let json = r#"{
            "e": "aggTrade",
            "E": 1672531199999,
            "s": "BTCUSDT",
            "a": 123456789,
            "p": "42150.75",
            "q": "0.1500",
            "f": 9876543,
            "l": 9876545,
            "T": 1672531200000,
            "m": true,
            "M": true
        }"#;

        let payload: BinanceAggTradePayload =
            serde_json::from_str(json).expect("failed to deserialize aggTrade payload");
        assert_eq!(payload.a, 123456789);
        assert_eq!(payload.p, dec!(42150.75));
        assert_eq!(payload.q, dec!(0.1500));
        assert_eq!(payload.T, 1672531200000);
        assert!(payload.m);

        let trade: Trade = payload.try_into().expect("conversion into Trade failed");
        assert_eq!(trade.timestamp, 1672531200000);
        assert_eq!(trade.price, dec!(42150.75));
        assert_eq!(trade.quantity, dec!(0.1500));
        assert_eq!(trade.side, Side::Sell);
    }

    #[test]
    fn test_aggressor_side_mapping_logic() {
        // m = true: buyer was maker -> aggressor was seller (Side::Sell)
        let payload_sell = BinanceAggTradePayload {
            a: 1,
            p: dec!(50000),
            q: dec!(1),
            T: 1000,
            m: true,
        };
        assert_eq!(payload_sell.side(), Side::Sell);
        let trade_sell = Trade::try_from(payload_sell).expect("valid trade");
        assert_eq!(trade_sell.side, Side::Sell);

        // m = false: seller was maker -> aggressor was buyer (Side::Buy)
        let payload_buy = BinanceAggTradePayload {
            a: 2,
            p: dec!(50000),
            q: dec!(1),
            T: 1000,
            m: false,
        };
        assert_eq!(payload_buy.side(), Side::Buy);
        let trade_buy = Trade::try_from(payload_buy).expect("valid trade");
        assert_eq!(trade_buy.side, Side::Buy);
    }

    #[test]
    fn test_ws_config_stream_url() {
        let config_default = BinanceWsConfig::default();
        assert_eq!(
            config_default.stream_url(),
            "wss://fstream.binance.com/ws/btcusdt@aggTrade"
        );

        let config_eth = BinanceWsConfig::for_symbol("ETHUSDT");
        assert_eq!(
            config_eth.stream_url(),
            "wss://fstream.binance.com/ws/ethusdt@aggTrade"
        );

        let custom_config = BinanceWsConfig {
            base_url: "wss://custom.stream.com/stream/".to_string(),
            symbol: "solusdt".to_string(),
            ..Default::default()
        };
        assert_eq!(
            custom_config.stream_url(),
            "wss://custom.stream.com/stream/solusdt@aggTrade"
        );
    }

    #[test]
    fn test_invalid_trade_payload_rejected_by_domain() {
        let payload_zero_price = BinanceAggTradePayload {
            a: 3,
            p: dec!(0),
            q: dec!(1),
            T: 1000,
            m: false,
        };
        assert!(Trade::try_from(payload_zero_price).is_err());

        let payload_zero_qty = BinanceAggTradePayload {
            a: 4,
            p: dec!(50000),
            q: dec!(0),
            T: 1000,
            m: false,
        };
        assert!(Trade::try_from(payload_zero_qty).is_err());
    }

    #[tokio::test]
    async fn test_stream_connect_validation() {
        let invalid_config = BinanceWsConfig {
            channel_capacity: 0,
            ..Default::default()
        };
        assert!(BinanceWebSocketStream::connect(invalid_config).is_err());

        let invalid_symbol_config = BinanceWsConfig {
            symbol: "   ".to_string(),
            ..Default::default()
        };
        assert!(BinanceWebSocketStream::connect(invalid_symbol_config).is_err());

        let invalid_ping_config = BinanceWsConfig {
            ping_interval_secs: 0,
            ..Default::default()
        };
        assert!(BinanceWebSocketStream::connect(invalid_ping_config).is_err());
    }
}
