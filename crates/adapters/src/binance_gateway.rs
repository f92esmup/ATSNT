//! Binance live execution gateway implementing authenticated REST trading operations.
//!
//! Enforces zero-float precision, cryptographic HMAC-SHA256 signing, and strict
//! pre-trade risk policy gatekeeping before dispatching any live order to the exchange.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;

use domain::{OrderIntent, RiskError, RiskPolicy, Side};
use reqwest::header::{HeaderMap, HeaderValue};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, error, info};

use crate::binance_auth::BinanceAuth;
use crate::binance_private_stream::ListenKeyRenewer;

const USER_STREAM_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[path = "binance_gateway/exits.rs"]
mod exits;
pub use exits::{ExitKind, PositionClosureReport, ProtectiveExitReport};
#[path = "binance_gateway/account_check.rs"]
mod account_check;
pub use account_check::{AccountFix, AccountReport, Finding, FindingStatus};

#[cfg(test)]
#[path = "binance_gateway/t6_tests.rs"]
mod t6_listen_key_tests;

static NEXT_FUTURES_CLIENT_ORDER_ID: AtomicU64 = AtomicU64::new(1);

/// Gateway execution errors.
#[derive(Debug, Error)]
pub enum GatewayError {
    /// Pre-trade risk check failed.
    #[error("Pre-trade risk policy violation: {0}")]
    RiskViolation(#[from] RiskError),
    /// Spot account state, symbol rules, or commission data could not be validated.
    #[error("Spot pre-trade check failed: {0}")]
    SpotPreflight(String),
    /// USD-M account state, contract rules, margin, or fee data could not be validated.
    #[error("USD-M pre-trade check failed: {0}")]
    FuturesPreflight(String),
    /// Another USD-M order is currently being reconciled by this gateway.
    #[error("USD-M order submission is already in progress (client order ID {client_order_id})")]
    FuturesSubmissionInProgress { client_order_id: String },
    /// Binance could not confirm whether the USD-M order was accepted; automatic retries are unsafe.
    #[error(
        "USD-M order submission is uncertain for {symbol} (client order ID {client_order_id})"
    )]
    FuturesSubmissionUncertain {
        symbol: String,
        client_order_id: String,
    },
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
    /// Local wall-clock time the order request was sent, in Unix milliseconds (0 when the report
    /// did not come from a fresh submission).
    #[serde(default)]
    pub sent_at_ms: i64,
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
    /// Fixed fractional equity risked per trade (e.g. 0.01 = 1%), shared with the simulator.
    pub risk_per_trade_pct: Decimal,
    /// Accept non-USDT balances in the Futures wallet instead of refusing the account. Only
    /// allowed on testnet, where Binance credits every account with BTC and USDC that cannot be
    /// removed; [`BinanceGateway::new`] refuses it anywhere else.
    #[serde(default)]
    pub allow_foreign_collateral: bool,
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
            risk_per_trade_pct: Decimal::new(1, 2),
            allow_foreign_collateral: false,
        }
    }
}

#[derive(Debug)]
struct SpotSymbolInfo {
    base_asset: String,
    quote_asset: String,
    /// `(min_qty, step_size, min_notional)` from the exchange filters, when all are published.
    lot_rules: Option<(Decimal, Decimal, Decimal)>,
}

const FUTURES_MAX_DATA_AGE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug)]
struct FuturesSymbolRules {
    min_price: Decimal,
    max_price: Decimal,
    tick_size: Decimal,
    min_qty: Decimal,
    max_qty: Decimal,
    step_size: Decimal,
    min_notional: Decimal,
}

#[derive(Debug)]
struct FuturesLeverageBracket {
    notional_floor: Decimal,
    notional_cap: Decimal,
    maintenance_rate: Decimal,
    maintenance_amount: Decimal,
    max_leverage: u32,
}

#[derive(Debug)]
struct FuturesPreflightSnapshot {
    observed_at: [std::time::Instant; 5],
    available_balance: Decimal,
    position_notional: Decimal,
    isolated_wallet: Decimal,
    current_leverage: u32,
    maximum_notional: Decimal,
    margin_type: String,
    open_order_notional: Decimal,
    has_position: bool,
    has_open_orders: bool,
    mark_time_ms: u64,
    /// Local wall-clock time at which the mark price response was received. The mark's age is
    /// judged then; the time since is bounded by the snapshot age guard.
    mark_received_ms: u64,
    rules: FuturesSymbolRules,
    leverage_brackets: Vec<FuturesLeverageBracket>,
    taker_commission_rate: Decimal,
}

#[derive(Debug)]
enum FuturesSubmissionState {
    Available,
    Reserved { client_order_id: String },
    Dispatching { order: FuturesOrderRequest },
    Uncertain { order: FuturesOrderRequest },
}

#[derive(Debug, Clone)]
struct FuturesOrderRequest {
    symbol: String,
    client_order_id: String,
    side: Side,
    price: Decimal,
    quantity: Decimal,
}

enum FuturesOrderLookup {
    Found(OrderExecutionReport),
    NotFound,
    Unavailable,
}

struct FuturesSubmissionLease<'a> {
    gateway: &'a BinanceGateway,
    client_order_id: String,
}

impl FuturesSubmissionLease<'_> {
    fn mark_dispatching(&self, order: FuturesOrderRequest) -> Result<(), GatewayError> {
        let mut state = self.gateway.futures_submission_state.lock().map_err(|_| {
            GatewayError::FuturesPreflight(
                "USD-M submission reservation state is unavailable".to_string(),
            )
        })?;
        if matches!(
            &*state,
            FuturesSubmissionState::Reserved { client_order_id }
                if client_order_id == &self.client_order_id
        ) {
            *state = FuturesSubmissionState::Dispatching { order };
            return Ok(());
        }
        Err(GatewayError::FuturesPreflight(
            "USD-M submission reservation changed before dispatch".to_string(),
        ))
    }

    fn mark_resolved(&self) {
        if let Ok(mut state) = self.gateway.futures_submission_state.lock() {
            let matches_order = match &*state {
                FuturesSubmissionState::Reserved { client_order_id } => {
                    client_order_id == &self.client_order_id
                }
                FuturesSubmissionState::Dispatching { order }
                | FuturesSubmissionState::Uncertain { order } => {
                    order.client_order_id == self.client_order_id
                }
                FuturesSubmissionState::Available => false,
            };
            if matches_order {
                *state = FuturesSubmissionState::Available;
            }
        }
    }

    fn mark_uncertain(&self, order: FuturesOrderRequest) {
        if let Ok(mut state) = self.gateway.futures_submission_state.lock() {
            if matches!(
                &*state,
                FuturesSubmissionState::Dispatching { order: current }
                    if current.client_order_id == self.client_order_id
            ) {
                *state = FuturesSubmissionState::Uncertain { order };
            }
        }
    }
}

impl Drop for FuturesSubmissionLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gateway.futures_submission_state.lock() {
            match &*state {
                FuturesSubmissionState::Reserved { client_order_id }
                    if client_order_id == &self.client_order_id =>
                {
                    *state = FuturesSubmissionState::Available;
                }
                FuturesSubmissionState::Dispatching { order }
                    if order.client_order_id == self.client_order_id =>
                {
                    *state = FuturesSubmissionState::Uncertain {
                        order: order.clone(),
                    };
                }
                _ => {}
            }
        }
    }
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
    futures_submission_state: StdMutex<FuturesSubmissionState>,
}

