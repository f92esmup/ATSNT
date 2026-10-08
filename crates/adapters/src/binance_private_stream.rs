use crate::binance_user_stream::ExecutionUpdate;
use crate::BinanceAuth;
use domain::Side;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const SPOT_USER_STREAM_PRODUCTION: &str = "wss://ws-api.binance.com:443/ws-api/v3";
const SPOT_USER_STREAM_TESTNET: &str = "wss://ws-api.testnet.binance.vision/ws-api/v3";
const FUTURES_USER_STREAM_PRODUCTION: &str = "wss://fstream.binance.com/ws/";
const FUTURES_USER_STREAM_TESTNET: &str = "wss://stream.binancefuture.com/ws/";
const USER_STREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const USER_STREAM_SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(10);
const USER_STREAM_RENEW_INTERVAL: Duration = Duration::from_secs(30 * 60);
const USER_STREAM_RENEW_TIMEOUT: Duration = Duration::from_secs(10);
const USER_STREAM_RENEW_ATTEMPTS: usize = 3;
const USER_STREAM_RENEW_RETRY_DELAY: Duration = Duration::from_secs(1);
const USER_STREAM_SEND_TIMEOUT: Duration = Duration::from_secs(10);
const USER_STREAM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(25);
const USER_STREAM_CHANNEL_CAPACITY: usize = 1_024;

static SPOT_REQUEST_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

type PrivateWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Narrow renewal port so the Futures stream can be tested without network access.
pub trait ListenKeyRenewer: Send + Sync + 'static {
    /// Extends a USD-M listen key's lifetime. Implementations must not log the key.
    fn renew<'a>(
        &'a self,
        listen_key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>>;

    /// Closes a USD-M listen key during stream teardown.
    fn close<'a>(
        &'a self,
        _listen_key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, Copy)]
struct FuturesRenewalPolicy {
    interval: Duration,
    request_timeout: Duration,
    attempts: usize,
    retry_delay: Duration,
}

impl Default for FuturesRenewalPolicy {
    fn default() -> Self {
        Self {
            interval: USER_STREAM_RENEW_INTERVAL,
            request_timeout: USER_STREAM_RENEW_TIMEOUT,
            attempts: USER_STREAM_RENEW_ATTEMPTS,
            retry_delay: USER_STREAM_RENEW_RETRY_DELAY,
        }
    }
}

/// Owned private-event reader. Call [`shutdown`](Self::shutdown) to close and join it.
pub struct BinancePrivateUserDataStream {
    receiver: mpsc::Receiver<Result<BinancePrivateEvent, BinancePrivateStreamError>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), BinancePrivateStreamError>>>,
}

impl BinancePrivateUserDataStream {
    /// Connects to the signed Spot WebSocket API user-data subscription.
    pub async fn connect_spot(
        auth: &BinanceAuth,
        testnet: bool,
    ) -> Result<Self, BinancePrivateStreamError> {
        let timestamp_ms = current_unix_time_ms()?;
        let sequence = SPOT_REQUEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let request_id = format!("atsnt-spot-{timestamp_ms}-{sequence}");
        let endpoint = if testnet {
            SPOT_USER_STREAM_TESTNET
        } else {
            SPOT_USER_STREAM_PRODUCTION
        };
        Self::connect_spot_with_endpoint(endpoint, auth, timestamp_ms, &request_id).await
    }

    /// Connects to the USD-M Futures listen-key stream and renews its key every 30 minutes.
    pub async fn connect_futures(
        listen_key: String,
        testnet: bool,
        renewer: Arc<dyn ListenKeyRenewer>,
    ) -> Result<Self, BinancePrivateStreamError> {
        if listen_key.is_empty() {
            return Err(BinancePrivateStreamError::InvalidConfiguration);
        }
        let base = if testnet {
            FUTURES_USER_STREAM_TESTNET
        } else {
            FUTURES_USER_STREAM_PRODUCTION
        };
        let endpoint = format!("{base}{listen_key}");
        Self::connect_futures_with_endpoint(
            &endpoint,
            listen_key,
            renewer,
            FuturesRenewalPolicy::default(),
        )
        .await
    }

    async fn connect_futures_with_endpoint(
        endpoint: &str,
        listen_key: String,
        renewer: Arc<dyn ListenKeyRenewer>,
        policy: FuturesRenewalPolicy,
    ) -> Result<Self, BinancePrivateStreamError> {
        if listen_key.is_empty() {
            return Err(BinancePrivateStreamError::InvalidConfiguration);
        }
        let websocket = connect_private_socket(endpoint).await?;
        Ok(Self::spawn_reader(
            websocket,
            Some((listen_key, renewer, policy)),
        ))
    }

    async fn connect_spot_with_endpoint(
        endpoint: &str,
        auth: &BinanceAuth,
        timestamp_ms: u64,
        request_id: &str,
    ) -> Result<Self, BinancePrivateStreamError> {
        let mut websocket = connect_private_socket(endpoint).await?;
        let request = build_spot_subscription_request(auth, timestamp_ms, request_id)?;
        tokio::time::timeout(
            USER_STREAM_SEND_TIMEOUT,
            websocket.send(Message::Text(request.into())),
        )
        .await
        .map_err(|_| BinancePrivateStreamError::TransportReconciliationRequired)?
        .map_err(|_| BinancePrivateStreamError::TransportReconciliationRequired)?;
        let response = tokio::time::timeout(USER_STREAM_SUBSCRIBE_TIMEOUT, websocket.next())
            .await
            .map_err(|_| BinancePrivateStreamError::SpotSubscriptionRejected)?
            .ok_or(BinancePrivateStreamError::SpotSubscriptionRejected)?
            .map_err(|_| BinancePrivateStreamError::TransportReconciliationRequired)?;
        let Message::Text(response) = response else {
            return Err(BinancePrivateStreamError::ProtocolReconciliationRequired);
        };
        let response: Value = serde_json::from_str(response.as_ref())
            .map_err(|_| BinancePrivateStreamError::ProtocolReconciliationRequired)?;
        if response.get("id").and_then(Value::as_str) != Some(request_id) {
            return Err(BinancePrivateStreamError::ProtocolReconciliationRequired);
        }
        let accepted = response.get("status").and_then(Value::as_u64) == Some(200)
            && response
                .get("result")
                .and_then(|result| result.get("subscriptionId"))
                .and_then(Value::as_u64)
                .is_some();
        if !accepted {
            return Err(BinancePrivateStreamError::SpotSubscriptionRejected);
        }
        Ok(Self::spawn_reader(websocket, None))
    }

