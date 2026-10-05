//! Binance live execution gateway implementing authenticated REST trading operations.
//!
//! Enforces zero-float precision, cryptographic HMAC-SHA256 signing, and strict
//! pre-trade risk policy gatekeeping before dispatching any live order to the exchange.

use std::str::FromStr;

use domain::{OrderIntent, RiskError, RiskPolicy, Side};
use reqwest::header::{HeaderMap, HeaderValue};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, error, info};

use crate::binance_auth::BinanceAuth;

/// Gateway execution errors.
#[derive(Debug, Error)]
pub enum GatewayError {
    /// Pre-trade risk check failed.
    #[error("Pre-trade risk policy violation: {0}")]
    RiskViolation(#[from] RiskError),
    /// Underlying HTTP request failed.
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),
    /// Exchange API error returned in JSON payload.
    #[error("Binance API error [{code}]: {message}")]
    BinanceApi { code: i64, message: String },
    /// JSON serialization or deserialization failed.
    #[error("Serialization / parsing error: {0}")]
    Serialization(#[from] serde_json::Error),
    /// Decimal parsing failed.
    #[error("Decimal parsing error: {0}")]
    Decimal(#[from] rust_decimal::Error),
    /// Invalid parameters or configuration.
    #[error("Configuration error: {0}")]
    Config(String),
}

/// Normalized order execution report returned by the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderExecutionReport {
    /// Traded asset pair (e.g. BTCUSDT).
    pub symbol: String,
    /// Exchange internal order ID.
    pub order_id: u64,
    /// Client-specified order ID.
    pub client_order_id: String,
    /// Direction: Buy or Sell.
    pub side: Side,
    /// Order status (e.g. "NEW", "FILLED", "PARTIALLY_FILLED", "CANCELED").
    pub status: String,
    /// Limit price or average fill price.
    pub price: Decimal,
    /// Original order quantity.
    pub original_qty: Decimal,
    /// Cumulative executed quantity.
    pub executed_qty: Decimal,
    /// Cumulative quote asset value transacted.
    pub cumulative_quote_qty: Decimal,
    /// Execution timestamp in Unix milliseconds.
    pub timestamp_ms: i64,
}

/// Configuration settings for [`BinanceGateway`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinanceGatewayConfig {
    /// Binance API key.
    pub api_key: String,
    /// Binance secret key.
    pub secret_key: String,
    /// Use official Binance Testnet when true.
    pub testnet: bool,
    /// Target Spot (true) or USD-M Perpetual Futures (false).
    pub spot: bool,
    /// Request timestamp validity window in milliseconds. Default: 5000.
    pub recv_window_ms: u64,
    /// Pre-trade risk policy constraints.
    pub risk_policy: RiskPolicy,
}

impl Default for BinanceGatewayConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            testnet: true,
            spot: true,
            recv_window_ms: 5000,
            risk_policy: RiskPolicy::default(),
        }
    }
}

/// Production and testnet order execution gateway for Binance.
pub struct BinanceGateway {
    config: BinanceGatewayConfig,
    auth: BinanceAuth,
    client: reqwest::Client,
    base_url: String,
}

impl BinanceGateway {
    /// Initializes a new [`BinanceGateway`].
    pub fn new(config: BinanceGatewayConfig) -> Result<Self, GatewayError> {
        let auth = BinanceAuth::new(&config.api_key, &config.secret_key);

        let base_url = if config.spot {
            if config.testnet {
                "https://testnet.binance.vision".to_string()
            } else {
                "https://api.binance.com".to_string()
            }
        } else if config.testnet {
            "https://testnet.binancefuture.com".to_string()
        } else {
            "https://fapi.binance.com".to_string()
        };

        let mut headers = HeaderMap::new();
        if let Ok(key_val) = HeaderValue::from_str(auth.api_key()) {
            headers.insert("X-MBX-APIKEY", key_val);
        }

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        info!(
            base_url = %base_url,
            testnet = config.testnet,
            spot = config.spot,
            "Initialized Binance Execution Gateway"
        );

        Ok(Self {
            config,
            auth,
            client,
            base_url,
        })
    }

    /// Access the underlying pre-trade risk policy.
    pub fn risk_policy(&self) -> &RiskPolicy {
        &self.config.risk_policy
    }

    /// Fetches the free cash balance for a specified asset (e.g. "USDT").
    pub async fn fetch_balance(&self, asset: &str) -> Result<Decimal, GatewayError> {
        let query = self.auth.sign_query("", Some(self.config.recv_window_ms));
        let path = if self.config.spot {
            "/api/v3/account"
        } else {
            "/fapi/v2/account"
        };
        let url = format!("{}{path}?{query}", self.base_url);

        let resp = self.client.get(&url).send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = body
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            return Err(GatewayError::BinanceApi { code, message: msg });
        }