impl BinanceGateway {
    /// Initializes a new [`BinanceGateway`].
    pub fn new(config: BinanceGatewayConfig) -> Result<Self, GatewayError> {
        // The tolerance exists for the default testnet account only; a real account that holds
        // foreign collateral must be fixed, not tolerated.
        if config.allow_foreign_collateral && (!config.testnet || config.spot) {
            return Err(GatewayError::Config(
                "allow_foreign_collateral is only available for USD-M futures on testnet"
                    .to_string(),
            ));
        }
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
            futures_submission_state: StdMutex::new(FuturesSubmissionState::Available),
        })
    }

    fn reserve_futures_submission(&self) -> Result<FuturesSubmissionLease<'_>, GatewayError> {
        let client_order_id = new_futures_client_order_id()?;
        let mut state = self.futures_submission_state.lock().map_err(|_| {
            GatewayError::FuturesPreflight(
                "USD-M submission reservation state is unavailable".to_string(),
            )
        })?;

        match &*state {
            FuturesSubmissionState::Available => {
                *state = FuturesSubmissionState::Reserved {
                    client_order_id: client_order_id.clone(),
                };
                Ok(FuturesSubmissionLease {
                    gateway: self,
                    client_order_id,
                })
            }
            FuturesSubmissionState::Reserved { client_order_id } => {
                Err(GatewayError::FuturesSubmissionInProgress {
                    client_order_id: client_order_id.clone(),
                })
            }
            FuturesSubmissionState::Dispatching { order } => {
                Err(GatewayError::FuturesSubmissionInProgress {
                    client_order_id: order.client_order_id.clone(),
                })
            }
            FuturesSubmissionState::Uncertain { order } => {
                Err(GatewayError::FuturesSubmissionUncertain {
                    symbol: order.symbol.clone(),
                    client_order_id: order.client_order_id.clone(),
                })
            }
        }
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

    fn signed_futures_path(&self, path: &str, parameters: &str) -> String {
        let query = self
            .auth
            .sign_query(parameters, Some(self.config.recv_window_ms));
        format!("{path}?{query}")
    }

    async fn get_observed_binance_json(
        &self,
        path_and_query: &str,
    ) -> Result<(serde_json::Value, std::time::Instant), GatewayError> {
        let observed_at = std::time::Instant::now();
        let body = self.get_binance_json(path_and_query).await?;
        Ok((body, observed_at))
    }

    async fn post_signed_futures_json(
        &self,
        path: &str,
        parameters: &str,
    ) -> Result<serde_json::Value, GatewayError> {
        let query = self
            .auth
            .sign_query(parameters, Some(self.config.recv_window_ms));
        let url = format!("{}{path}?{query}", self.base_url);
        let response = self.client.post(&url).send().await?;
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
                .unwrap_or("Binance Futures configuration request failed")
                .to_string();
            return Err(GatewayError::BinanceApi { code, message });
        }
        Ok(body)
    }

    async fn submit_futures_order(
        &self,
        symbol: &str,
        intent: &OrderIntent,
        quantity: Decimal,
        snapshot: &FuturesPreflightSnapshot,
        lease: &FuturesSubmissionLease<'_>,
    ) -> Result<OrderExecutionReport, GatewayError> {
        let side = match intent.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };
        // Entries are market orders (taker): the strategy signals on a closed bar, so a resting
        // limit at the bar close would fill only when the signal is wrong. Binance reports a
        // market order with price 0; the intent price is only a sizing and sanity reference.
        let order = FuturesOrderRequest {
            symbol: symbol.to_string(),
            client_order_id: lease.client_order_id.clone(),
            side: intent.side,
            price: Decimal::ZERO,
            quantity,
        };
        let parameters = format!(
            "symbol={}&side={side}&type=MARKET&quantity={quantity}&newClientOrderId={}",
            order.symbol, order.client_order_id
        );
        let signed_query = self
            .auth
            .sign_query(&parameters, Some(self.config.recv_window_ms));
        let url = format!("{}/fapi/v1/order?{signed_query}", self.base_url);

        validate_futures_snapshot_age(&snapshot.observed_at, std::time::Instant::now())?;
        validate_futures_mark_timestamp(snapshot.mark_time_ms, snapshot.mark_received_ms)?;

        lease.mark_dispatching(order.clone())?;
        let sent_at_ms = current_unix_time_ms()? as i64;
        let response = match self.client.post(&url).send().await {
            Ok(response) => response,
            Err(_) => return self.reconcile_futures_submission(order, lease).await,
        };
        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => return self.reconcile_futures_submission(order, lease).await,
        };
        let body = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(body) => body,
            Err(_) => return self.reconcile_futures_submission(order, lease).await,
        };

        if !status.is_success() {
            let code = body.get("code").and_then(|value| value.as_i64());
            let message = body
                .get("msg")
                .and_then(|value| value.as_str())
                .unwrap_or("USD-M order placement failed")
                .to_string();
            if status.is_server_error()
                || code.is_none()
                || matches!(code, Some(-1000 | -1001 | -1004 | -1006 | -1007))
            {
                return self.reconcile_futures_submission(order, lease).await;
            }
            if status.is_client_error() {
                error!(code = code.unwrap_or(-1), message = %message, "Exchange rejected USD-M order");
                lease.mark_resolved();
                return Err(GatewayError::BinanceApi {
                    code: code.unwrap_or(-1),
                    message,
                });
            }
            return self.reconcile_futures_submission(order, lease).await;
        }

        match parse_futures_order_report(&body, &order) {
            Ok(report) => {
                lease.mark_resolved();
                Ok(OrderExecutionReport {
                    sent_at_ms,
                    ..report
                })
            }
            Err(_) => self.reconcile_futures_submission(order, lease).await,
        }
    }

    async fn reconcile_futures_submission(
        &self,
        order: FuturesOrderRequest,
        lease: &FuturesSubmissionLease<'_>,
    ) -> Result<OrderExecutionReport, GatewayError> {
        if let Some(report) = self.lookup_futures_order_until_resolved(&order).await {
            lease.mark_resolved();
            return Ok(report);
        }

        lease.mark_uncertain(order.clone());
        Err(GatewayError::FuturesSubmissionUncertain {
            symbol: order.symbol,
            client_order_id: order.client_order_id,
        })
    }

    async fn lookup_futures_order_until_resolved(
        &self,
        order: &FuturesOrderRequest,
    ) -> Option<OrderExecutionReport> {
        const LOOKUP_ATTEMPTS: usize = 3;
        for attempt in 0..LOOKUP_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            if let FuturesOrderLookup::Found(report) = self.query_futures_order(order).await {
                return Some(report);
            }
        }
        None
    }

    async fn query_futures_order(&self, order: &FuturesOrderRequest) -> FuturesOrderLookup {
        let parameters = format!(
            "symbol={}&origClientOrderId={}",
            order.symbol, order.client_order_id
        );
        let path = self.signed_futures_path("/fapi/v1/order", &parameters);
        let url = format!("{}{path}", self.base_url);
        let response = match self.client.get(&url).send().await {
            Ok(response) => response,
            Err(_) => return FuturesOrderLookup::Unavailable,
        };
        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => return FuturesOrderLookup::Unavailable,
        };
        let body = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(body) => body,
            Err(_) => return FuturesOrderLookup::Unavailable,
        };

        if !status.is_success() {
            if body.get("code").and_then(|value| value.as_i64()) == Some(-2013) {
                return FuturesOrderLookup::NotFound;
            }
            return FuturesOrderLookup::Unavailable;
        }

        match parse_futures_order_report(&body, order) {
            Ok(report) => FuturesOrderLookup::Found(report),
            Err(_) => FuturesOrderLookup::Unavailable,
        }
    }

    async fn fetch_futures_preflight_snapshot(
        &self,
        symbol: &str,
    ) -> Result<FuturesPreflightSnapshot, GatewayError> {
        if self.config.spot {
            return Err(GatewayError::FuturesPreflight(
                "USD-M preflight is unavailable in Spot mode".to_string(),
            ));
        }
        if !is_valid_asset(symbol) {
            return Err(GatewayError::FuturesPreflight(
                "symbol must contain only ASCII letters and digits".to_string(),
            ));
        }

        let multi_assets_path = self.signed_futures_path("/fapi/v1/multiAssetsMargin", "");
        let position_mode_path = self.signed_futures_path("/fapi/v1/positionSide/dual", "");
        let account_path = self.signed_futures_path("/fapi/v3/account", "");
        let positions_path =
            self.signed_futures_path("/fapi/v3/positionRisk", &format!("symbol={symbol}"));
        let orders_path =
            self.signed_futures_path("/fapi/v1/openOrders", &format!("symbol={symbol}"));
        let algo_orders_path =
            self.signed_futures_path("/fapi/v1/openAlgoOrders", &format!("symbol={symbol}"));
        let bracket_path =
            self.signed_futures_path("/fapi/v1/leverageBracket", &format!("symbol={symbol}"));
        let commission_path =
            self.signed_futures_path("/fapi/v1/commissionRate", &format!("symbol={symbol}"));
        let symbol_config_path =
            self.signed_futures_path("/fapi/v1/symbolConfig", &format!("symbol={symbol}"));
        let exchange_info_path = "/fapi/v1/exchangeInfo".to_string();
        let mark_price_path = format!("/fapi/v1/premiumIndex?symbol={symbol}");

        let (
            multi_assets,
            position_mode,
            account,
            positions,
            open_orders,
            algo_orders,
            exchange_info,
            mark_price,
            leverage_brackets,
            commission,
            symbol_config,
        ) = tokio::try_join!(
            self.get_binance_json(&multi_assets_path),
            self.get_binance_json(&position_mode_path),
            self.get_observed_binance_json(&account_path),
            self.get_observed_binance_json(&positions_path),
            self.get_observed_binance_json(&orders_path),
            self.get_observed_binance_json(&algo_orders_path),
            self.get_binance_json(&exchange_info_path),
            async {
                let (body, observed_at) = self.get_observed_binance_json(&mark_price_path).await?;
                Ok::<_, GatewayError>((body, observed_at, current_unix_time_ms()?))
            },
            self.get_binance_json(&bracket_path),
            self.get_binance_json(&commission_path),
            self.get_binance_json(&symbol_config_path),
        )?;

        if required_futures_bool(&multi_assets, "multiAssetsMargin", "account mode")? {
            return Err(GatewayError::FuturesPreflight(
                "multi-asset margin mode is unsupported; use single-asset USDT mode".to_string(),
            ));
        }
        if required_futures_bool(&position_mode, "dualSidePosition", "position mode")? {
            return Err(GatewayError::FuturesPreflight(
                "hedge mode is unsupported; use one-way position mode".to_string(),
            ));
        }

        let observed_at = [
            account.1,
            positions.1,
            open_orders.1,
            algo_orders.1,
            mark_price.1,
        ];
        parse_futures_preflight_snapshot(FuturesPreflightInput {
            symbol,
            observed_at,
            account: &account.0,
            positions: &positions.0,
            open_orders: &open_orders.0,
            algo_orders: &algo_orders.0,
            exchange_info: &exchange_info,
            mark: &mark_price.0,
            leverage_brackets: &leverage_brackets,
            commission: &commission,
            symbol_config: &symbol_config,
            mark_received_ms: mark_price.2,
            allow_foreign_collateral: self.config.allow_foreign_collateral,
        })
    }

    async fn validate_futures_preflight(
        &self,
        symbol: &str,
        intent: &OrderIntent,
        quantity: Decimal,
        snapshot: &FuturesPreflightSnapshot,
    ) -> Result<u32, GatewayError> {
        validate_futures_snapshot_age(&snapshot.observed_at, std::time::Instant::now())?;
        validate_futures_mark_timestamp(snapshot.mark_time_ms, current_unix_time_ms()?)?;
        validate_futures_order_rules(&snapshot.rules, intent.price, quantity)?;

        let order_notional = intent.price * quantity;
        let projected_notional =
            snapshot.position_notional + snapshot.open_order_notional + order_notional;
        if order_notional > self.config.risk_policy.max_order_notional {
            return Err(GatewayError::FuturesPreflight(format!(
                "order notional {order_notional} exceeds the configured cap {}",
                self.config.risk_policy.max_order_notional
            )));
        }
        if projected_notional > self.config.risk_policy.max_position_notional {
            return Err(GatewayError::FuturesPreflight(format!(
                "projected symbol exposure {projected_notional} exceeds the configured cap {}",
                self.config.risk_policy.max_position_notional
            )));
        }

        let bracket = bracket_for_notional(&snapshot.leverage_brackets, projected_notional)?;
        let maintenance_margin =
            projected_notional * bracket.maintenance_rate - bracket.maintenance_amount;
        if maintenance_margin < Decimal::ZERO {
            return Err(GatewayError::FuturesPreflight(
                "leverage bracket produced a negative maintenance margin".to_string(),
            ));
        }
        let commission = order_notional * snapshot.taker_commission_rate;
        let has_existing_exposure = snapshot.has_position || snapshot.has_open_orders;

        let leverage = if has_existing_exposure {
            snapshot.current_leverage
        } else {
            minimum_sufficient_leverage(
                projected_notional,
                maintenance_margin,
                commission,
                snapshot.available_balance,
                bracket.max_leverage,
            )?
        };
        if leverage == 0 || leverage > bracket.max_leverage {
            return Err(GatewayError::FuturesPreflight(
                "current leverage exceeds the maximum allowed by the active notional bracket"
                    .to_string(),
            ));
        }
        if has_existing_exposure && projected_notional > snapshot.maximum_notional {
            return Err(GatewayError::FuturesPreflight(format!(
                "projected symbol exposure {projected_notional} exceeds the configured leverage notional limit {}",
                snapshot.maximum_notional
            )));
        }

        let additional_initial_margin = order_notional / Decimal::from(leverage);
        if snapshot.available_balance < additional_initial_margin + commission {
            return Err(GatewayError::FuturesPreflight(format!(
                "available USDT balance {} cannot cover initial margin and fees {}",
                snapshot.available_balance,
                additional_initial_margin + commission
            )));
        }
        if snapshot.available_balance + snapshot.isolated_wallet < maintenance_margin + commission {
            return Err(GatewayError::FuturesPreflight(
                "available and isolated USDT margin cannot cover projected maintenance margin and fees"
                    .to_string(),
            ));
        }

        if !snapshot.margin_type.eq_ignore_ascii_case("isolated") {
            return Err(GatewayError::FuturesPreflight(
                "cross margin is unsupported; configure this symbol as isolated before trading"
                    .to_string(),
            ));
        }

        if !has_existing_exposure && leverage != snapshot.current_leverage {
            let response = self
                .post_signed_futures_json(
                    "/fapi/v1/leverage",
                    &format!("symbol={symbol}&leverage={leverage}"),
                )
                .await?;
            let applied_leverage =
                required_futures_u32(&response, "leverage", "leverage update response")?;
            let applied_symbol =
                required_futures_string(&response, "symbol", "leverage update response")?;
            let applied_maximum_notional =
                futures_decimal_field(&response, "maxNotionalValue", "leverage update response")?;
            if applied_leverage != leverage
                || applied_symbol != symbol
                || projected_notional > applied_maximum_notional
            {
                return Err(GatewayError::FuturesPreflight(format!(
                    "exchange leverage update did not support the requested {leverage}x exposure"
                )));
            }
        } else if projected_notional > snapshot.maximum_notional {
            return Err(GatewayError::FuturesPreflight(format!(
                "projected symbol exposure {projected_notional} exceeds the configured leverage notional limit {}",
                snapshot.maximum_notional
            )));
        }

        validate_futures_snapshot_age(&snapshot.observed_at, std::time::Instant::now())?;
        Ok(leverage)
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
            lot_rules: parse_spot_lot_rules(item),
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
        symbol_info: &SpotSymbolInfo,
    ) -> Result<(), GatewayError> {
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
                        symbol_info,
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

    /// Overlays the symbol's real exchange filters onto the configured risk policy.
    ///
    /// Futures use the live `LOT_SIZE`/`MIN_NOTIONAL` rules; Spot cannot borrow, so leverage is 1x.
    fn exchange_sized_policy(
        &self,
        futures: Option<&FuturesPreflightSnapshot>,
        spot: Option<&SpotSymbolInfo>,
    ) -> Result<RiskPolicy, GatewayError> {
        let base = &self.config.risk_policy;
        let policy = match (futures, spot) {
            (Some(snapshot), _) => base.clone().with_execution_rules(
                base.max_leverage,
                snapshot.rules.min_qty,
                snapshot.rules.step_size,
                snapshot.rules.min_notional,
            )?,
            (None, Some(info)) => {
                let (min_qty, step_size, min_notional) = info.lot_rules.unwrap_or((
                    base.min_order_quantity,
                    base.quantity_step_size,
                    base.min_order_notional,
                ));
                base.clone()
                    .with_execution_rules(Decimal::ONE, min_qty, step_size, min_notional)?
            }
            (None, None) => base.clone(),
        };
        Ok(policy)
    }

    /// Sizes with the shared session policy, then submits a signed live order to Binance.
    pub async fn place_order(
        &self,
        symbol: &str,
        intent: &OrderIntent,
        mark_to_market_equity: Decimal,
        session_peak_equity: Decimal,
        current_position_notional: Decimal,
    ) -> Result<OrderExecutionReport, GatewayError> {
        let submission_lease = if self.config.spot {
            None
        } else {
            Some(self.reserve_futures_submission()?)
        };
        let futures_snapshot = if self.config.spot {
            None
        } else {
            Some(self.fetch_futures_preflight_snapshot(symbol).await?)
        };
        let sizing_position_notional = futures_snapshot
            .as_ref()
            .map(|snapshot| snapshot.position_notional + snapshot.open_order_notional)
            .unwrap_or(current_position_notional);

        // 1. Mandatory shared sizing and pre-trade risk gatekeeping.
        let current_session_drawdown_pct =
            RiskPolicy::calculate_session_drawdown_pct(session_peak_equity, mark_to_market_equity)?;
        let spot_info = if self.config.spot {
            Some(self.fetch_spot_symbol_info(symbol).await?)
        } else {
            None
        };
        let sizing_policy =
            self.exchange_sized_policy(futures_snapshot.as_ref(), spot_info.as_ref())?;
        let quantity = sizing_policy.size_order(
            intent,
            intent.price,
            mark_to_market_equity,
            self.config.risk_per_trade_pct,
            sizing_position_notional,
            current_session_drawdown_pct,
        )?;

        if let Some(symbol_info) = spot_info.as_ref() {
            self.validate_spot_preflight(symbol, intent, quantity, symbol_info)
                .await?;
        } else if let Some(snapshot) = futures_snapshot.as_ref() {
            self.validate_futures_preflight(symbol, intent, quantity, snapshot)
                .await?;
        }

        if let (Some(snapshot), Some(lease)) =
            (futures_snapshot.as_ref(), submission_lease.as_ref())
        {
            return self
                .submit_futures_order(symbol, intent, quantity, snapshot, lease)
                .await;
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

        if let Some(snapshot) = futures_snapshot.as_ref() {
            validate_futures_snapshot_age(&snapshot.observed_at, std::time::Instant::now())?;
            validate_futures_mark_timestamp(snapshot.mark_time_ms, current_unix_time_ms()?)?;
        }
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
            sent_at_ms: 0,
        })
    }

    /// Rechecks the client order ID retained after an uncertain USD-M submission.
    ///
    /// A matching exchange record returns its report and releases the gateway's
    /// submission latch. A missing or invalid lookup keeps the latch closed.
    pub async fn reconcile_uncertain_futures_submission(
        &self,
    ) -> Result<OrderExecutionReport, GatewayError> {
        if self.config.spot {
            return Err(GatewayError::FuturesPreflight(
                "USD-M submission reconciliation is unavailable in Spot mode".to_string(),
            ));
        }

        let order = {
            let state = self.futures_submission_state.lock().map_err(|_| {
                GatewayError::FuturesPreflight(
                    "USD-M submission reservation state is unavailable".to_string(),
                )
            })?;
            match &*state {
                FuturesSubmissionState::Available => {
                    return Err(GatewayError::FuturesPreflight(
                        "there is no uncertain USD-M submission to reconcile".to_string(),
                    ));
                }
                FuturesSubmissionState::Reserved { client_order_id } => {
                    return Err(GatewayError::FuturesSubmissionInProgress {
                        client_order_id: client_order_id.clone(),
                    });
                }
                FuturesSubmissionState::Dispatching { order } => {
                    return Err(GatewayError::FuturesSubmissionInProgress {
                        client_order_id: order.client_order_id.clone(),
                    });
                }
                FuturesSubmissionState::Uncertain { order } => order.clone(),
            }
        };

        let Some(report) = self.lookup_futures_order_until_resolved(&order).await else {
            return Err(GatewayError::FuturesSubmissionUncertain {
                symbol: order.symbol,
                client_order_id: order.client_order_id,
            });
        };

        let mut state = self.futures_submission_state.lock().map_err(|_| {
            GatewayError::FuturesPreflight(
                "USD-M submission reservation state is unavailable".to_string(),
            )
        })?;
        if matches!(
            &*state,
            FuturesSubmissionState::Uncertain { order: current }
                if current.client_order_id == order.client_order_id
        ) {
            *state = FuturesSubmissionState::Available;
        }
        Ok(report)
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
            sent_at_ms: 0,
        })
    }

    /// Creates a new User Data Stream `listenKey` for streaming execution reports.
    pub async fn create_listen_key(&self) -> Result<String, GatewayError> {
        if self.config.spot {
            return Err(GatewayError::Config(
                "Spot user streams use the signed WebSocket API subscription".to_string(),
            ));
        }
        let path = "/fapi/v1/listenKey";
        let url = format!("{}{path}", self.base_url);

        let resp = tokio::time::timeout(
            USER_STREAM_HTTP_TIMEOUT,
            self.client
                .post(&url)
                .timeout(USER_STREAM_HTTP_TIMEOUT)
                .send(),
        )
        .await
        .map_err(|_| GatewayError::Config("USD-M listen-key creation timed out".to_string()))?
        .map_err(|_| GatewayError::Config("USD-M listen-key creation failed".to_string()))?;
        if !resp.status().is_success() {
            return Err(GatewayError::Config(
                "USD-M listen-key creation failed".to_string(),
            ));
        }
        let body: serde_json::Value = resp.json().await.map_err(|_| {
            GatewayError::Config("USD-M listen-key response was malformed".to_string())
        })?;

        let listen_key = body
            .get("listenKey")
            .and_then(|k| k.as_str())
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                GatewayError::Config("USD-M response omitted its listen key".to_string())
            })?
            .to_string();

        Ok(listen_key)
    }

    /// Pings the `listenKey` to prevent expiration (should be called every ~30 minutes).
    pub async fn keep_alive_listen_key(&self, listen_key: &str) -> Result<(), GatewayError> {
        if self.config.spot || listen_key.is_empty() || listen_key.chars().any(char::is_control) {
            return Err(GatewayError::Config(
                "USD-M listen-key renewal configuration is invalid".to_string(),
            ));
        }
        let path = "/fapi/v1/listenKey";
        let url = format!("{}{path}", self.base_url);

        let resp = tokio::time::timeout(
            USER_STREAM_HTTP_TIMEOUT,
            self.client
                .put(&url)
                .query(&[("listenKey", listen_key)])
                .timeout(USER_STREAM_HTTP_TIMEOUT)
                .send(),
        )
        .await
        .map_err(|_| GatewayError::Config("USD-M listen-key renewal timed out".to_string()))?
        .map_err(|_| GatewayError::Config("USD-M listen-key renewal failed".to_string()))?;
        if !resp.status().is_success() {
            return Err(GatewayError::Config(
                "USD-M listen-key renewal failed".to_string(),
            ));
        }
        let body: serde_json::Value = resp.json().await.map_err(|_| {
            GatewayError::Config("USD-M listen-key renewal response was malformed".to_string())
        })?;
        if !body.is_object() || body.get("code").is_some() || body.get("msg").is_some() {
            return Err(GatewayError::Config(
                "USD-M listen-key renewal response was invalid".to_string(),
            ));
        }
        Ok(())
    }

    /// Closes a USD-M listen key during private-stream shutdown.
    pub async fn close_listen_key(&self, listen_key: &str) -> Result<(), GatewayError> {
        if self.config.spot || listen_key.is_empty() || listen_key.chars().any(char::is_control) {
            return Err(GatewayError::Config(
                "USD-M listen-key cleanup configuration is invalid".to_string(),
            ));
        }
        let url = format!("{}/fapi/v1/listenKey", self.base_url);
        let response = tokio::time::timeout(
            USER_STREAM_HTTP_TIMEOUT,
            self.client
                .delete(&url)
                .query(&[("listenKey", listen_key)])
                .timeout(USER_STREAM_HTTP_TIMEOUT)
                .send(),
        )
        .await
        .map_err(|_| GatewayError::Config("USD-M listen-key cleanup timed out".to_string()))?
        .map_err(|_| GatewayError::Config("USD-M listen-key cleanup failed".to_string()))?;
        if !response.status().is_success() {
            return Err(GatewayError::Config(
                "USD-M listen-key cleanup failed".to_string(),
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

impl ListenKeyRenewer for BinanceGateway {
    fn renew<'a>(
        &'a self,
        listen_key: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + 'a>> {
        Box::pin(async move { self.keep_alive_listen_key(listen_key).await.map_err(|_| ()) })
    }

    fn close<'a>(
        &'a self,
        listen_key: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + 'a>> {
        Box::pin(async move { self.close_listen_key(listen_key).await.map_err(|_| ()) })
    }
}

fn new_futures_client_order_id() -> Result<String, GatewayError> {
    let timestamp_ms = current_unix_time_ms()?;
    let sequence = NEXT_FUTURES_CLIENT_ORDER_ID.fetch_add(1, Ordering::Relaxed);
    let client_order_id = format!("atsnt_{timestamp_ms:x}_{sequence:x}");
    if client_order_id.len() > 36 {
        return Err(GatewayError::Config(
            "generated USD-M client order ID exceeds Binance's 36-character limit".to_string(),
        ));
    }
    Ok(client_order_id)
}

fn parse_futures_order_report(
    body: &serde_json::Value,
    expected: &FuturesOrderRequest,
) -> Result<OrderExecutionReport, GatewayError> {
    let symbol = required_futures_string(body, "symbol", "USD-M order response")?;
    let client_order_id = required_futures_string(body, "clientOrderId", "USD-M order response")?;
    let side = required_futures_string(body, "side", "USD-M order response")?;
    let status = required_futures_string(body, "status", "USD-M order response")?;
    let order_id = body
        .get("orderId")
        .and_then(serde_json::Value::as_u64)
        .filter(|order_id| *order_id > 0)
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(
                "USD-M order response is missing a valid order ID".to_string(),
            )
        })?;
    let original_qty = futures_decimal_field(body, "origQty", "USD-M order response")?;
    let executed_qty = futures_decimal_field(body, "executedQty", "USD-M order response")?;
    // Only the acknowledgement of an order that has executed nothing may omit `cumQuote`; a
    // present but malformed value, or a missing one next to an execution, still fails.
    let cumulative_quote_qty = match body.get("cumQuote") {
        None | Some(serde_json::Value::Null) if executed_qty.is_zero() => Decimal::ZERO,
        _ => futures_decimal_field(body, "cumQuote", "USD-M order response")?,
    };
    let price = futures_decimal_field(body, "price", "USD-M order response")?;
    let timestamp_ms = ["updateTime", "time", "transactTime"]
        .into_iter()
        .find_map(|field| {
            body.get(field).and_then(|value| {
                value.as_i64().or_else(|| {
                    value
                        .as_u64()
                        .and_then(|timestamp| i64::try_from(timestamp).ok())
                })
            })
        })
        .filter(|timestamp| *timestamp > 0)
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(
                "USD-M order response is missing a valid exchange timestamp".to_string(),
            )
        })?;
    let expected_side = match expected.side {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    };

    if symbol != expected.symbol
        || client_order_id != expected.client_order_id
        || side != expected_side
        || original_qty != expected.quantity
        || price != expected.price
        || original_qty <= Decimal::ZERO
        || executed_qty < Decimal::ZERO
        || executed_qty > original_qty
        || cumulative_quote_qty < Decimal::ZERO
        || price < Decimal::ZERO
        || status.is_empty()
    {
        return Err(GatewayError::FuturesPreflight(
            "USD-M order response does not match the submitted order".to_string(),
        ));
    }

    Ok(OrderExecutionReport {
        symbol: symbol.to_string(),
        order_id,
        client_order_id: client_order_id.to_string(),
        side: expected.side,
        status: status.to_string(),
        price,
        original_qty,
        executed_qty,
        cumulative_quote_qty,
        timestamp_ms,
        sent_at_ms: 0,
    })
}