    fn spawn_reader(
        websocket: PrivateWebSocket,
        renewal: Option<(String, Arc<dyn ListenKeyRenewer>, FuturesRenewalPolicy)>,
    ) -> Self {
        let (event_tx, receiver) = mpsc::channel(USER_STREAM_CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(run_private_reader(
            websocket,
            event_tx,
            shutdown_rx,
            renewal,
        ));
        Self {
            receiver,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        }
    }

    /// Returns the next normalized event or a reconciliation-required stream failure.
    pub async fn next_event(
        &mut self,
    ) -> Option<Result<BinancePrivateEvent, BinancePrivateStreamError>> {
        self.receiver.recv().await
    }

    /// Requests a WebSocket close and joins the reader task; this does not reconcile account state.
    pub async fn shutdown(&mut self) -> Result<(), BinancePrivateStreamError> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(task) = self.task.take() {
            let mut task = task;
            match tokio::time::timeout(USER_STREAM_SHUTDOWN_TIMEOUT, &mut task).await {
                Ok(Ok(result)) => result?,
                Ok(Err(_)) => return Err(BinancePrivateStreamError::TaskJoinFailed),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    return Err(BinancePrivateStreamError::TaskJoinFailed);
                }
            }
        }
        Ok(())
    }
}

impl Drop for BinancePrivateUserDataStream {
    fn drop(&mut self) {
        drop(self.shutdown_tx.take());
        if let Some(task) = self.task.take() {
            // Drop cannot await graceful socket/key cleanup, so abort rather than detach the reader.
            task.abort();
        }
    }
}

async fn connect_private_socket(
    endpoint: &str,
) -> Result<PrivateWebSocket, BinancePrivateStreamError> {
    tokio::time::timeout(USER_STREAM_CONNECT_TIMEOUT, connect_async(endpoint))
        .await
        .map_err(|_| BinancePrivateStreamError::TransportReconciliationRequired)?
        .map(|(socket, _)| socket)
        .map_err(|_| BinancePrivateStreamError::TransportReconciliationRequired)
}

async fn run_private_reader(
    mut websocket: PrivateWebSocket,
    event_tx: mpsc::Sender<Result<BinancePrivateEvent, BinancePrivateStreamError>>,
    mut shutdown_rx: oneshot::Receiver<()>,
    renewal: Option<(String, Arc<dyn ListenKeyRenewer>, FuturesRenewalPolicy)>,
) -> Result<(), BinancePrivateStreamError> {
    let mut renewal_interval = renewal.as_ref().map(|(_, _, policy)| {
        tokio::time::interval_at(
            tokio::time::Instant::now() + policy.interval,
            policy.interval,
        )
    });
    if let Some(interval) = renewal_interval.as_mut() {
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    }

    let mut last_event_time_ms = None;
    let mut shutdown_requested = false;
    'reader: loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => {
                shutdown_requested = true;
                break 'reader;
            }
            _ = renewal_tick(&mut renewal_interval) => {
                let Some((listen_key, renewer, policy)) = renewal.as_ref() else {
                    continue;
                };
                let renewed = tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => None,
                    result = renew_listen_key_with_policy(renewer.as_ref(), listen_key, *policy) => Some(result.is_ok()),
                };
                match renewed {
                    None => {
                        shutdown_requested = true;
                        break 'reader;
                    }
                    Some(true) => {}
                    Some(false) => {
                        let error = BinancePrivateStreamError::ListenKeyRenewalFailed;
                        if !send_stream_result(&event_tx, &mut shutdown_rx, Err(error)).await {
                            shutdown_requested = true;
                        }
                        break 'reader;
                    }
                }
            }
            message = websocket.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let parsed = parse_private_user_event(text.as_ref()).and_then(|event| {
                            let event_time_ms = private_event_time_ms(&event);
                            if let Some(previous_time_ms) = last_event_time_ms {
                                if event_time_ms < previous_time_ms {
                                    return Err(BinancePrivateStreamError::EventReconciliationRequired);
                                }
                            }
                            last_event_time_ms = Some(event_time_ms);
                            Ok(event)
                        });
                        let invalid = parsed.is_err();
                        let expired = matches!(parsed, Ok(BinancePrivateEvent::FuturesListenKeyExpired { .. }));
                        if !send_stream_result(&event_tx, &mut shutdown_rx, parsed).await {
                            shutdown_requested = true;
                            break 'reader;
                        }
                        if expired {
                            if !send_stream_result(
                                &event_tx,
                                &mut shutdown_rx,
                                Err(BinancePrivateStreamError::TransportReconciliationRequired),
                            ).await {
                                shutdown_requested = true;
                            }
                            break 'reader;
                        }
                        if invalid {
                            break 'reader;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let pong = tokio::select! {
                            biased;
                            _ = &mut shutdown_rx => {
                                shutdown_requested = true;
                                break 'reader;
                            }
                            result = tokio::time::timeout(
                                USER_STREAM_SEND_TIMEOUT,
                                websocket.send(Message::Pong(payload)),
                            ) => result,
                        };
                        if !matches!(pong, Ok(Ok(()))) {
                            if !send_stream_result(
                                &event_tx,
                                &mut shutdown_rx,
                                Err(BinancePrivateStreamError::TransportReconciliationRequired),
                            ).await {
                                shutdown_requested = true;
                            }
                            break 'reader;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        if !send_stream_result(
                            &event_tx,
                            &mut shutdown_rx,
                            Err(BinancePrivateStreamError::TransportReconciliationRequired),
                        ).await {
                            shutdown_requested = true;
                        }
                        break 'reader;
                    }
                    Some(Ok(Message::Binary(_))) | Some(Ok(Message::Frame(_))) => {
                        if !send_stream_result(
                            &event_tx,
                            &mut shutdown_rx,
                            Err(BinancePrivateStreamError::ProtocolReconciliationRequired),
                        ).await {
                            shutdown_requested = true;
                        }
                        break 'reader;
                    }
                }
            }
        }
    }

    let _ = tokio::time::timeout(
        USER_STREAM_SEND_TIMEOUT,
        websocket.send(Message::Close(None)),
    )
    .await;
    if let Some((listen_key, renewer, _)) = renewal.as_ref() {
        let closed =
            tokio::time::timeout(USER_STREAM_RENEW_TIMEOUT, renewer.close(listen_key)).await;
        if !matches!(closed, Ok(Ok(()))) {
            let error = BinancePrivateStreamError::ListenKeyCloseFailed;
            if !shutdown_requested {
                let _ = send_stream_result(&event_tx, &mut shutdown_rx, Err(error.clone())).await;
            }
            return Err(error);
        }
    }
    Ok(())
}

