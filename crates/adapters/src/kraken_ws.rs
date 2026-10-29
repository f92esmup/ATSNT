//! Kraken WebSocket v2 streaming market data adapter.
//!
//! Ingests real-time public trade events from the Kraken WebSocket API v2
//! (`wss://ws.kraken.com/v2`) with automatic reconnection, exponential backoff,
//! and heartbeat ping frames. Normalizes incoming trades into canonical [`domain::Trade`].

use std::time::Duration;

use chrono::DateTime;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use domain::{Side, Trade};

use crate::error::AdapterError;
use crate::traits::AsyncMarketDataStream;

/// Official public WebSocket endpoint for Kraken API v2.
pub const KRAKEN_WS_V2_URL: &str = "wss://ws.kraken.com/v2";

/// Normalizes an ATSNT internal symbol (e.g. `"BTCEUR"` or `"BTC/EUR"`) to Kraken v2 format (`"BTC/EUR"`).
pub fn normalize_to_kraken_symbol(symbol: &str) -> String {
    let sym = symbol.trim().to_ascii_uppercase();
    if sym.contains('/') {
        return sym;
    }
    // Handle standard EUR spot pairs
    if let Some(base) = sym.strip_suffix("EUR") {
        return format!("{base}/EUR");
    }
    if let Some(base) = sym.strip_suffix("USDT") {
        return format!("{base}/USDT");
    }
    if let Some(base) = sym.strip_suffix("USD") {
        return format!("{base}/USD");
    }
    sym
}

/// Normalizes a Kraken v2 pair (e.g. `"BTC/EUR"`) back to internal ATSNT canonical format (`"BTCEUR"`).
pub fn normalize_from_kraken_symbol(kraken_symbol: &str) -> String {
    kraken_symbol.replace('/', "").to_ascii_uppercase()
}

/// Configuration parameters for the Kraken WebSocket v2 client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KrakenWsConfig {
    /// WebSocket base endpoint URL. Default: `wss://ws.kraken.com/v2`.
    pub base_url: String,
    /// Target trading symbol in Kraken v2 format (e.g., `"BTC/EUR"`).
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

impl Default for KrakenWsConfig {
    fn default() -> Self {
        Self {
            base_url: KRAKEN_WS_V2_URL.to_string(),
            symbol: "BTC/EUR".to_string(),
            channel_capacity: 10_000,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
            ping_interval_secs: 30,
        }
    }
}

impl KrakenWsConfig {
    /// Creates a configuration for a specific market symbol with default parameters.
    pub fn for_symbol(symbol: impl AsRef<str>) -> Self {
        let kraken_sym = normalize_to_kraken_symbol(symbol.as_ref());
        Self {
            symbol: kraken_sym,
            ..Self::default()
        }
    }

    /// Formats the subscription JSON message required by Kraken v2 API.
    pub fn subscription_payload(&self) -> String {
        serde_json::json!({
            "method": "subscribe",
            "params": {
                "channel": "trade",
                "symbol": [self.symbol]
            }
        })
        .to_string()
    }
}

/// Deserialized wrapper for Kraken WebSocket v2 trade channel messages.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KrakenWsMessage {
    pub channel: Option<String>,
    pub r#type: Option<String>,
    pub data: Option<Vec<KrakenTradeItem>>,
    pub method: Option<String>,
    pub success: Option<bool>,
}

/// Individual trade print item from Kraken v2 `trade` channel.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KrakenTradeItem {
    pub symbol: String,
    pub side: String,
    pub price: Decimal,
    pub qty: Decimal,
    #[serde(default)]
    pub ord_type: Option<String>,
    #[serde(default)]
    pub trade_id: Option<u64>,
    pub timestamp: String,
}

impl KrakenTradeItem {
    /// Converts raw Kraken trade into a strongly typed domain [`Trade`].
    pub fn into_trade(self) -> Result<Trade, domain::DomainError> {
        let side = match self.side.to_ascii_lowercase().as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => Side::Buy,
        };

        let timestamp_ms = DateTime::parse_from_rfc3339(&self.timestamp)
            .map(|dt| dt.timestamp_millis())
            .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis());

        Trade::new(timestamp_ms, self.price, self.qty, side)
    }
}

impl TryFrom<KrakenTradeItem> for Trade {
    type Error = domain::DomainError;

    fn try_from(item: KrakenTradeItem) -> Result<Self, Self::Error> {
        item.into_trade()
    }
}