fn is_valid_asset(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// Reads LOT_SIZE and (MIN_)NOTIONAL from a Spot symbol; `None` unless every bound is valid.
fn parse_spot_lot_rules(item: &serde_json::Value) -> Option<(Decimal, Decimal, Decimal)> {
    let filters = item.get("filters")?.as_array()?;
    let find = |names: &[&str]| {
        filters.iter().find(|filter| {
            filter
                .get("filterType")
                .and_then(|value| value.as_str())
                .is_some_and(|kind| names.contains(&kind))
        })
    };
    let field = |filter: &serde_json::Value, key: &str| {
        filter
            .get(key)?
            .as_str()?
            .parse::<Decimal>()
            .ok()
            .filter(|value| *value > Decimal::ZERO)
    };
    let lot = find(&["LOT_SIZE"])?;
    let notional = find(&["NOTIONAL", "MIN_NOTIONAL"])?;
    Some((
        field(lot, "minQty")?,
        field(lot, "stepSize")?,
        field(notional, "minNotional")?,
    ))
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

fn futures_decimal_field(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<Decimal, GatewayError> {
    let value = body.get(key).ok_or_else(|| {
        GatewayError::FuturesPreflight(format!("{context} response is missing {key}"))
    })?;
    let raw = value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_number().map(ToString::to_string))
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!("{context} response has malformed {key}"))
        })?;
    Decimal::from_str(&raw)
        .or_else(|_| Decimal::from_scientific(&raw))
        .map_err(|error| {
            GatewayError::FuturesPreflight(format!("{context} response has invalid {key}: {error}"))
        })
}

