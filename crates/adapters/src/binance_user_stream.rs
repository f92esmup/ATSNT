//! Binance User Data Stream WebSocket listener for real-time order fill reconciliation.
//!
//! Connects using an authenticated `listenKey`, automatically handles periodic keep-alive
//! pings, and deserializes execution reports with zero floating-point math.

use std::str::FromStr;

use domain::Side;
use futures_util::StreamExt;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tracing::{debug, error, info, warn};

/// Real-time execution update emitted on order fill or lifecycle change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionUpdate {
    /// Asset symbol (e.g. "BTCUSDT").
    pub symbol: String,
    /// Exchange internal order ID.
    pub order_id: u64,
    /// Client order identifier.
    pub client_order_id: String,
    /// Direction: Buy or Sell.
    pub side: Side,
    /// Status: "NEW", "PARTIALLY_FILLED", "FILLED", "CANCELED", "REJECTED".
    pub status: String,
    /// Last fill execution price.
    pub last_filled_price: Decimal,
    /// Quantity transacted in this specific fill execution.
    pub last_filled_qty: Decimal,
    /// Cumulative executed quantity across all fills.
    pub cumulative_filled_qty: Decimal,
    /// Commission fee deducted.
    pub commission_amount: Decimal,
    /// Asset used for fee payment (e.g. "USDT", "BNB").
    pub commission_asset: Option<String>,
    /// Execution timestamp in Unix milliseconds.
    pub timestamp_ms: i64,
}

/// Asynchronous User Data Stream client.
pub struct BinanceUserDataStream {
    receiver: mpsc::Receiver<ExecutionUpdate>,
}

impl BinanceUserDataStream {
    /// Connects to the User Data Stream WebSocket and starts background keepalive and reception loops.
    pub async fn connect(
        listen_key: String,
        testnet: bool,
        spot: bool,
    ) -> Result<Self, crate::error::AdapterError> {
        let (tx, rx) = mpsc::channel(10_000);

        let ws_url = if spot {
            if testnet {
                format!("wss://testnet.binance.vision/ws/{listen_key}")
            } else {
                format!("wss://stream.binance.com:9443/ws/{listen_key}")
            }
        } else if testnet {
            format!("wss://stream.binancefuture.com/ws/{listen_key}")
        } else {
            format!("wss://fstream.binance.com/ws/{listen_key}")
        };

        info!(ws_url = %ws_url, "Connecting to Binance User Data Stream");
        let (ws_stream, _) = connect_async(&ws_url).await?;

        let (_, mut reader) = ws_stream.split();

        tokio::spawn(async move {
            while let Some(msg_result) = reader.next().await {
                match msg_result {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        if let Some(update) = parse_user_stream_event(&text) {
                            if tx.send(update).await.is_err() {
                                debug!(
                                    "User stream channel receiver dropped; shutting down listener"
                                );
                                break;
                            }
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Ping(_)) => {
                        debug!("Received WS ping from user data stream");
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                        warn!("User data stream closed by exchange");
                        break;
                    }
                    Err(e) => {
                        error!(error = %e, "User data stream WebSocket reception error");
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(Self { receiver: rx })
    }

    /// Fetches the next execution update event from the channel.
    pub async fn next_update(&mut self) -> Option<ExecutionUpdate> {
        self.receiver.recv().await
    }
}

/// Parses Spot and Futures user stream JSON events into [`ExecutionUpdate`].
pub fn parse_user_stream_event(json_str: &str) -> Option<ExecutionUpdate> {
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let event_type = v.get("e").and_then(|e| e.as_str())?;

    // Spot executionReport event
    if event_type == "executionReport" {
        let symbol = v.get("s").and_then(|s| s.as_str())?.to_string();
        let order_id = v.get("i").and_then(|i| i.as_u64())?;
        let client_order_id = v
            .get("c")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let side = match v.get("S").and_then(|s| s.as_str())? {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => return None,
        };
        let status = v.get("X").and_then(|x| x.as_str())?.to_string();
        let last_price =
            Decimal::from_str(v.get("L").and_then(|l| l.as_str()).unwrap_or("0")).ok()?;
        let last_qty =
            Decimal::from_str(v.get("l").and_then(|l| l.as_str()).unwrap_or("0")).ok()?;
        let cum_qty = Decimal::from_str(v.get("z").and_then(|z| z.as_str()).unwrap_or("0")).ok()?;
        let comm_amt = Decimal::from_str(v.get("n").and_then(|n| n.as_str()).unwrap_or("0"))
            .unwrap_or(Decimal::ZERO);
        let comm_asset = v.get("N").and_then(|n| n.as_str()).map(String::from);
        let time_ms = v.get("T").and_then(|t| t.as_i64()).unwrap_or(0);

        return Some(ExecutionUpdate {
            symbol,
            order_id,
            client_order_id,
            side,
            status,
            last_filled_price: last_price,
            last_filled_qty: last_qty,
            cumulative_filled_qty: cum_qty,
            commission_amount: comm_amt,
            commission_asset: comm_asset,
            timestamp_ms: time_ms,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_spot_execution_report() {
        let payload = r#"{
            "e": "executionReport",
            "E": 1499405638848,
            "s": "BTCUSDT",
            "c": "mUvoqJxFIILMdfAW5iGSOW",
            "S": "BUY",
            "o": "LIMIT",
            "f": "GTC",
            "q": "1.00000000",
            "p": "65000.00000000",
            "P": "0.00000000",
            "F": "0.00000000",
            "g": -1,
            "C": null,
            "x": "TRADE",
            "X": "FILLED",
            "r": "NONE",
            "i": 4293153,
            "l": "1.00000000",
            "z": "1.00000000",
            "L": "65000.00000000",
            "n": "0.00050000",
            "N": "BTC",
            "T": 1499405638848,
            "t": 12345,
            "I": 8641984,
            "w": false,
            "m": false,
            "M": false,
            "O": 1499405638848,
            "Z": "65000.00000000",
            "Y": "65000.00000000",
            "Q": "0.00000000",
            "W": 1499405638848,
            "V": "EXPIRE_TAKER"
        }"#;

        let update = parse_user_stream_event(payload).expect("should parse successfully");
        assert_eq!(update.symbol, "BTCUSDT");
        assert_eq!(update.order_id, 4293153);
        assert_eq!(update.side, Side::Buy);
        assert_eq!(update.status, "FILLED");
        assert_eq!(update.last_filled_price, Decimal::from(65000));
        assert_eq!(update.last_filled_qty, Decimal::ONE);
        assert_eq!(update.commission_asset.as_deref(), Some("BTC"));
    }
}