/// Asynchronous market data stream ingesting real-time trades from Kraken WebSocket v2.
pub struct KrakenWebSocketStream {
    receiver: mpsc::Receiver<Trade>,
    shutdown_tx: Option<watch::Sender<bool>>,
    failures_rx: watch::Receiver<u32>,
    task_handle: tokio::task::JoinHandle<()>,
}

impl KrakenWebSocketStream {
    /// Connects to Kraken WebSocket v2 feed using the provided configuration,
    /// spawning a background resilience manager task for automatic reconnection and ping heartbeats.
    pub fn connect(config: KrakenWsConfig) -> Result<Self, AdapterError> {
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
        let (failures_tx, failures_rx) = watch::channel(0);

        let task_handle = tokio::spawn(async move {
            run_kraken_connection_manager(config, trade_tx, shutdown_rx, failures_tx).await;
        });

        Ok(Self {
            receiver: trade_rx,
            shutdown_tx: Some(shutdown_tx),
            failures_rx,
            task_handle,
        })
    }

    /// Consecutive connection attempts that failed; resets to 0 once a connection succeeds.
    pub fn connection_failures(&self) -> watch::Receiver<u32> {
        self.failures_rx.clone()
    }

    /// Explicitly shuts down the streaming background task and disconnects from the feed.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        self.task_handle.abort();
    }
}

impl Drop for KrakenWebSocketStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl AsyncMarketDataStream for KrakenWebSocketStream {
    type Error = AdapterError;

    async fn next_trade(&mut self) -> Result<Option<Trade>, Self::Error> {
        Ok(self.receiver.recv().await)
    }
}