fn required_futures_bool(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<bool, GatewayError> {
    body.get(key)
        .and_then(|value| value.as_bool())
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

fn required_futures_bool_or_string(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<bool, GatewayError> {
    body.get(key)
        .and_then(|value| {
            value
                .as_bool()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<bool>().ok()))
        })
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

fn required_futures_u32(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<u32, GatewayError> {
    body.get(key)
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        })
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

fn required_futures_timestamp(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<u64, GatewayError> {
    body.get(key)
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        })
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

fn required_futures_string<'a>(
    body: &'a serde_json::Value,
    key: &str,
    context: &str,
) -> Result<&'a str, GatewayError> {
    body.get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

fn required_futures_array<'a>(
    body: &'a serde_json::Value,
    key: &str,
    context: &str,
) -> Result<&'a Vec<serde_json::Value>, GatewayError> {
    body.get(key)
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

fn matching_futures_symbol<'a>(
    items: &'a [serde_json::Value],
    symbol: &str,
    context: &str,
) -> Result<&'a serde_json::Value, GatewayError> {
    let mut matches = items
        .iter()
        .filter(|item| item.get("symbol").and_then(|value| value.as_str()) == Some(symbol));
    let item = matches.next().ok_or_else(|| {
        GatewayError::FuturesPreflight(format!("{context} response is missing {symbol}"))
    })?;
    if matches.next().is_some() {
        return Err(GatewayError::FuturesPreflight(format!(
            "{context} response contains duplicate entries for {symbol}"
        )));
    }
    Ok(item)
}

