//! Binance live execution gateway implementing authenticated REST trading operations.
//!
//! Enforces zero-float precision, cryptographic HMAC-SHA256 signing, and strict
//! pre-trade risk policy gatekeeping before dispatching any live order to the exchange.

use std::collections::HashMap;
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
    /// Spot account state, symbol rules, or commission data could not be validated.
    #[error("Spot pre-trade check failed: {0}")]
    SpotPreflight(String),
    /// The available Spot balance cannot cover the order's required funds.
    #[error("Insufficient Spot funds for {asset}: required {required}, available {available}")]
    InsufficientSpotFunds {
        asset: String,
        required: Decimal,
        available: Decimal,
    },
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

#[derive(Debug)]
struct SpotSymbolInfo {
    base_asset: String,
    quote_asset: String,
}

#[derive(Debug)]
struct CommissionRates {
    maker: Decimal,
    taker: Decimal,
    buyer: Decimal,
    seller: Decimal,
}

impl CommissionRates {
    fn worst_case_for(&self, side: Side) -> Decimal {
        self.maker.max(self.taker)
            + match side {
                Side::Buy => self.buyer,
                Side::Sell => self.seller,
            }
    }
}

#[derive(Debug)]
struct SpotCommissionInfo {
    standard: CommissionRates,
    special: CommissionRates,
    tax: CommissionRates,
    discount_enabled: bool,
    discount_asset: String,
}