fn private_event_time_ms(event: &BinancePrivateEvent) -> u64 {
    match event {
        BinancePrivateEvent::SpotAccountPosition(update) => update.event_time_ms,
        BinancePrivateEvent::SpotBalanceDelta(update) => update.event_time_ms,
        BinancePrivateEvent::SpotExecution { event_time_ms, .. } => *event_time_ms,
        BinancePrivateEvent::FuturesAccountUpdate(update) => update.event_time_ms,
        BinancePrivateEvent::FuturesOrderTradeUpdate(update) => update.event_time_ms,
        BinancePrivateEvent::FuturesListenKeyExpired { event_time_ms } => *event_time_ms,
    }
}

async fn renewal_tick(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

async fn send_stream_result(
    sender: &mpsc::Sender<Result<BinancePrivateEvent, BinancePrivateStreamError>>,
    shutdown_rx: &mut oneshot::Receiver<()>,
    result: Result<BinancePrivateEvent, BinancePrivateStreamError>,
) -> bool {
    tokio::select! {
        biased;
        _ = shutdown_rx => false,
        sent = sender.send(result) => sent.is_ok(),
    }
}

async fn renew_listen_key_with_policy(
    renewer: &dyn ListenKeyRenewer,
    listen_key: &str,
    policy: FuturesRenewalPolicy,
) -> Result<(), BinancePrivateStreamError> {
    if policy.attempts == 0 || policy.request_timeout.is_zero() {
        return Err(BinancePrivateStreamError::ListenKeyRenewalFailed);
    }
    for attempt in 0..policy.attempts {
        let renewal = tokio::time::timeout(policy.request_timeout, renewer.renew(listen_key)).await;
        if matches!(renewal, Ok(Ok(()))) {
            return Ok(());
        }
        if attempt + 1 < policy.attempts {
            tokio::time::sleep(policy.retry_delay).await;
        }
    }
    Err(BinancePrivateStreamError::ListenKeyRenewalFailed)
}

fn current_unix_time_ms() -> Result<u64, BinancePrivateStreamError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|_| BinancePrivateStreamError::InvalidConfiguration)
}

/// Typed private-account event understood by the supported Spot and USD-M profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinancePrivateEvent {
    /// Spot account position events contain only balances possibly changed by this event.
    SpotAccountPosition(SpotAccountPosition),
    /// Spot balance transfer event; `delta` is a change, not a balance snapshot.
    SpotBalanceDelta(SpotBalanceDelta),
    /// Spot order execution report normalized by the existing adapter model.
    SpotExecution {
        event_time_ms: u64,
        update: ExecutionUpdate,
    },
    /// USD-M account update, restricted to USDT collateral and isolated one-way positions.
    FuturesAccountUpdate(FuturesAccountUpdate),
    /// USD-M order trade event with normalized execution quantities and prices.
    FuturesOrderTradeUpdate(FuturesOrderTradeUpdate),
    /// The listen key expired; the caller must reconcile account state.
    FuturesListenKeyExpired { event_time_ms: u64 },
}

/// Spot balances that may have changed; this is deliberately not a full account snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotAccountPosition {
    pub event_time_ms: u64,
    /// Binance `u`: timestamp of the last account update, in milliseconds.
    pub last_account_update_ms: u64,
    pub balances: Vec<SpotBalance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotBalance {
    pub asset: String,
    pub free: Decimal,
    pub locked: Decimal,
}

