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

#[path = "binance_gateway/exits.rs"]
mod exits;
pub use exits::{ExitKind, PositionClosureReport, ProtectiveExitReport};
#[path = "binance_gateway/account_check.rs"]
mod account_check;
pub use account_check::{AccountFix, AccountReport, Finding, FindingStatus};
mod futures_preflight;
use futures_preflight::{
    current_unix_time_ms, validate_futures_mark_timestamp, validate_futures_snapshot_age,
    FuturesPreflightSnapshot,
};
mod json_fields;
use json_fields::{
    futures_decimal_field, is_valid_asset, matching_futures_symbol, parse_decimal_field,
    required_asset_field, required_futures_array, required_futures_bool,
    required_futures_bool_or_string, required_futures_string, required_futures_timestamp,
    required_futures_u32,
};
mod listen_key;
mod spot_preflight;
use spot_preflight::{required_spot_balance, SpotSymbolInfo};

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

    /// Base URL configured for this gateway instance.
    #[inline]
    pub fn base_url(&self) -> &str {
        &self.base_url
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

#[cfg(test)]
mod tests;