fn parse_futures_symbol_rules(
    body: &serde_json::Value,
    symbol: &str,
) -> Result<FuturesSymbolRules, GatewayError> {
    let symbols = required_futures_array(body, "symbols", "exchange information")?;
    let item = matching_futures_symbol(symbols, symbol, "exchange information")?;
    if required_futures_string(item, "status", "exchange symbol")? != "TRADING"
        || required_futures_string(item, "contractType", "exchange symbol")? != "PERPETUAL"
        || required_futures_string(item, "quoteAsset", "exchange symbol")? != "USDT"
        || required_futures_string(item, "marginAsset", "exchange symbol")? != "USDT"
    {
        return Err(GatewayError::FuturesPreflight(format!(
            "{symbol} is not an active USDT-margined perpetual contract"
        )));
    }
    let filters = required_futures_array(item, "filters", "exchange symbol")?;
    let filter = |name: &str| -> Result<&serde_json::Value, GatewayError> {
        let mut matches = filters
            .iter()
            .filter(|entry| entry.get("filterType").and_then(|value| value.as_str()) == Some(name));
        let found = matches.next().ok_or_else(|| {
            GatewayError::FuturesPreflight(format!("exchange symbol is missing the {name} filter"))
        })?;
        if matches.next().is_some() {
            return Err(GatewayError::FuturesPreflight(format!(
                "exchange symbol contains duplicate {name} filters"
            )));
        }
        Ok(found)
    };

    let price_filter = filter("PRICE_FILTER")?;
    let quantity_filter = filter("LOT_SIZE")?;
    let notional_filter = filter("MIN_NOTIONAL")?;
    let rules = FuturesSymbolRules {
        min_price: futures_decimal_field(price_filter, "minPrice", "price filter")?,
        max_price: futures_decimal_field(price_filter, "maxPrice", "price filter")?,
        tick_size: futures_decimal_field(price_filter, "tickSize", "price filter")?,
        min_qty: futures_decimal_field(quantity_filter, "minQty", "quantity filter")?,
        max_qty: futures_decimal_field(quantity_filter, "maxQty", "quantity filter")?,
        step_size: futures_decimal_field(quantity_filter, "stepSize", "quantity filter")?,
        min_notional: futures_decimal_field(notional_filter, "notional", "notional filter")?,
    };
    if rules.min_price <= Decimal::ZERO
        || rules.max_price < rules.min_price
        || rules.tick_size <= Decimal::ZERO
        || rules.min_qty <= Decimal::ZERO
        || rules.max_qty < rules.min_qty
        || rules.step_size <= Decimal::ZERO
        || rules.min_notional <= Decimal::ZERO
    {
        return Err(GatewayError::FuturesPreflight(
            "exchange symbol filters contain invalid bounds".to_string(),
        ));
    }
    Ok(rules)
}