/// Spot `balanceUpdate`, where `delta` is the signed transfer amount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotBalanceDelta {
    pub event_time_ms: u64,
    pub transaction_time_ms: u64,
    pub asset: String,
    pub delta: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuturesAccountUpdate {
    pub event_time_ms: u64,
    pub transaction_time_ms: u64,
    pub reason: String,
    pub balances: Vec<FuturesBalance>,
    pub positions: Vec<FuturesPosition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuturesBalance {
    pub asset: String,
    pub wallet_balance: Decimal,
    pub cross_wallet_balance: Decimal,
    pub balance_change: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuturesPosition {
    pub symbol: String,
    pub position_amount: Decimal,
    pub entry_price: Decimal,
    pub break_even_price: Decimal,
    pub accumulated_realized: Decimal,
    pub unrealized_profit: Decimal,
    pub margin_type: String,
    pub isolated_wallet: Decimal,
    pub position_side: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuturesOrderTradeUpdate {
    pub event_time_ms: u64,
    pub transaction_time_ms: u64,
    pub symbol: String,
    pub client_order_id: String,
    pub side: Side,
    pub position_side: String,
    pub status: String,
    pub order_id: u64,
    pub last_filled_quantity: Decimal,
    pub last_filled_price: Decimal,
    pub cumulative_filled_quantity: Decimal,
    pub trade_id: Option<i64>,
}

/// Private stream errors intentionally omit endpoint, key, credential, and payload values.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BinancePrivateStreamError {
    #[error("private stream configuration is invalid")]
    InvalidConfiguration,
    #[error("private stream transport failed; account reconciliation is required")]
    TransportReconciliationRequired,
    #[error("private stream protocol failed; account reconciliation is required")]
    ProtocolReconciliationRequired,
    #[error(
        "private stream event is malformed or unsupported; account reconciliation is required"
    )]
    EventReconciliationRequired,
    #[error("Spot user-data subscription was rejected")]
    SpotSubscriptionRejected,
    #[error("USD-M listen-key renewal failed after its bounded retry budget")]
    ListenKeyRenewalFailed,
    #[error("USD-M listen-key cleanup failed during stream shutdown")]
    ListenKeyCloseFailed,
    #[error("private stream task failed to join")]
    TaskJoinFailed,
}

fn build_spot_subscription_request(
    auth: &BinanceAuth,
    timestamp_ms: u64,
    request_id: &str,
) -> Result<String, BinancePrivateStreamError> {
    if request_id.is_empty() {
        return Err(BinancePrivateStreamError::InvalidConfiguration);
    }
    let api_key = auth.api_key();
    let payload = format!("apiKey={api_key}&recvWindow=5000&timestamp={timestamp_ms}");
    let signature = auth.sign(&payload);
    serde_json::to_string(&json!({
        "id": request_id,
        "method": "userDataStream.subscribe.signature",
        "params": {
            "apiKey": api_key,
            "recvWindow": 5000,
            "timestamp": timestamp_ms,
            "signature": signature,
        }
    }))
    .map_err(|_| BinancePrivateStreamError::InvalidConfiguration)
}

/// Strictly parses supported private events without inventing missing values.
pub fn parse_private_user_event(
    json_str: &str,
) -> Result<BinancePrivateEvent, BinancePrivateStreamError> {
    let root: Value = serde_json::from_str(json_str)
        .map_err(|_| BinancePrivateStreamError::EventReconciliationRequired)?;
    let value = match (root.get("subscriptionId"), root.get("event")) {
        (None, None) => &root,
        (Some(subscription_id), Some(event))
            if subscription_id.as_u64().is_some() && event.is_object() =>
        {
            event
        }
        _ => return Err(BinancePrivateStreamError::EventReconciliationRequired),
    };
    let event_type = required_string(value, "e")?;

    match event_type {
        "outboundAccountPosition" => {
            let rows = value
                .get("B")
                .and_then(Value::as_array)
                .ok_or(BinancePrivateStreamError::EventReconciliationRequired)?;
            let balances = rows
                .iter()
                .map(|row| {
                    Ok(SpotBalance {
                        asset: required_string(row, "a")?.to_owned(),
                        free: required_decimal(row, "f")?,
                        locked: required_decimal(row, "l")?,
                    })
                })
                .collect::<Result<Vec<_>, BinancePrivateStreamError>>()?;
            Ok(BinancePrivateEvent::SpotAccountPosition(
                SpotAccountPosition {
                    event_time_ms: required_u64(value, "E")?,
                    last_account_update_ms: required_u64(value, "u")?,
                    balances,
                },
            ))
        }
        "balanceUpdate" => Ok(BinancePrivateEvent::SpotBalanceDelta(SpotBalanceDelta {
            event_time_ms: required_u64(value, "E")?,
            transaction_time_ms: required_u64(value, "T")?,
            asset: required_string(value, "a")?.to_owned(),
            delta: required_decimal(value, "d")?,
        })),
        "executionReport" => Ok(BinancePrivateEvent::SpotExecution {
            event_time_ms: required_u64(value, "E")?,
            update: parse_spot_execution_report(value)?,
        }),
        "ACCOUNT_UPDATE" => {
            parse_futures_account_update(value).map(BinancePrivateEvent::FuturesAccountUpdate)
        }
        "ORDER_TRADE_UPDATE" => parse_futures_order_trade_update(value)
            .map(BinancePrivateEvent::FuturesOrderTradeUpdate),
        "listenKeyExpired" => {
            // Validate presence, but never retain or expose the secret listen key.
            if required_string(value, "listenKey")?.is_empty() {
                return Err(BinancePrivateStreamError::EventReconciliationRequired);
            }
            Ok(BinancePrivateEvent::FuturesListenKeyExpired {
                event_time_ms: required_u64(value, "E")?,
            })
        }
        _ => Err(BinancePrivateStreamError::EventReconciliationRequired),
    }
}