impl SpotCommissionInfo {
    fn worst_case_components(&self, side: Side) -> (Decimal, Decimal, Decimal) {
        (
            self.standard.worst_case_for(side),
            self.special.worst_case_for(side),
            self.tax.worst_case_for(side),
        )
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
        if self.config.spot {
            let balances = self.fetch_spot_balances().await?;
            return required_spot_balance(&balances, asset);
        }

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

    async fn get_binance_json(
        &self,
        path_and_query: &str,
    ) -> Result<serde_json::Value, GatewayError> {
        let url = format!("{}{}", self.base_url, path_and_query);
        let response = self.client.get(&url).send().await?;
        let status = response.status();
        let body: serde_json::Value = response.json().await?;
        if !status.is_success() {
            let code = body
                .get("code")
                .and_then(|value| value.as_i64())
                .unwrap_or(-1);
            let message = body
                .get("msg")
                .and_then(|value| value.as_str())
                .unwrap_or("Binance request failed")
                .to_string();
            return Err(GatewayError::BinanceApi { code, message });
        }
        Ok(body)
    }

    async fn fetch_spot_balances(&self) -> Result<HashMap<String, Decimal>, GatewayError> {
        let query = self
            .auth
            .sign_query("omitZeroBalances=false", Some(self.config.recv_window_ms));
        let body = self
            .get_binance_json(&format!("/api/v3/account?{query}"))
            .await?;
        let balances = body
            .get("balances")
            .and_then(|value| value.as_array())
            .ok_or_else(|| {
                GatewayError::SpotPreflight(
                    "account response is missing the balances array".to_string(),
                )
            })?;

        let mut parsed = HashMap::with_capacity(balances.len());
        for balance in balances {
            let asset = balance
                .get("asset")
                .and_then(|value| value.as_str())
                .filter(|asset| is_valid_asset(asset))
                .ok_or_else(|| {
                    GatewayError::SpotPreflight(
                        "account response contains a missing or invalid asset".to_string(),
                    )
                })?;
            let free = parse_decimal_field(balance, "free", "account free balance")?;
            if free < Decimal::ZERO {
                return Err(GatewayError::SpotPreflight(format!(
                    "account response contains a negative free balance for {asset}"
                )));
            }
            if parsed.insert(asset.to_string(), free).is_some() {
                return Err(GatewayError::SpotPreflight(format!(
                    "account response contains duplicate balances for {asset}"
                )));
            }
        }
        Ok(parsed)
    }

    async fn fetch_spot_symbol_info(&self, symbol: &str) -> Result<SpotSymbolInfo, GatewayError> {
        if !is_valid_asset(symbol) {
            return Err(GatewayError::SpotPreflight(
                "symbol must contain only ASCII letters and digits".to_string(),
            ));
        }
        let body = self
            .get_binance_json(&format!("/api/v3/exchangeInfo?symbol={symbol}"))
            .await?;
        let symbols = body
            .get("symbols")
            .and_then(|value| value.as_array())
            .filter(|symbols| symbols.len() == 1)
            .ok_or_else(|| {
                GatewayError::SpotPreflight(
                    "exchange information did not contain exactly one symbol".to_string(),
                )
            })?;
        let item = &symbols[0];
        if item.get("symbol").and_then(|value| value.as_str()) != Some(symbol)
            || item.get("status").and_then(|value| value.as_str()) != Some("TRADING")
            || item
                .get("isSpotTradingAllowed")
                .and_then(|value| value.as_bool())
                != Some(true)
        {
            return Err(GatewayError::SpotPreflight(format!(
                "{symbol} is not an active standard Spot symbol"
            )));
        }
        let base_asset = required_asset_field(item, "baseAsset", "exchange symbol")?;
        let quote_asset = required_asset_field(item, "quoteAsset", "exchange symbol")?;
        Ok(SpotSymbolInfo {
            base_asset,
            quote_asset,
        })
    }

    async fn fetch_spot_commission_info(
        &self,
        symbol: &str,
    ) -> Result<SpotCommissionInfo, GatewayError> {
        let raw_query = format!("symbol={symbol}");
        let query = self
            .auth
            .sign_query(&raw_query, Some(self.config.recv_window_ms));
        let body = self
            .get_binance_json(&format!("/api/v3/account/commission?{query}"))
            .await?;
        if body.get("symbol").and_then(|value| value.as_str()) != Some(symbol) {
            return Err(GatewayError::SpotPreflight(
                "commission response did not match the requested symbol".to_string(),
            ));
        }
        let discount_value = body.get("discount").ok_or_else(|| {
            GatewayError::SpotPreflight(
                "commission response is missing discount settings".to_string(),
            )
        })?;
        let discount = discount_value.as_object().ok_or_else(|| {
            GatewayError::SpotPreflight(
                "commission response is missing discount settings".to_string(),
            )
        })?;
        let account_discount = discount
            .get("enabledForAccount")
            .and_then(|value| value.as_bool())
            .ok_or_else(|| {
                GatewayError::SpotPreflight(
                    "commission discount account flag is missing or malformed".to_string(),
                )
            })?;
        let symbol_discount = discount
            .get("enabledForSymbol")
            .and_then(|value| value.as_bool())
            .ok_or_else(|| {
                GatewayError::SpotPreflight(
                    "commission discount symbol flag is missing or malformed".to_string(),
                )
            })?;
        let discount_asset = required_asset_field(discount_value, "discountAsset", "commission")?;
        let discount_factor =
            parse_decimal_field(discount_value, "discount", "commission discount")?;
        if !(Decimal::ZERO..=Decimal::ONE).contains(&discount_factor) {
            return Err(GatewayError::SpotPreflight(
                "commission discount is outside the supported range".to_string(),
            ));
        }

        Ok(SpotCommissionInfo {
            standard: parse_commission_rates(&body, "standardCommission")?,
            special: parse_commission_rates(&body, "specialCommission")?,
            tax: parse_commission_rates(&body, "taxCommission")?,
            discount_enabled: account_discount && symbol_discount,
            discount_asset,
        })
    }

    async fn fetch_spot_price(&self, symbol: &str) -> Result<Decimal, GatewayError> {
        let body = self
            .get_binance_json(&format!("/api/v3/ticker/price?symbol={symbol}"))
            .await?;
        let price = parse_decimal_field(&body, "price", "ticker price")?;
        if price <= Decimal::ZERO {
            return Err(GatewayError::SpotPreflight(
                "ticker price must be positive".to_string(),
            ));
        }
        Ok(price)
    }

    async fn fetch_discount_asset_price_in_quote(
        &self,
        discount_asset: &str,
        symbol: &SpotSymbolInfo,
        intent: &OrderIntent,
    ) -> Result<Decimal, GatewayError> {
        if discount_asset == symbol.quote_asset {
            return Ok(Decimal::ONE);
        }
        if discount_asset == symbol.base_asset {
            return Ok(intent.price);
        }
        let direct = format!("{discount_asset}{}", symbol.quote_asset);
        match self.fetch_spot_price(&direct).await {
            Ok(price) => return Ok(price),
            Err(GatewayError::BinanceApi { code: -1121, .. }) => {}
            Err(error) => return Err(error),
        }
        let inverse = format!("{}{discount_asset}", symbol.quote_asset);
        let inverse_price = self.fetch_spot_price(&inverse).await.map_err(|_| {
            GatewayError::SpotPreflight(format!(
                "cannot value commission asset {discount_asset} in {}",
                symbol.quote_asset
            ))
        })?;
        Ok(Decimal::ONE / inverse_price)
    }

    async fn validate_spot_preflight(
        &self,
        symbol: &str,
        intent: &OrderIntent,
        quantity: Decimal,
    ) -> Result<(), GatewayError> {
        let symbol_info = self.fetch_spot_symbol_info(symbol).await?;
        let balances = self.fetch_spot_balances().await?;
        let notional = intent.price * quantity;
        let (required_asset, required_amount, received_amount) = match intent.side {
            Side::Buy => (&symbol_info.quote_asset, notional, quantity),
            Side::Sell => (&symbol_info.base_asset, quantity, notional),
        };
        let available = required_spot_balance(&balances, required_asset)?;
        if available < required_amount {
            return Err(GatewayError::InsufficientSpotFunds {
                asset: required_asset.clone(),
                required: required_amount,
                available,
            });
        }

        let commission = self.fetch_spot_commission_info(symbol).await?;
        let (standard_rate, special_rate, tax_rate) = commission.worst_case_components(intent.side);
        let total_rate = standard_rate + special_rate + tax_rate;
        if total_rate < Decimal::ZERO || total_rate >= Decimal::ONE {
            return Err(GatewayError::SpotPreflight(
                "commission rate cannot be safely reserved from received funds".to_string(),
            ));
        }
        let received_fee = received_amount * total_rate;

        if commission.discount_enabled && total_rate > Decimal::ZERO {
            let discount_asset_balance =
                required_spot_balance(&balances, &commission.discount_asset)?;
            if discount_asset_balance > Decimal::ZERO {
                let asset_price = self
                    .fetch_discount_asset_price_in_quote(
                        &commission.discount_asset,
                        &symbol_info,
                        intent,
                    )
                    .await?;
                let fee_in_quote = received_fee
                    * if intent.side == Side::Buy {
                        intent.price
                    } else {
                        Decimal::ONE
                    };
                // Reserve against the undiscounted upper bound. Binance may instead debit
                // the received asset when the discount-asset balance cannot cover the fee.
                let required_discount_asset = fee_in_quote / asset_price;
                if discount_asset_balance >= required_discount_asset {
                    return Ok(());
                }
            }
        }

        if received_fee >= received_amount {
            return Err(GatewayError::SpotPreflight(
                "received asset cannot cover the estimated commission".to_string(),
            ));
        }
        Ok(())
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

        if self.config.spot {
            self.validate_spot_preflight(symbol, intent, quantity)
                .await?;
        }

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

fn is_valid_asset(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn required_asset_field(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<String, GatewayError> {
    let asset = body
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| is_valid_asset(value))
        .ok_or_else(|| {
            GatewayError::SpotPreflight(format!(
                "{context} response is missing or has an invalid {key}"
            ))
        })?;
    Ok(asset.to_string())
}

fn parse_decimal_field(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<Decimal, GatewayError> {
    let value = body.get(key).ok_or_else(|| {
        GatewayError::SpotPreflight(format!("{context} response is missing {key}"))
    })?;
    let raw = value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_number().map(ToString::to_string))
        .ok_or_else(|| {
            GatewayError::SpotPreflight(format!("{context} response has malformed {key}"))
        })?;
    Decimal::from_str(&raw)
        .or_else(|_| Decimal::from_scientific(&raw))
        .map_err(GatewayError::Decimal)
}

fn parse_commission_rates(
    body: &serde_json::Value,
    group: &str,
) -> Result<CommissionRates, GatewayError> {
    let values = body.get(group).ok_or_else(|| {
        GatewayError::SpotPreflight(format!("commission response is missing {group}"))
    })?;
    let rates = CommissionRates {
        maker: parse_decimal_field(values, "maker", group)?,
        taker: parse_decimal_field(values, "taker", group)?,
        buyer: parse_decimal_field(values, "buyer", group)?,
        seller: parse_decimal_field(values, "seller", group)?,
    };
    if [rates.maker, rates.taker, rates.buyer, rates.seller]
        .into_iter()
        .any(|rate| rate < Decimal::ZERO)
    {
        return Err(GatewayError::SpotPreflight(format!(
            "commission response contains a negative rate in {group}"
        )));
    }
    Ok(rates)
}

fn required_spot_balance(
    balances: &HashMap<String, Decimal>,
    asset: &str,
) -> Result<Decimal, GatewayError> {
    balances.get(asset).copied().ok_or_else(|| {
        GatewayError::SpotPreflight(format!(
            "account response is missing the required {asset} balance"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Side;
    use rust_decimal_macros::dec;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    const VALID_COMMISSION_RESPONSE: &str = r#"{"symbol":"BTCUSDT","standardCommission":{"maker":"0.001","taker":"0.002","buyer":"0.001","seller":"0.001"},"specialCommission":{"maker":"0","taker":"0","buyer":"0","seller":"0"},"taxCommission":{"maker":0.0001,"taker":"0","buyer":"0","seller":"0"},"discount":{"enabledForAccount":true,"enabledForSymbol":true,"discountAsset":"BNB","discount":"0.25"}}"#;

    fn spawn_spot_mock_server(
        account_response: &'static str,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        spawn_spot_mock_server_with_commission(account_response, VALID_COMMISSION_RESPONSE)
    }

    fn spawn_spot_mock_server_with_commission(
        account_response: &'static str,
        commission_response: &'static str,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);

        let handle = thread::spawn(move || {
            let mut last_request = Instant::now();
            while last_request.elapsed() < Duration::from_millis(150) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let mut request = Vec::new();
                let mut chunk = [0_u8; 2048];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            request.extend_from_slice(&chunk[..read]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                let request_line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                captured.lock().unwrap().push(request_line.clone());
                last_request = Instant::now();

                let path = request_line.split_whitespace().nth(1).unwrap_or_default();
                let body = if path.starts_with("/api/v3/exchangeInfo") {
                    r#"{"symbols":[{"symbol":"BTCUSDT","status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT","isSpotTradingAllowed":true}]}"#
                } else if path.starts_with("/api/v3/account?") {
                    account_response
                } else if path.starts_with("/api/v3/account/commission?") {
                    commission_response
                } else if path.starts_with("/api/v3/ticker/price?") {
                    r#"{"symbol":"BNBUSDT","price":"100"}"#
                } else if request_line.starts_with("POST /api/v3/order") {
                    r#"{"orderId":1,"clientOrderId":"test-order","status":"NEW","origQty":"1","executedQty":"0","cummulativeQuoteQty":"0","transactTime":1}"#
                } else {
                    r#"{"code":-1121,"msg":"Invalid mock request"}"#
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        (format!("http://{address}"), requests, handle)
    }

    #[tokio::test]
    async fn spot_order_preflight_rejects_insufficient_or_malformed_funds_without_dispatch() {
        let cases = [
            (
                "buy without enough quote funds",
                Side::Buy,
                r#"{"balances":[{"asset":"USDT","free":"49","locked":"0"},{"asset":"BTC","free":"0","locked":"0"}]}"#,
            ),
            (
                "sell without enough base inventory",
                Side::Sell,
                r#"{"balances":[{"asset":"USDT","free":"1000","locked":"0"},{"asset":"BTC","free":"0.5","locked":"0"}]}"#,
            ),
            (
                "malformed balance state",
                Side::Buy,
                r#"{"balances":[{"asset":"USDT","locked":"0"},{"asset":"BTC","free":"0","locked":"0"}]}"#,
            ),
        ];

        for (name, side, account_response) in cases {
            let (base_url, requests, server) = spawn_spot_mock_server(account_response);
            let mut gateway = BinanceGateway::new(BinanceGatewayConfig::default()).unwrap();
            gateway.base_url = base_url;
            let intent = OrderIntent::new(1, side, dec!(50), dec!(49), dec!(51), 5).unwrap();

            let result = gateway
                .place_order("BTCUSDT", &intent, dec!(1), Decimal::ZERO, Decimal::ZERO)
                .await;

            assert!(result.is_err(), "{name} must be rejected before dispatch");
            server.join().unwrap();
            assert!(
                !requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request.starts_with("POST /api/v3/order")),
                "{name} must not dispatch an order"
            );
        }

        let account_response = r#"{"balances":[{"asset":"USDT","free":"1000","locked":"0"},{"asset":"BTC","free":"0","locked":"0"},{"asset":"BNB","free":"0","locked":"0"}]}"#;
        let (base_url, requests, server) =
            spawn_spot_mock_server_with_commission(account_response, r#"{"symbol":"BTCUSDT"}"#);
        let mut gateway = BinanceGateway::new(BinanceGatewayConfig::default()).unwrap();
        gateway.base_url = base_url;
        let intent = OrderIntent::new(1, Side::Buy, dec!(50), dec!(49), dec!(51), 5).unwrap();
        let result = gateway
            .place_order("BTCUSDT", &intent, dec!(1), Decimal::ZERO, Decimal::ZERO)
            .await;

        assert!(result.is_err(), "missing commission rates must fail closed");
        server.join().unwrap();
        assert!(
            !requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.starts_with("POST /api/v3/order")),
            "missing commission rates must not dispatch an order"
        );

        let account_response = r#"{"balances":[{"asset":"USDT","free":"1000","locked":"0"},{"asset":"BTC","free":"0","locked":"0"},{"asset":"BNB","free":"1","locked":"0"}]}"#;
        let (base_url, requests, server) = spawn_spot_mock_server(account_response);
        let mut gateway = BinanceGateway::new(BinanceGatewayConfig::default()).unwrap();
        gateway.base_url = base_url;
        let intent = OrderIntent::new(1, Side::Buy, dec!(50), dec!(49), dec!(51), 5).unwrap();
        let result = gateway
            .place_order("BTCUSDT", &intent, dec!(1), Decimal::ZERO, Decimal::ZERO)
            .await;

        assert!(
            result.is_ok(),
            "funded buy should pass its Spot preflight: {result:?}"
        );
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests
            .iter()
            .any(|request| request.starts_with("GET /api/v3/account/commission?")));
        assert!(requests
            .iter()
            .any(|request| request.starts_with("GET /api/v3/ticker/price?")));
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /api/v3/order"))
                .count(),
            1,
            "a funded order should be dispatched exactly once"
        );
    }

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