fn parse_futures_leverage_brackets(
    body: &serde_json::Value,
    symbol: &str,
) -> Result<Vec<FuturesLeverageBracket>, GatewayError> {
    let items = body.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "leverage bracket response is missing the result array".to_string(),
        )
    })?;
    let symbol_entry = matching_futures_symbol(items, symbol, "leverage bracket")?;
    let rows = required_futures_array(symbol_entry, "brackets", "leverage bracket")?;
    if rows.is_empty() {
        return Err(GatewayError::FuturesPreflight(
            "leverage bracket response contains no brackets".to_string(),
        ));
    }
    let mut brackets = Vec::with_capacity(rows.len());
    for row in rows {
        let floor = futures_decimal_field(row, "notionalFloor", "leverage bracket")?;
        let cap = futures_decimal_field(row, "notionalCap", "leverage bracket")?;
        let rate = futures_decimal_field(row, "maintMarginRatio", "leverage bracket")?;
        let amount = futures_decimal_field(row, "cum", "leverage bracket")?;
        let max_leverage = required_futures_u32(row, "initialLeverage", "leverage bracket")?;
        if floor < Decimal::ZERO
            || cap <= floor
            || !(Decimal::ZERO..Decimal::ONE).contains(&rate)
            || amount < Decimal::ZERO
            || max_leverage == 0
        {
            return Err(GatewayError::FuturesPreflight(
                "leverage bracket response contains invalid bounds".to_string(),
            ));
        }
        brackets.push(FuturesLeverageBracket {
            notional_floor: floor,
            notional_cap: cap,
            maintenance_rate: rate,
            maintenance_amount: amount,
            max_leverage,
        });
    }
    brackets.sort_by_key(|bracket| bracket.notional_floor);
    if brackets
        .windows(2)
        .any(|pair| pair[0].notional_floor == pair[1].notional_floor)
    {
        return Err(GatewayError::FuturesPreflight(
            "leverage bracket response contains duplicate notional tiers".to_string(),
        ));
    }
    Ok(brackets)
}