fn parse_spot_execution_report(
    value: &Value,
) -> Result<ExecutionUpdate, BinancePrivateStreamError> {
    let side = match required_string(value, "S")? {
        "BUY" => Side::Buy,
        "SELL" => Side::Sell,
        _ => return Err(BinancePrivateStreamError::EventReconciliationRequired),
    };
    let commission_asset = match value.get("N") {
        Some(Value::String(asset)) => Some(asset.clone()),
        Some(Value::Null) => None,
        _ => return Err(BinancePrivateStreamError::EventReconciliationRequired),
    };
    let symbol = required_string(value, "s")?;
    let client_order_id = required_string(value, "c")?;
    let status = required_string(value, "X")?;
    let timestamp_ms = i64::try_from(required_u64(value, "T")?)
        .map_err(|_| BinancePrivateStreamError::EventReconciliationRequired)?;
    if symbol.is_empty() || client_order_id.is_empty() || status.is_empty() {
        return Err(BinancePrivateStreamError::EventReconciliationRequired);
    }

    Ok(ExecutionUpdate {
        symbol: symbol.to_owned(),
        order_id: required_u64(value, "i")?,
        client_order_id: client_order_id.to_owned(),
        side,
        status: status.to_owned(),
        last_filled_price: required_decimal(value, "L")?,
        last_filled_qty: required_decimal(value, "l")?,
        cumulative_filled_qty: required_decimal(value, "z")?,
        commission_amount: required_decimal(value, "n")?,
        commission_asset,
        timestamp_ms,
    })
}

fn parse_futures_account_update(
    value: &Value,
) -> Result<FuturesAccountUpdate, BinancePrivateStreamError> {
    let account = value
        .get("a")
        .ok_or(BinancePrivateStreamError::EventReconciliationRequired)?;
    let balances = account
        .get("B")
        .and_then(Value::as_array)
        .ok_or(BinancePrivateStreamError::EventReconciliationRequired)?
        .iter()
        .map(|row| {
            let balance = FuturesBalance {
                asset: required_string(row, "a")?.to_owned(),
                wallet_balance: required_decimal(row, "wb")?,
                cross_wallet_balance: required_decimal(row, "cw")?,
                balance_change: required_decimal(row, "bc")?,
            };
            if balance.asset != "USDT" && balance.wallet_balance != Decimal::ZERO {
                return Err(BinancePrivateStreamError::EventReconciliationRequired);
            }
            Ok(balance)
        })
        .collect::<Result<Vec<_>, BinancePrivateStreamError>>()?;
    let positions = account
        .get("P")
        .and_then(Value::as_array)
        .ok_or(BinancePrivateStreamError::EventReconciliationRequired)?
        .iter()
        .map(|row| {
            let position = FuturesPosition {
                symbol: required_string(row, "s")?.to_owned(),
                position_amount: required_decimal(row, "pa")?,
                entry_price: required_decimal(row, "ep")?,
                break_even_price: required_decimal(row, "bep")?,
                accumulated_realized: required_decimal(row, "cr")?,
                unrealized_profit: required_decimal(row, "up")?,
                margin_type: required_string(row, "mt")?.to_owned(),
                isolated_wallet: required_decimal(row, "iw")?,
                position_side: required_string(row, "ps")?.to_owned(),
            };
            if position.margin_type != "isolated" || position.position_side != "BOTH" {
                return Err(BinancePrivateStreamError::EventReconciliationRequired);
            }
            Ok(position)
        })
        .collect::<Result<Vec<_>, BinancePrivateStreamError>>()?;

    Ok(FuturesAccountUpdate {
        event_time_ms: required_u64(value, "E")?,
        transaction_time_ms: required_u64(value, "T")?,
        reason: required_string(account, "m")?.to_owned(),
        balances,
        positions,
    })
}

fn parse_futures_order_trade_update(
    value: &Value,
) -> Result<FuturesOrderTradeUpdate, BinancePrivateStreamError> {
    let order = value
        .get("o")
        .ok_or(BinancePrivateStreamError::EventReconciliationRequired)?;
    let trade_id = match order.get("t") {
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or(BinancePrivateStreamError::EventReconciliationRequired)?,
        _ => return Err(BinancePrivateStreamError::EventReconciliationRequired),
    };
    let side = match required_string(order, "S")? {
        "BUY" => Side::Buy,
        "SELL" => Side::Sell,
        _ => return Err(BinancePrivateStreamError::EventReconciliationRequired),
    };
    let position_side = required_string(order, "ps")?;
    if position_side != "BOTH" {
        return Err(BinancePrivateStreamError::EventReconciliationRequired);
    }
    Ok(FuturesOrderTradeUpdate {
        event_time_ms: required_u64(value, "E")?,
        transaction_time_ms: required_u64(value, "T")?,
        symbol: required_string(order, "s")?.to_owned(),
        client_order_id: required_string(order, "c")?.to_owned(),
        side,
        position_side: position_side.to_owned(),
        status: required_string(order, "X")?.to_owned(),
        order_id: required_u64(order, "i")?,
        last_filled_quantity: required_decimal(order, "l")?,
        last_filled_price: required_decimal(order, "L")?,
        cumulative_filled_quantity: required_decimal(order, "z")?,
        trade_id: (trade_id >= 0).then_some(trade_id),
    })
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, BinancePrivateStreamError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(BinancePrivateStreamError::EventReconciliationRequired)
}

fn required_u64(value: &Value, key: &str) -> Result<u64, BinancePrivateStreamError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(BinancePrivateStreamError::EventReconciliationRequired)
}