        if self.config.spot {
            if let Some(balances) = body.get("balances").and_then(|b| b.as_array()) {
                for b in balances {
                    if b.get("asset").and_then(|a| a.as_str()) == Some(asset) {
                        let free_str = b.get("free").and_then(|f| f.as_str()).unwrap_or("0");
                        return Ok(Decimal::from_str(free_str)?);
                    }
                }
            }
        } else if let Some(assets) = body.get("assets").and_then(|a| a.as_array()) {
            for a in assets {
                if a.get("asset").and_then(|s| s.as_str()) == Some(asset) {
                    let wallet_str = a
                        .get("availableBalance")
                        .and_then(|v| v.as_str())
                        .unwrap_or("0");
                    return Ok(Decimal::from_str(wallet_str)?);
                }
            }
        }

        Ok(Decimal::ZERO)
    }

    /// Evaluates pre-trade risk checks and submits a signed live order to Binance.
    pub async fn place_order(
        &self,
        symbol: &str,
        intent: &OrderIntent,
        quantity: Decimal,
        current_position_notional: Decimal,
        daily_drawdown_pct: Decimal,
    ) -> Result<OrderExecutionReport, GatewayError> {
        // 1. Mandatory Pre-Trade Risk Gatekeeping
        self.config.risk_policy.evaluate_order(
            intent,
            quantity,
            current_position_notional,
            daily_drawdown_pct,
        )?;

        // 2. Prepare query payload
        let side_str = match intent.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        let raw_query = format!(
            "symbol={symbol}&side={side_str}&type=LIMIT&timeInForce=GTC&quantity={quantity}&price={}",
            intent.price
        );

        let signed_query = self
            .auth
            .sign_query(&raw_query, Some(self.config.recv_window_ms));
        let path = if self.config.spot {
            "/api/v3/order"
        } else {
            "/fapi/v1/order"
        };
        let url = format!("{}{path}?{signed_query}", self.base_url);

        debug!(url = %url, "Dispatching live order to Binance");

        let resp = self.client.post(&url).send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = body
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("Order placement failed")
                .to_string();
            error!(code = code, message = %msg, "Exchange rejected order");
            return Err(GatewayError::BinanceApi { code, message: msg });
        }

        let order_id = body.get("orderId").and_then(|o| o.as_u64()).unwrap_or(0);
        let client_order_id = body
            .get("clientOrderId")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let order_status = body
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("NEW")
            .to_string();
        let orig_qty =
            Decimal::from_str(body.get("origQty").and_then(|q| q.as_str()).unwrap_or("0"))?;
        let exec_qty = Decimal::from_str(
            body.get("executedQty")
                .and_then(|q| q.as_str())
                .unwrap_or("0"),
        )?;
        let cum_quote = Decimal::from_str(
            body.get("cummulativeQuoteQty")
                .and_then(|q| q.as_str())
                .unwrap_or("0"),
        )?;
        let time_ms = body
            .get("transactTime")
            .and_then(|t| t.as_i64())
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64
            });

        info!(
            order_id = order_id,
            status = %order_status,
            symbol = %symbol,
            "Live order successfully accepted by exchange"
        );

        Ok(OrderExecutionReport {
            symbol: symbol.to_string(),
            order_id,
            client_order_id,
            side: intent.side,
            status: order_status,
            price: intent.price,
            original_qty: orig_qty,
            executed_qty: exec_qty,
            cumulative_quote_qty: cum_quote,
            timestamp_ms: time_ms,
        })
    }

    /// Cancels an existing open order on the exchange.
    pub async fn cancel_order(
        &self,
        symbol: &str,
        order_id: u64,
    ) -> Result<OrderExecutionReport, GatewayError> {
        let raw_query = format!("symbol={symbol}&orderId={order_id}");
        let signed_query = self
            .auth
            .sign_query(&raw_query, Some(self.config.recv_window_ms));
        let path = if self.config.spot {
            "/api/v3/order"
        } else {
            "/fapi/v1/order"
        };
        let url = format!("{}{path}?{signed_query}", self.base_url);

        let resp = self.client.delete(&url).send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = body
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("Order cancellation failed")
                .to_string();
            return Err(GatewayError::BinanceApi { code, message: msg });
        }

        let client_order_id = body
            .get("clientOrderId")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let order_status = body
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("CANCELED")
            .to_string();
        let price = Decimal::from_str(body.get("price").and_then(|p| p.as_str()).unwrap_or("0"))?;
        let orig_qty =
            Decimal::from_str(body.get("origQty").and_then(|q| q.as_str()).unwrap_or("0"))?;
        let exec_qty = Decimal::from_str(
            body.get("executedQty")
                .and_then(|q| q.as_str())
                .unwrap_or("0"),
        )?;
        let cum_quote = Decimal::from_str(
            body.get("cummulativeQuoteQty")
                .and_then(|q| q.as_str())
                .unwrap_or("0"),
        )?;
        let side = if body.get("side").and_then(|s| s.as_str()) == Some("BUY") {
            Side::Buy
        } else {
            Side::Sell
        };

        Ok(OrderExecutionReport {
            symbol: symbol.to_string(),
            order_id,
            client_order_id,
            side,
            status: order_status,
            price,
            original_qty: orig_qty,
            executed_qty: exec_qty,
            cumulative_quote_qty: cum_quote,
            timestamp_ms: 0,
        })
    }

    /// Creates a new User Data Stream `listenKey` for streaming execution reports.
    pub async fn create_listen_key(&self) -> Result<String, GatewayError> {
        let path = if self.config.spot {
            "/api/v3/userDataStream"
        } else {
            "/fapi/v1/listenKey"
        };
        let url = format!("{}{path}", self.base_url);

        let resp = self.client.post(&url).send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = body
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("Failed creating listenKey")
                .to_string();
            return Err(GatewayError::BinanceApi { code, message: msg });
        }

        let listen_key = body
            .get("listenKey")
            .and_then(|k| k.as_str())
            .ok_or_else(|| GatewayError::Config("No listenKey field in response".to_string()))?
            .to_string();

        Ok(listen_key)
    }

    /// Pings the `listenKey` to prevent expiration (should be called every ~30 minutes).
    pub async fn keep_alive_listen_key(&self, listen_key: &str) -> Result<(), GatewayError> {
        let path = if self.config.spot {
            "/api/v3/userDataStream"
        } else {
            "/fapi/v1/listenKey"
        };
        let url = format!("{}{path}?listenKey={listen_key}", self.base_url);

        let resp = self.client.put(&url).send().await?;
        if !resp.status().is_success() {
            return Err(GatewayError::Config(
                "Failed to refresh listenKey".to_string(),
            ));
        }
        Ok(())
    }

    /// Base URL configured for this gateway instance.
    #[inline]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Side;
    use rust_decimal_macros::dec;

    #[test]
    fn test_gateway_endpoint_resolution() {
        // Spot Testnet
        let cfg_spot_testnet = BinanceGatewayConfig {
            testnet: true,
            spot: true,
            ..Default::default()
        };
        let gw = BinanceGateway::new(cfg_spot_testnet).unwrap();
        assert_eq!(gw.base_url(), "https://testnet.binance.vision");

        // Spot Production
        let cfg_spot_prod = BinanceGatewayConfig {
            testnet: false,
            spot: true,
            ..Default::default()
        };
        let gw_prod = BinanceGateway::new(cfg_spot_prod).unwrap();
        assert_eq!(gw_prod.base_url(), "https://api.binance.com");

        // Futures Testnet
        let cfg_futures_testnet = BinanceGatewayConfig {
            testnet: true,
            spot: false,
            ..Default::default()
        };
        let gw_fut_testnet = BinanceGateway::new(cfg_futures_testnet).unwrap();
        assert_eq!(
            gw_fut_testnet.base_url(),
            "https://testnet.binancefuture.com"
        );

        // Futures Production
        let cfg_futures_prod = BinanceGatewayConfig {
            testnet: false,
            spot: false,
            ..Default::default()
        };
        let gw_fut_prod = BinanceGateway::new(cfg_futures_prod).unwrap();
        assert_eq!(gw_fut_prod.base_url(), "https://fapi.binance.com");
    }

    #[tokio::test]
    async fn test_pre_trade_risk_rejection_blocks_order_dispatch() {
        let risk_policy = RiskPolicy::new(dec!(20_000.00), dec!(5_000.00), dec!(0.05)).unwrap();
        let config = BinanceGatewayConfig {
            risk_policy,
            ..Default::default()
        };
        let gateway = BinanceGateway::new(config).unwrap();

        let intent = OrderIntent::new(
            1704067200000,
            Side::Buy,
            dec!(60_000.00),
            dec!(59_000.00),
            dec!(62_000.00),
            15,
        )
        .unwrap();

        // 1. Order size exceeds 5,000 (0.1 * 60,000 = 6,000 > 5,000)
        let res = gateway
            .place_order("BTCUSDT", &intent, dec!(0.1), dec!(0.0), dec!(0.01))
            .await;
        assert!(matches!(
            res,
            Err(GatewayError::RiskViolation(
                RiskError::OrderSizeExceeded { .. }
            ))
        ));

        // 2. Circuit breaker blocks order (daily drawdown 6% >= 5% limit)
        let res_circuit = gateway
            .place_order("BTCUSDT", &intent, dec!(0.05), dec!(0.0), dec!(0.06))
            .await;
        assert!(matches!(
            res_circuit,
            Err(GatewayError::RiskViolation(
                RiskError::CircuitBreakerTriggered { .. }
            ))
        ));
    }
}