struct FuturesPreflightInput<'a> {
    symbol: &'a str,
    observed_at: [std::time::Instant; 5],
    account: &'a serde_json::Value,
    positions: &'a serde_json::Value,
    open_orders: &'a serde_json::Value,
    algo_orders: &'a serde_json::Value,
    exchange_info: &'a serde_json::Value,
    mark: &'a serde_json::Value,
    leverage_brackets: &'a serde_json::Value,
    commission: &'a serde_json::Value,
    symbol_config: &'a serde_json::Value,
    mark_received_ms: u64,
    allow_foreign_collateral: bool,
}

fn parse_futures_preflight_snapshot(
    input: FuturesPreflightInput<'_>,
) -> Result<FuturesPreflightSnapshot, GatewayError> {
    let FuturesPreflightInput {
        symbol,
        observed_at,
        account,
        positions,
        open_orders,
        algo_orders,
        exchange_info,
        mark,
        leverage_brackets,
        commission,
        symbol_config,
        mark_received_ms,
        allow_foreign_collateral,
    } = input;

    // `/fapi/v3/account` does not return `canTrade`: reject only when it is present and false.
    if account.get("canTrade").is_some()
        && !required_futures_bool(account, "canTrade", "futures account")?
    {
        return Err(GatewayError::FuturesPreflight(
            "futures account is not enabled for trading".to_string(),
        ));
    }
    let assets = required_futures_array(account, "assets", "futures account")?;
    let mut usdt_assets = assets
        .iter()
        .filter(|asset| asset.get("asset").and_then(|value| value.as_str()) == Some("USDT"));
    let usdt_asset = usdt_assets.next().ok_or_else(|| {
        GatewayError::FuturesPreflight("futures account is missing USDT collateral".to_string())
    })?;
    if usdt_assets.next().is_some() {
        return Err(GatewayError::FuturesPreflight(
            "futures account contains duplicate USDT collateral entries".to_string(),
        ));
    }
    let available_balance =
        futures_decimal_field(usdt_asset, "availableBalance", "USDT account balance")?;
    let account_available = futures_decimal_field(account, "availableBalance", "futures account")?;
    let usdt_wallet = futures_decimal_field(usdt_asset, "walletBalance", "USDT account balance")?;
    if available_balance != account_available
        || available_balance < Decimal::ZERO
        || usdt_wallet < Decimal::ZERO
    {
        return Err(GatewayError::FuturesPreflight(
            "account-level and USDT available balances are inconsistent".to_string(),
        ));
    }
    for asset in assets {
        if !allow_foreign_collateral
            && asset.get("asset").and_then(|value| value.as_str()) != Some("USDT")
            && futures_decimal_field(asset, "walletBalance", "futures account asset")?
                != Decimal::ZERO
        {
            return Err(GatewayError::FuturesPreflight(
                "non-USDT futures collateral is unsupported in the selected account profile"
                    .to_string(),
            ));
        }
    }

    let position_rows = positions.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight("position response is missing the result array".to_string())
    })?;
    let (position_amount, isolated_wallet, exchange_position_notional) = if position_rows.is_empty()
    {
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
    } else {
        let position = matching_futures_symbol(position_rows, symbol, "position")?;
        if required_futures_string(position, "positionSide", "position")? != "BOTH" {
            return Err(GatewayError::FuturesPreflight(
                "position response is not in one-way mode".to_string(),
            ));
        }
        let position_amount = futures_decimal_field(position, "positionAmt", "position")?;
        let isolated_wallet = futures_decimal_field(position, "isolatedWallet", "position")?;
        let exchange_notional = futures_decimal_field(position, "notional", "position")?.abs();
        if required_futures_string(position, "marginAsset", "position")? != "USDT"
            || isolated_wallet < Decimal::ZERO
        {
            return Err(GatewayError::FuturesPreflight(
                "position response contains unsupported margin asset or invalid isolated margin"
                    .to_string(),
            ));
        }
        (position_amount, isolated_wallet, exchange_notional)
    };
    let position_notional = (position_amount.abs()
        * futures_decimal_field(mark, "markPrice", "mark price")?)
    .max(exchange_position_notional);
    if position_amount == Decimal::ZERO && exchange_position_notional != Decimal::ZERO {
        return Err(GatewayError::FuturesPreflight(
            "position amount and reported notional are inconsistent".to_string(),
        ));
    }
    if position_amount == Decimal::ZERO && exchange_position_notional != Decimal::ZERO {
        return Err(GatewayError::FuturesPreflight(
            "position amount and reported notional are inconsistent".to_string(),
        ));
    }
    let has_position = position_amount != Decimal::ZERO;

    let algo_orders = algo_orders.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "algorithmic open-order response is missing the result array".to_string(),
        )
    })?;
    if !algo_orders.is_empty() {
        return Err(GatewayError::FuturesPreflight(
            "open conditional orders are unsupported in the selected Futures profile".to_string(),
        ));
    }

    let orders = open_orders.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "open-order response is missing the result array".to_string(),
        )
    })?;
    let mut open_order_notional = Decimal::ZERO;
    for order in orders {
        if required_futures_string(order, "symbol", "open order")? != symbol
            || required_futures_string(order, "positionSide", "open order")? != "BOTH"
            || required_futures_string(order, "type", "open order")? != "LIMIT"
            || !matches!(
                required_futures_string(order, "side", "open order")?,
                "BUY" | "SELL"
            )
        {
            return Err(GatewayError::FuturesPreflight(
                "open orders contain an unsupported symbol, position side, or order type"
                    .to_string(),
            ));
        }
        let price = futures_decimal_field(order, "price", "open order")?;
        let original = futures_decimal_field(order, "origQty", "open order")?;
        let executed = futures_decimal_field(order, "executedQty", "open order")?;
        let remaining = original - executed;
        if price <= Decimal::ZERO
            || original <= Decimal::ZERO
            || executed < Decimal::ZERO
            || remaining <= Decimal::ZERO
        {
            return Err(GatewayError::FuturesPreflight(
                "open order contains invalid price or remaining quantity".to_string(),
            ));
        }
        open_order_notional += price * remaining;
    }

    let mark_price = futures_decimal_field(mark, "markPrice", "mark price")?;
    let mark_time_ms = required_futures_timestamp(mark, "time", "mark price")?;
    if mark_price <= Decimal::ZERO {
        return Err(GatewayError::FuturesPreflight(
            "mark price must be positive".to_string(),
        ));
    }
    validate_futures_mark_timestamp(mark_time_ms, mark_received_ms)?;

    let brackets = parse_futures_leverage_brackets(leverage_brackets, symbol)?;
    let commission_symbol = required_futures_string(commission, "symbol", "commission")?;
    if commission_symbol != symbol {
        return Err(GatewayError::FuturesPreflight(
            "commission response symbol does not match the order symbol".to_string(),
        ));
    }
    let maker_rate = futures_decimal_field(commission, "makerCommissionRate", "commission")?;
    let taker_rate = futures_decimal_field(commission, "takerCommissionRate", "commission")?;
    if maker_rate < Decimal::ZERO
        || taker_rate < Decimal::ZERO
        || maker_rate >= Decimal::ONE
        || taker_rate >= Decimal::ONE
    {
        return Err(GatewayError::FuturesPreflight(
            "commission rates are outside the safe supported range".to_string(),
        ));
    }

    let symbol_config_rows = symbol_config.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "symbol configuration response is missing the result array".to_string(),
        )
    })?;
    let symbol_config = matching_futures_symbol(symbol_config_rows, symbol, "symbol config")?;
    let margin_type = required_futures_string(symbol_config, "marginType", "symbol config")?;
    let current_leverage = required_futures_u32(symbol_config, "leverage", "symbol config")?;
    let maximum_notional =
        futures_decimal_field(symbol_config, "maxNotionalValue", "symbol config")?;
    // Leverage 0 means "never set on this account" (testnet reports it for untouched symbols).
    // While flat the gateway chooses and sets the leverage of the order itself, so it is only
    // unusable when a position or orders already run on the exchange's current leverage.
    let leverage_unusable = current_leverage == 0 && (has_position || !orders.is_empty());
    if leverage_unusable || maximum_notional <= Decimal::ZERO {
        tracing::warn!(
            current_leverage,
            maximum_notional = %maximum_notional,
            has_position,
            has_open_orders = !orders.is_empty(),
            "symbol configuration refused: invalid leverage or notional bounds"
        );
        return Err(GatewayError::FuturesPreflight(
            "symbol configuration contains invalid leverage or notional bounds".to_string(),
        ));
    }
    if required_futures_bool_or_string(symbol_config, "isAutoAddMargin", "symbol config")? {
        return Err(GatewayError::FuturesPreflight(
            "automatic margin addition is unsupported in the selected isolated profile".to_string(),
        ));
    }

    Ok(FuturesPreflightSnapshot {
        observed_at,
        available_balance,
        position_notional,
        isolated_wallet,
        current_leverage,
        maximum_notional,
        margin_type: margin_type.to_string(),
        open_order_notional,
        has_position,
        has_open_orders: !orders.is_empty(),
        mark_time_ms,
        mark_received_ms,
        rules: parse_futures_symbol_rules(exchange_info, symbol)?,
        leverage_brackets: brackets,
        taker_commission_rate: maker_rate.max(taker_rate),
    })
}