fn required_decimal(value: &Value, key: &str) -> Result<Decimal, BinancePrivateStreamError> {
    let raw = required_string(value, key)?;
    Decimal::from_str(raw).map_err(|_| BinancePrivateStreamError::EventReconciliationRequired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BinanceAuth;
    use futures_util::{SinkExt, StreamExt};
    use rust_decimal_macros::dec;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    #[test]
    fn spot_subscription_signs_alphabetically_sorted_params() {
        let auth = BinanceAuth::new("test-api-key", "test-secret");
        let request = build_spot_subscription_request(&auth, 1_700_000_000_000, "request-1")
            .expect("request should build");
        let request: serde_json::Value =
            serde_json::from_str(&request).expect("request should be JSON");

        assert_eq!(request["method"], "userDataStream.subscribe.signature");
        assert_eq!(request["id"], "request-1");
        assert_eq!(request["params"]["apiKey"], "test-api-key");
        assert_eq!(request["params"]["timestamp"], 1_700_000_000_000u64);
        assert_eq!(request["params"]["recvWindow"], 5_000);
        assert_eq!(
            request["params"]["signature"],
            auth.sign("apiKey=test-api-key&recvWindow=5000&timestamp=1700000000000")
        );
    }

    #[test]
    fn spot_account_position_keeps_partial_snapshot_semantics() {
        let event = parse_private_user_event(include_str!(
            "../tests/fixtures/binance_private_events/spot_outbound_account_position.json"
        ))
        .expect("official Spot account-position shape should parse");

        match event {
            BinancePrivateEvent::SpotAccountPosition(update) => {
                assert_eq!(update.event_time_ms, 1_700_000_000_001);
                assert_eq!(update.last_account_update_ms, 1_700_000_000_000);
                assert_eq!(update.balances.len(), 1);
                assert_eq!(update.balances[0].asset, "USDT");
                assert_eq!(update.balances[0].free, dec!(12.50));
                assert_eq!(update.balances[0].locked, dec!(1.25));
            }
            other => panic!("expected Spot account position, got {other:?}"),
        }
    }

    #[test]
    fn spot_balance_update_keeps_delta_semantics() {
        let event = parse_private_user_event(include_str!(
            "../tests/fixtures/binance_private_events/spot_balance_update.json"
        ))
        .expect("official Spot balance-update shape should parse");

        match event {
            BinancePrivateEvent::SpotBalanceDelta(update) => {
                assert_eq!(update.asset, "USDT");
                assert_eq!(update.delta, dec!(-2.75));
                assert_eq!(update.transaction_time_ms, 1_700_000_000_000);
            }
            other => panic!("expected Spot balance delta, got {other:?}"),
        }
    }

    #[test]
    fn futures_account_update_preserves_supported_usdt_one_way_isolated_profile() {
        let event = parse_private_user_event(include_str!(
            "../tests/fixtures/binance_private_events/usdm_account_update.json"
        ))
        .expect("supported USD-M account-update shape should parse");

        match event {
            BinancePrivateEvent::FuturesAccountUpdate(update) => {
                assert_eq!(update.reason, "ORDER");
                assert_eq!(update.balances[0].asset, "USDT");
                assert_eq!(update.balances[0].wallet_balance, dec!(50.0));
                assert_eq!(update.positions[0].symbol, "BTCUSDT");
                assert_eq!(update.positions[0].position_amount, dec!(0.001));
                assert_eq!(update.positions[0].margin_type, "isolated");
                assert_eq!(update.positions[0].position_side, "BOTH");
            }
            other => panic!("expected Futures account update, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_futures_margin_profile_is_rejected() {
        let event = r#"{"e":"ACCOUNT_UPDATE","E":1700000000003,"T":1700000000000,"a":{"m":"ORDER","B":[{"a":"USDT","wb":"50.0","cw":"48.0","bc":"-2.0"}],"P":[{"s":"BTCUSDT","pa":"0.001","ep":"42000.0","bep":"42001.0","cr":"0.0","up":"1.0","mt":"cross","iw":"0.0","ps":"BOTH"}]}}"#;
        assert!(parse_private_user_event(event).is_err());
    }

    #[test]
    fn futures_order_trade_update_maps_to_normalized_execution_fields() {
        let event = parse_private_user_event(include_str!(
            "../tests/fixtures/binance_private_events/usdm_order_trade_update.json"
        ))
        .expect("official USD-M order update shape should parse");

        match event {
            BinancePrivateEvent::FuturesOrderTradeUpdate(update) => {
                assert_eq!(update.symbol, "BTCUSDT");
                assert_eq!(update.client_order_id, "client-1");
                assert_eq!(update.side, Side::Buy);
                assert_eq!(update.position_side, "BOTH");
                assert_eq!(update.order_id, 123);
                assert_eq!(update.status, "PARTIALLY_FILLED");
                assert_eq!(update.last_filled_quantity, dec!(0.002));
                assert_eq!(update.last_filled_price, dec!(42000.5));
                assert_eq!(update.cumulative_filled_quantity, dec!(0.002));
                assert_eq!(update.trade_id, Some(456));
            }
            other => panic!("expected Futures order update, got {other:?}"),
        }
    }

    #[test]
    fn futures_order_trade_update_rejects_unsupported_hedge_position_side() {
        let event = r#"{"e":"ORDER_TRADE_UPDATE","E":1700000000004,"T":1700000000003,"o":{"s":"BTCUSDT","c":"client-1","S":"BUY","X":"PARTIALLY_FILLED","i":123,"l":"0.002","L":"42000.5","z":"0.002","t":456,"ps":"LONG"}}"#;
        assert!(parse_private_user_event(event).is_err());
    }

    #[test]
    fn futures_listen_key_expiration_is_explicit_without_returning_the_key() {
        let event = parse_private_user_event(include_str!(
            "../tests/fixtures/binance_private_events/usdm_listen_key_expired.json"
        ))
        .expect("listen-key expiration should parse");

        assert_eq!(
            event,
            BinancePrivateEvent::FuturesListenKeyExpired {
                event_time_ms: 1_700_000_000_005
            }
        );
        assert!(!format!("{event:?}").contains("synthetic-fixture-key"));
    }

    #[test]
    fn unsupported_or_malformed_private_events_are_errors() {
        assert!(parse_private_user_event(r#"{"e":"unknownEvent"}"#).is_err());
        assert!(parse_private_user_event(
            r#"{"e":"balanceUpdate","E":1700000000002,"a":"USDT","d":"not-a-decimal","T":1700000000000}"#
        )
        .is_err());
    }

    #[test]
    fn spot_execution_report_rejects_fields_that_the_legacy_parser_defaults() {
        let event = r#"{"e":"executionReport","E":1700000000006,"T":1700000000005,"s":"BTCUSDT","i":123,"c":"client-1","S":"BUY","X":"PARTIALLY_FILLED","l":"0.01","L":"42000.0","z":"0.01"}"#;
        assert!(parse_private_user_event(event).is_err());
    }

    #[test]
    fn spot_execution_report_fixture_parses_without_default_values() {
        let event = parse_private_user_event(include_str!(
            "../tests/fixtures/binance_private_events/spot_execution_report.json"
        ))
        .expect("official Spot execution report should parse");
        let BinancePrivateEvent::SpotExecution { update, .. } = event else {
            panic!("expected Spot execution report");
        };
        assert_eq!(update.symbol, "BTCUSDT");
        assert_eq!(update.side, Side::Buy);
        assert_eq!(update.last_filled_qty, dec!(0.01));
        assert_eq!(update.commission_amount, dec!(0.10));
        assert_eq!(update.timestamp_ms, 1_700_000_000_005);
    }

    #[tokio::test]
    async fn spot_stream_subscribes_to_local_server_and_shutdown_joins_reader() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local test listener should bind");
        let address = listener.local_addr().expect("listener has local address");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("local client connects");
            let mut websocket = accept_async(socket)
                .await
                .expect("WebSocket handshake works");
            let request = websocket
                .next()
                .await
                .expect("subscription request arrives")
                .expect("subscription frame is valid");
            let Message::Text(request) = request else {
                panic!("subscription request must be text");
            };
            let request: serde_json::Value =
                serde_json::from_str(&request).expect("subscription request is JSON");
            assert_eq!(request["method"], "userDataStream.subscribe.signature");
            assert_eq!(request["params"]["apiKey"], "test-api-key");
            websocket
                .send(Message::Text(
                    r#"{"id":"local-spot-1","status":200,"result":{"subscriptionId":1}}"#.into(),
                ))
                .await
                .expect("subscription acknowledgement is sent");
            websocket
                .send(Message::Text(
                    r#"{"subscriptionId":1,"event":{"e":"outboundAccountPosition","E":1700000000001,"u":1700000000000,"B":[{"a":"USDT","f":"12.50","l":"1.25"}]}}"#.into(),
                ))
                .await
                .expect("wrapped Spot event is sent");
            let close = tokio::time::timeout(std::time::Duration::from_secs(2), websocket.next())
                .await
                .expect("shutdown reaches local server")
                .expect("close frame arrives")
                .expect("close frame is valid");
            assert!(matches!(close, Message::Close(_)));
        });

        let auth = BinanceAuth::new("test-api-key", "test-secret");
        let mut stream = BinancePrivateUserDataStream::connect_spot_with_endpoint(
            &format!("ws://{address}"),
            &auth,
            1_700_000_000_000,
            "local-spot-1",
        )
        .await
        .expect("local signed Spot subscription succeeds");
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(BinancePrivateEvent::SpotAccountPosition(_)))
        ));
        stream.shutdown().await.expect("reader task joins cleanly");
        server.await.expect("local server task joins");
    }

    #[tokio::test]
    async fn spot_stream_marks_out_of_order_account_deltas_as_reconciliation_required() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local test listener should bind");
        let address = listener.local_addr().expect("listener has local address");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("local client connects");
            let mut websocket = accept_async(socket)
                .await
                .expect("WebSocket handshake works");
            let _request = websocket
                .next()
                .await
                .expect("subscription request arrives")
                .expect("subscription frame is valid");
            websocket
                .send(Message::Text(
                    r#"{"id":"stale-spot-1","status":200,"result":{"subscriptionId":1}}"#.into(),
                ))
                .await
                .expect("subscription acknowledgement is sent");
            for (event_time, transaction_time) in [(2_000, 2_000), (1_000, 1_000)] {
                let event = format!(
                    r#"{{"subscriptionId":1,"event":{{"e":"balanceUpdate","E":{event_time},"T":{transaction_time},"a":"USDT","d":"1.0"}}}}"#
                );
                websocket
                    .send(Message::Text(event.into()))
                    .await
                    .expect("balance delta is sent");
            }
        });

        let auth = BinanceAuth::new("test-api-key", "test-secret");
        let mut stream = BinancePrivateUserDataStream::connect_spot_with_endpoint(
            &format!("ws://{address}"),
            &auth,
            1_700_000_000_000,
            "stale-spot-1",
        )
        .await
        .expect("local signed Spot subscription succeeds");
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(BinancePrivateEvent::SpotBalanceDelta(_)))
        ));
        assert_eq!(
            stream.next_event().await,
            Some(Err(BinancePrivateStreamError::EventReconciliationRequired))
        );
        stream.shutdown().await.expect("reader task joins cleanly");
        server.await.expect("local server task joins");
    }

    struct RecordingRenewer {
        close_count: AtomicUsize,
    }

    impl ListenKeyRenewer for RecordingRenewer {
        fn renew<'a>(
            &'a self,
            _listen_key: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn close<'a>(
            &'a self,
            _listen_key: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>> {
            self.close_count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    struct CloseCancellationObserver(Arc<AtomicBool>);

    impl Drop for CloseCancellationObserver {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct PendingCloseRenewer {
        close_started: Arc<AtomicBool>,
        close_cancelled: Arc<AtomicBool>,
    }

    impl ListenKeyRenewer for PendingCloseRenewer {
        fn renew<'a>(
            &'a self,
            _listen_key: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn close<'a>(
            &'a self,
            _listen_key: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>> {
            self.close_started.store(true, Ordering::SeqCst);
            let close_cancelled = Arc::clone(&self.close_cancelled);
            Box::pin(async move {
                let _observer = CloseCancellationObserver(close_cancelled);
                std::future::pending().await
            })
        }
    }

    #[tokio::test]
    async fn dropping_stream_aborts_a_reader_stuck_in_remote_key_cleanup() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local test listener should bind");
        let address = listener.local_addr().expect("listener has local address");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("local client connects");
            let mut websocket = accept_async(socket)
                .await
                .expect("WebSocket handshake works");
            websocket
                .send(Message::Close(None))
                .await
                .expect("server close frame is sent");
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), websocket.next()).await;
        });
        let close_started = Arc::new(AtomicBool::new(false));
        let close_cancelled = Arc::new(AtomicBool::new(false));
        let renewer = Arc::new(PendingCloseRenewer {
            close_started: Arc::clone(&close_started),
            close_cancelled: Arc::clone(&close_cancelled),
        });
        let stream = BinancePrivateUserDataStream::connect_futures_with_endpoint(
            &format!("ws://{address}/synthetic-key"),
            "synthetic-key".to_string(),
            renewer,
            FuturesRenewalPolicy::default(),
        )
        .await
        .expect("local USD-M stream connects");

        for _ in 0..100 {
            if close_started.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(close_started.load(Ordering::SeqCst));
        drop(stream);
        for _ in 0..100 {
            if close_cancelled.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(close_cancelled.load(Ordering::SeqCst));
        server.await.expect("local server task joins");
    }

    #[tokio::test]
    async fn futures_shutdown_closes_the_socket_and_owned_listen_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local test listener should bind");
        let address = listener.local_addr().expect("listener has local address");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("local client connects");
            let mut websocket = accept_async(socket)
                .await
                .expect("WebSocket handshake works");
            let close = tokio::time::timeout(std::time::Duration::from_secs(2), websocket.next())
                .await
                .expect("shutdown reaches local server")
                .expect("close frame arrives")
                .expect("close frame is valid");
            assert!(matches!(close, Message::Close(_)));
        });
        let renewer = Arc::new(RecordingRenewer {
            close_count: AtomicUsize::new(0),
        });
        let mut stream = BinancePrivateUserDataStream::connect_futures_with_endpoint(
            &format!("ws://{address}/synthetic-key"),
            "synthetic-key".to_string(),
            renewer.clone(),
            FuturesRenewalPolicy::default(),
        )
        .await
        .expect("local USD-M stream connects");

        stream
            .shutdown()
            .await
            .expect("listen key cleanup succeeds");
        assert_eq!(renewer.close_count.load(Ordering::SeqCst), 1);
        server.await.expect("local server task joins");
    }

    struct PendingRenewer {
        attempts: AtomicUsize,
    }

    impl ListenKeyRenewer for PendingRenewer {
        fn renew<'a>(
            &'a self,
            _listen_key: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_during_pending_renewal_cancels_without_repolling_shutdown_signal() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local test listener should bind");
        let address = listener.local_addr().expect("listener has local address");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("local client connects");
            let mut websocket = accept_async(socket)
                .await
                .expect("WebSocket handshake works");
            let close = tokio::time::timeout(std::time::Duration::from_secs(2), websocket.next())
                .await
                .expect("shutdown reaches local server")
                .expect("close frame arrives")
                .expect("close frame is valid");
            assert!(matches!(close, Message::Close(_)));
        });
        let renewer = Arc::new(PendingRenewer {
            attempts: AtomicUsize::new(0),
        });
        let policy = FuturesRenewalPolicy {
            interval: std::time::Duration::from_secs(1),
            request_timeout: std::time::Duration::from_secs(10),
            attempts: 1,
            retry_delay: std::time::Duration::from_secs(1),
        };
        let mut stream = BinancePrivateUserDataStream::connect_futures_with_endpoint(
            &format!("ws://{address}/synthetic-key"),
            "synthetic-key".to_string(),
            renewer.clone(),
            policy,
        )
        .await
        .expect("local USD-M stream connects");

        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        for _ in 0..10 {
            if renewer.attempts.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(renewer.attempts.load(Ordering::SeqCst), 1);
        stream
            .shutdown()
            .await
            .expect("shutdown cancels renewal and joins the reader");
        server.await.expect("local server task joins");
    }

    struct FailingRenewer {
        attempts: AtomicUsize,
    }

    impl ListenKeyRenewer for FailingRenewer {
        fn renew<'a>(
            &'a self,
            _listen_key: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(()) })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn futures_listen_key_renewal_uses_fake_time_and_a_bounded_retry_budget() {
        let renewer = Arc::new(FailingRenewer {
            attempts: AtomicUsize::new(0),
        });
        let policy = FuturesRenewalPolicy {
            attempts: 3,
            request_timeout: std::time::Duration::from_secs(1),
            retry_delay: std::time::Duration::from_secs(2),
            interval: std::time::Duration::from_secs(30 * 60),
        };
        let renewal = {
            let renewer = Arc::clone(&renewer);
            tokio::spawn(async move {
                renew_listen_key_with_policy(renewer.as_ref(), "not-logged-key", policy).await
            })
        };

        tokio::task::yield_now().await;
        assert_eq!(renewer.attempts.load(Ordering::SeqCst), 1);
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(renewer.attempts.load(Ordering::SeqCst), 2);
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        assert_eq!(
            renewal.await.expect("renewal task joins"),
            Err(BinancePrivateStreamError::ListenKeyRenewalFailed)
        );
        assert_eq!(renewer.attempts.load(Ordering::SeqCst), 3);
    }
}