async fn run_kraken_connection_manager(
    config: KrakenWsConfig,
    trade_tx: mpsc::Sender<Trade>,
    mut shutdown_rx: watch::Receiver<bool>,
    failures_tx: watch::Sender<u32>,
) {
    let mut current_backoff = Duration::from_millis(config.initial_backoff_ms);
    let max_backoff = Duration::from_millis(config.max_backoff_ms);
    let ping_interval = Duration::from_secs(config.ping_interval_secs);
    let url = config.base_url.clone();

    loop {
        if *shutdown_rx.borrow() || trade_tx.is_closed() {
            tracing::info!("Kraken WebSocket stream shutdown requested.");
            break;
        }

        tracing::info!(stream_url = %url, symbol = %config.symbol, "Connecting to Kraken WebSocket v2...");

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
                        tracing::info!(status = %response.status(), symbol = %config.symbol, "Connected to Kraken WebSocket v2");
                        current_backoff = Duration::from_millis(config.initial_backoff_ms);
                        failures_tx.send_replace(0);
                        stream
                    }
                    Err(err) => {
                        failures_tx.send_modify(|failures| *failures = failures.saturating_add(1));
                        tracing::warn!(
                            error = %err,
                            symbol = %config.symbol,
                            backoff_ms = current_backoff.as_millis(),
                            "Kraken WebSocket connection failed, backing off..."
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

        // Send subscribe message immediately upon establishing connection
        let sub_payload = config.subscription_payload();
        tracing::info!(symbol = %config.symbol, payload = %sub_payload, "Sending Kraken v2 trade subscription");
        if let Err(err) = ws_sink
            .send(tokio_tungstenite::tungstenite::Message::Text(
                sub_payload.into(),
            ))
            .await
        {
            tracing::warn!(error = %err, symbol = %config.symbol, "Failed to send subscribe message to Kraken");
            continue;
        }

        let mut ping_ticker = tokio::time::interval(ping_interval);
        ping_ticker.tick().await;

        let mut clean_exit = false;

        loop {
            tokio::select! {
                biased;
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        tracing::info!("Shutdown signal received during active Kraken stream.");
                        let _ = ws_sink.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                        clean_exit = true;
                        break;
                    }
                }
                _ = ping_ticker.tick() => {
                    tracing::trace!("Sending WebSocket ping heartbeat to Kraken");
                    if let Err(err) = ws_sink.send(tokio_tungstenite::tungstenite::Message::Ping(Default::default())).await {
                        tracing::warn!(error = %err, "Failed to send WebSocket ping frame to Kraken");
                        break;
                    }
                }
                msg = ws_stream.next() => {
                    match msg {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            if let Ok(ws_msg) = serde_json::from_str::<KrakenWsMessage>(&text) {
                                if ws_msg.channel.as_deref() == Some("trade") {
                                    if let Some(items) = ws_msg.data {
                                        for item in items {
                                            match Trade::try_from(item) {
                                                Ok(trade) => {
                                                    if trade_tx.send(trade).await.is_err() {
                                                        tracing::info!("Trade receiver dropped; shutting down Kraken stream manager.");
                                                        clean_exit = true;
                                                        break;
                                                    }
                                                }
                                                Err(domain_err) => {
                                                    tracing::warn!(error = %domain_err, "Invalid domain trade payload from Kraken");
                                                }
                                            }
                                        }
                                    }
                                } else if ws_msg.channel.as_deref() == Some("heartbeat") {
                                    tracing::trace!("Kraken heartbeat frame received");
                                } else if ws_msg.method.as_deref() == Some("subscribe") {
                                    tracing::info!(success = ?ws_msg.success, "Kraken subscription confirmed");
                                }
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes))) => {
                            if let Ok(text) = std::str::from_utf8(&bytes) {
                                if let Ok(ws_msg) = serde_json::from_str::<KrakenWsMessage>(text) {
                                    if ws_msg.channel.as_deref() == Some("trade") {
                                        if let Some(items) = ws_msg.data {
                                            for item in items {
                                                if let Ok(trade) = Trade::try_from(item) {
                                                    if trade_tx.send(trade).await.is_err() {
                                                        clean_exit = true;
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                            let _ = ws_sink.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await;
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => {
                            tracing::warn!(frame = ?frame, "Kraken WebSocket closed by server");
                            break;
                        }
                        Some(Err(err)) => {
                            tracing::warn!(error = %err, "Kraken WebSocket read error");
                            break;
                        }
                        None => {
                            tracing::warn!("Kraken WebSocket stream ended unexpectedly");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        if clean_exit {
            break;
        }

        tokio::time::sleep(current_backoff).await;
        current_backoff = (current_backoff * 2).min(max_backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_normalize_symbols() {
        assert_eq!(normalize_to_kraken_symbol("BTCEUR"), "BTC/EUR");
        assert_eq!(normalize_to_kraken_symbol("ETHEUR"), "ETH/EUR");
        assert_eq!(normalize_to_kraken_symbol("SOL/EUR"), "SOL/EUR");
        assert_eq!(normalize_to_kraken_symbol("BNBEUR"), "BNB/EUR");
        assert_eq!(normalize_to_kraken_symbol("NEARUSDT"), "NEAR/USDT");

        assert_eq!(normalize_from_kraken_symbol("BTC/EUR"), "BTCEUR");
        assert_eq!(normalize_from_kraken_symbol("SOL/EUR"), "SOLEUR");
    }

    #[test]
    fn test_subscription_payload() {
        let cfg = KrakenWsConfig::for_symbol("BTCEUR");
        assert_eq!(cfg.symbol, "BTC/EUR");
        let payload = cfg.subscription_payload();
        assert!(payload.contains("\"method\":\"subscribe\""));
        assert!(payload.contains("\"channel\":\"trade\""));
        assert!(payload.contains("\"symbol\":[\"BTC/EUR\"]"));
    }

    #[test]
    fn test_parse_kraken_v2_trade_json() {
        let raw_json = r#"{
            "channel": "trade",
            "type": "update",
            "data": [
                {
                    "symbol": "BTC/EUR",
                    "side": "buy",
                    "price": 74500.50,
                    "qty": 0.125,
                    "ord_type": "limit",
                    "trade_id": 987654321,
                    "timestamp": "2026-10-29T14:30:00.500000Z"
                }
            ]
        }"#;

        let msg: KrakenWsMessage = serde_json::from_str(raw_json).expect("valid json");
        assert_eq!(msg.channel.as_deref(), Some("trade"));
        assert_eq!(msg.r#type.as_deref(), Some("update"));

        let items = msg.data.expect("data items exist");
        assert_eq!(items.len(), 1);

        let item = items.into_iter().next().unwrap();
        assert_eq!(item.symbol, "BTC/EUR");
        assert_eq!(item.side, "buy");
        assert_eq!(item.price, dec!(74500.50));
        assert_eq!(item.qty, dec!(0.125));

        let trade = Trade::try_from(item).expect("valid domain trade");
        assert_eq!(trade.price, dec!(74500.50));
        assert_eq!(trade.quantity, dec!(0.125));
        assert_eq!(trade.side, Side::Buy);
        assert_eq!(trade.dollar_value(), dec!(9312.5625));
    }
}