fn validate_futures_order_rules(
    rules: &FuturesSymbolRules,
    price: Decimal,
    quantity: Decimal,
) -> Result<(), GatewayError> {
    let notional = price * quantity;
    // `price` is the reference of a market order, never sent to the exchange, so it only has to
    // lie inside the contract bounds; tick alignment applies to limit prices only.
    if price < rules.min_price || price > rules.max_price {
        return Err(GatewayError::FuturesPreflight(
            "order price violates the contract price filter".to_string(),
        ));
    }
    if quantity < rules.min_qty
        || quantity > rules.max_qty
        || quantity % rules.step_size != Decimal::ZERO
    {
        return Err(GatewayError::FuturesPreflight(
            "order quantity violates the contract lot-size filter".to_string(),
        ));
    }
    if notional < rules.min_notional {
        return Err(GatewayError::FuturesPreflight(
            "order notional is below the contract minimum".to_string(),
        ));
    }
    Ok(())
}

fn validate_futures_snapshot_age(
    observations: &[std::time::Instant],
    dispatch_at: std::time::Instant,
) -> Result<(), GatewayError> {
    if observations.iter().any(|observed_at| {
        dispatch_at < *observed_at
            || dispatch_at.duration_since(*observed_at) > FUTURES_MAX_DATA_AGE
    }) {
        return Err(GatewayError::FuturesPreflight(
            "account, position, open-order, or mark-price data is older than two seconds"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_futures_mark_timestamp(mark_time_ms: u64, now_ms: u64) -> Result<(), GatewayError> {
    if mark_time_ms > now_ms || now_ms - mark_time_ms > 2_000 {
        // Positive: how old the mark price is; negative: how far ahead of the local clock. The
        // refusal text stays as it always was; the numbers go to the log for diagnosis.
        let age_ms = i128::from(now_ms) - i128::from(mark_time_ms);
        tracing::warn!(
            mark_time_ms,
            received_ms = now_ms,
            age_ms = %age_ms,
            "mark price refused: stale or in the future (limit 2000 ms when received)"
        );
        return Err(GatewayError::FuturesPreflight(
            "mark-price exchange timestamp is stale or in the future".to_string(),
        ));
    }
    Ok(())
}

fn current_unix_time_ms() -> Result<u64, GatewayError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|_| {
            GatewayError::FuturesPreflight("system clock is before the Unix epoch".to_string())
        })
}

fn bracket_for_notional(
    brackets: &[FuturesLeverageBracket],
    notional: Decimal,
) -> Result<&FuturesLeverageBracket, GatewayError> {
    brackets
        .iter()
        .filter(|bracket| notional >= bracket.notional_floor && notional <= bracket.notional_cap)
        .max_by(|left, right| left.notional_floor.cmp(&right.notional_floor))
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(
                "projected symbol exposure is outside all exchange leverage brackets".to_string(),
            )
        })
}

fn minimum_sufficient_leverage(
    notional: Decimal,
    maintenance_margin: Decimal,
    commission: Decimal,
    available_balance: Decimal,
    maximum_leverage: u32,
) -> Result<u32, GatewayError> {
    if available_balance <= Decimal::ZERO || maximum_leverage == 0 {
        return Err(GatewayError::FuturesPreflight(
            "no positive USDT balance or supported leverage is available".to_string(),
        ));
    }
    domain::futures::minimum_leverage(
        notional,
        maintenance_margin,
        commission,
        available_balance,
        maximum_leverage,
    )
    .ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "available USDT balance cannot cover exposure at any supported leverage".to_string(),
        )
    })
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
mod tests;
