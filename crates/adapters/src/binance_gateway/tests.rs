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

fn spawn_futures_profile_mock_server(
    multi_assets_response: &'static str,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let responses = HashMap::from([(
        "/fapi/v1/multiAssetsMargin",
        multi_assets_response.to_string(),
    )]);
    spawn_futures_mock_server(responses)
}

fn spawn_futures_mock_server(
    responses: HashMap<&'static str, String>,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    spawn_futures_mock_server_with_order_behavior(responses, MockFuturesOrderBehavior::default())
}

#[derive(Clone, Copy)]
enum MockFuturesPostBehavior {
    Accepted,
    DropResponse,
    MalformedResponse,
    Rejected,
}

#[derive(Clone, Copy)]
enum MockFuturesQueryBehavior {
    Found,
    NotFound,
    Malformed,
    FoundAfter(usize),
}

#[derive(Clone)]
struct MockFuturesOrderBehavior {
    post: MockFuturesPostBehavior,
    query: MockFuturesQueryBehavior,
    post_gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
}

impl Default for MockFuturesOrderBehavior {
    fn default() -> Self {
        Self {
            post: MockFuturesPostBehavior::Accepted,
            query: MockFuturesQueryBehavior::Found,
            post_gate: None,
        }
    }
}

#[derive(Clone)]
struct MockFuturesOrder {
    symbol: String,
    side: String,
    quantity: String,
    price: String,
    client_order_id: String,
}

fn request_parameter(request_line: &str, name: &str) -> Option<String> {
    let query = request_line.split_whitespace().nth(1)?.split_once('?')?.1;
    query.split('&').find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn mock_futures_order_response(order: &MockFuturesOrder) -> String {
    format!(
        r#"{{"symbol":"{}","orderId":1,"clientOrderId":"{}","side":"{}","status":"NEW","origQty":"{}","executedQty":"0","cumQuote":"0","price":"{}","updateTime":1700000000000}}"#,
        order.symbol, order.client_order_id, order.side, order.quantity, order.price
    )
}

fn spawn_futures_mock_server_with_order_behavior(
    responses: HashMap<&'static str, String>,
    order_behavior: MockFuturesOrderBehavior,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let orders = Arc::new(Mutex::new(HashMap::<String, MockFuturesOrder>::new()));
    let captured_orders = Arc::clone(&orders);
    let query_count = Arc::new(Mutex::new(0_usize));
    let captured_query_count = Arc::clone(&query_count);

    let handle = thread::spawn(move || {
        let mut last_request = Instant::now();
        while last_request.elapsed() < Duration::from_millis(200) {
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

            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or_default();
            let (status, body, drop_response) = if request_line
                .starts_with("POST /fapi/v1/leverage")
            {
                (
                    200,
                    r#"{"symbol":"BTCUSDT","leverage":2,"maxNotionalValue":"50000"}"#.to_string(),
                    false,
                )
            } else if request_line.starts_with("POST /fapi/v1/order") {
                let order = MockFuturesOrder {
                    symbol: request_parameter(&request_line, "symbol")
                        .unwrap_or_else(|| "BTCUSDT".to_string()),
                    side: request_parameter(&request_line, "side")
                        .unwrap_or_else(|| "BUY".to_string()),
                    quantity: request_parameter(&request_line, "quantity")
                        .unwrap_or_else(|| "0".to_string()),
                    price: request_parameter(&request_line, "price")
                        .unwrap_or_else(|| "0".to_string()),
                    client_order_id: request_parameter(&request_line, "newClientOrderId")
                        .unwrap_or_default(),
                };
                captured_orders
                    .lock()
                    .unwrap()
                    .insert(order.client_order_id.clone(), order.clone());
                if let Some(gate) = order_behavior.post_gate.as_ref() {
                    let (released, condition) = &**gate;
                    let mut released = released.lock().unwrap();
                    while !*released {
                        released = condition.wait(released).unwrap();
                    }
                }
                match order_behavior.post {
                    MockFuturesPostBehavior::Accepted => {
                        (200, mock_futures_order_response(&order), false)
                    }
                    MockFuturesPostBehavior::DropResponse => (200, String::new(), true),
                    MockFuturesPostBehavior::MalformedResponse => (200, "{}".to_string(), false),
                    MockFuturesPostBehavior::Rejected => (
                        400,
                        r#"{"code":-2010,"msg":"Order rejected"}"#.to_string(),
                        false,
                    ),
                }
            } else if request_line.starts_with("GET /fapi/v1/order") {
                match order_behavior.query {
                    MockFuturesQueryBehavior::Found => {
                        let client_order_id = request_parameter(&request_line, "origClientOrderId")
                            .unwrap_or_default();
                        let order = captured_orders
                            .lock()
                            .unwrap()
                            .get(&client_order_id)
                            .cloned();
                        match order {
                            Some(order) => (200, mock_futures_order_response(&order), false),
                            None => (
                                400,
                                r#"{"code":-2013,"msg":"Order does not exist"}"#.to_string(),
                                false,
                            ),
                        }
                    }
                    MockFuturesQueryBehavior::NotFound => (
                        400,
                        r#"{"code":-2013,"msg":"Order does not exist"}"#.to_string(),
                        false,
                    ),
                    MockFuturesQueryBehavior::Malformed => (200, "{}".to_string(), false),
                    MockFuturesQueryBehavior::FoundAfter(required_attempts) => {
                        let mut count = captured_query_count.lock().unwrap();
                        *count += 1;
                        if *count < required_attempts {
                            (
                                400,
                                r#"{"code":-2013,"msg":"Order does not exist"}"#.to_string(),
                                false,
                            )
                        } else {
                            let client_order_id =
                                request_parameter(&request_line, "origClientOrderId")
                                    .unwrap_or_default();
                            match captured_orders
                                .lock()
                                .unwrap()
                                .get(&client_order_id)
                                .cloned()
                            {
                                Some(order) => (200, mock_futures_order_response(&order), false),
                                None => (
                                    400,
                                    r#"{"code":-2013,"msg":"Order does not exist"}"#.to_string(),
                                    false,
                                ),
                            }
                        }
                    }
                }
            } else {
                (
                    200,
                    responses.get(path).cloned().unwrap_or_else(|| {
                        r#"{"code":-1121,"msg":"Invalid mock request"}"#.to_string()
                    }),
                    false,
                )
            };
            if drop_response {
                drop(stream);
                continue;
            }
            let status_text = match status {
                200 => "OK",
                400 => "BAD REQUEST",
                _ => "MOCK STATUS",
            };
            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    (format!("http://{address}"), requests, handle)
}

fn valid_futures_responses(mark_timestamp_ms: u64) -> HashMap<&'static str, String> {
    HashMap::from([
        (
            "/fapi/v1/multiAssetsMargin",
            r#"{"multiAssetsMargin":false}"#.to_string(),
        ),
        (
            "/fapi/v1/positionSide/dual",
            r#"{"dualSidePosition":false}"#.to_string(),
        ),
        (
            "/fapi/v3/account",
            r#"{"canTrade":true,"availableBalance":"500.4","assets":[{"asset":"USDT","availableBalance":"500.4","walletBalance":"500.4"}]}"#.to_string(),
        ),
        (
            "/fapi/v3/positionRisk",
            "[]".to_string(),
        ),
        ("/fapi/v1/openOrders", "[]".to_string()),
        ("/fapi/v1/openAlgoOrders", "[]".to_string()),
        (
            "/fapi/v1/exchangeInfo",
            r#"{"symbols":[{"symbol":"BTCUSDT","status":"TRADING","contractType":"PERPETUAL","quoteAsset":"USDT","marginAsset":"USDT","filters":[{"filterType":"PRICE_FILTER","minPrice":"0.01","maxPrice":"1000000","tickSize":"0.01"},{"filterType":"LOT_SIZE","minQty":"0.001","maxQty":"1000","stepSize":"0.001"},{"filterType":"MIN_NOTIONAL","notional":"5"}]}]}"#.to_string(),
        ),
        (
            "/fapi/v1/premiumIndex",
            format!(
                r#"{{"symbol":"BTCUSDT","markPrice":"100","time":{mark_timestamp_ms}}}"#
            ),
        ),
        (
            "/fapi/v1/leverageBracket",
            r#"[{"symbol":"BTCUSDT","brackets":[{"bracket":1,"initialLeverage":20,"notionalFloor":"0","notionalCap":"50000","maintMarginRatio":"0.005","cum":"0"}]}]"#.to_string(),
        ),
        (
            "/fapi/v1/commissionRate",
            r#"{"symbol":"BTCUSDT","makerCommissionRate":"0.0002","takerCommissionRate":"0.0004"}"#.to_string(),
        ),
        (
            "/fapi/v1/symbolConfig",
            r#"[{"symbol":"BTCUSDT","marginType":"isolated","leverage":20,"maxNotionalValue":"50000","isAutoAddMargin":"false"}]"#.to_string(),
        ),
    ])
}

#[tokio::test]
async fn futures_preflight_rejects_multi_asset_mode_without_dispatch() {
    let (base_url, requests, server) =
        spawn_futures_profile_mock_server(r#"{"multiAssetsMargin":true}"#);
    let config = BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    };
    let mut gateway = BinanceGateway::new(config).unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let result = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    assert!(result.is_err(), "multi-asset mode must be rejected");
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert!(requests
        .iter()
        .any(|request| request.starts_with("GET /fapi/v1/multiAssetsMargin?")));
    assert!(!requests
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order")));
}

#[tokio::test]
async fn futures_preflight_rejects_hedge_mode_without_dispatch() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert(
        "/fapi/v1/positionSide/dual",
        r#"{"dualSidePosition":true}"#.to_string(),
    );
    let (base_url, requests, server) = spawn_futures_mock_server(responses);
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let result = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    assert!(matches!(result, Err(GatewayError::FuturesPreflight(_))));
    server.join().unwrap();
    assert!(!requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order")));
}

#[tokio::test]
async fn futures_preflight_rejects_cross_margin_without_dispatch() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert(
        "/fapi/v1/symbolConfig",
        r#"[{"symbol":"BTCUSDT","marginType":"cross","leverage":20,"maxNotionalValue":"50000","isAutoAddMargin":"false"}]"#.to_string(),
    );
    let (base_url, requests, server) = spawn_futures_mock_server(responses);
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let result = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    assert!(matches!(result, Err(GatewayError::FuturesPreflight(_))));
    server.join().unwrap();
    assert!(!requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order")));
}

#[tokio::test]
async fn futures_preflight_rejects_stale_mark_without_dispatch() {
    let now_ms = current_unix_time_ms().unwrap();
    let (base_url, requests, server) =
        spawn_futures_mock_server(valid_futures_responses(now_ms - 2_001));
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let result = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    assert!(matches!(result, Err(GatewayError::FuturesPreflight(_))));
    server.join().unwrap();
    assert!(!requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order")));
}

#[tokio::test]
async fn futures_preflight_rejects_open_conditional_orders_without_dispatch() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert(
        "/fapi/v1/openAlgoOrders",
        r#"[{"symbol":"BTCUSDT","algoType":"CONDITIONAL","orderType":"STOP"}]"#.to_string(),
    );
    let (base_url, requests, server) = spawn_futures_mock_server(responses);
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let result = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    assert!(matches!(result, Err(GatewayError::FuturesPreflight(_))));
    server.join().unwrap();
    assert!(!requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order")));
}

#[tokio::test]
async fn futures_preflight_rejects_quantity_above_max_qty_without_dispatch() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert(
        "/fapi/v1/exchangeInfo",
        r#"{"symbols":[{"symbol":"BTCUSDT","status":"TRADING","contractType":"PERPETUAL","quoteAsset":"USDT","marginAsset":"USDT","filters":[{"filterType":"PRICE_FILTER","minPrice":"0.01","maxPrice":"1000000","tickSize":"0.01"},{"filterType":"LOT_SIZE","minQty":"0.001","maxQty":"5","stepSize":"0.001"},{"filterType":"MIN_NOTIONAL","notional":"5"}]}]}"#.to_string(),
    );
    let (base_url, requests, server) = spawn_futures_mock_server(responses);
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let result = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    assert!(matches!(result, Err(GatewayError::FuturesPreflight(_))));
    server.join().unwrap();
    assert!(!requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order")));
}

#[tokio::test]
async fn futures_preflight_chooses_minimum_supported_leverage_before_dispatch() {
    let (base_url, requests, server) =
        spawn_futures_mock_server(valid_futures_responses(current_unix_time_ms().unwrap()));
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let report = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await
        .unwrap();

    assert_eq!(report.original_qty, dec!(10));
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    let leverage_request = requests
        .iter()
        .find(|request| request.starts_with("POST /fapi/v1/leverage?"))
        .expect("minimum leverage should be configured");
    assert!(leverage_request.contains("leverage=2"));
    assert!(requests
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order?")));
}

fn account_with_foreign_collateral() -> String {
    r#"{"canTrade":true,"availableBalance":"500.4","assets":[{"asset":"USDT","availableBalance":"500.4","walletBalance":"500.4"},{"asset":"BTC","availableBalance":"0.01","walletBalance":"0.01"},{"asset":"USDC","availableBalance":"5000","walletBalance":"5000"}]}"#.to_string()
}

#[tokio::test]
async fn foreign_collateral_is_refused_by_default_when_placing_an_order() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert("/fapi/v3/account", account_with_foreign_collateral());
    let (base_url, _requests, server) = spawn_futures_mock_server(responses);
    let gateway = futures_submission_test_gateway(base_url);

    let result = submit_futures_test_order(&gateway).await;
    server.join().unwrap();

    assert!(
        matches!(&result, Err(GatewayError::FuturesPreflight(reason)) if reason.contains("non-USDT")),
        "{result:?}"
    );
}

#[tokio::test]
async fn foreign_collateral_is_accepted_only_with_the_explicit_testnet_tolerance() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert("/fapi/v3/account", account_with_foreign_collateral());
    let (base_url, requests, server) = spawn_futures_mock_server(responses);
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        testnet: true,
        allow_foreign_collateral: true,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;

    let result = submit_futures_test_order(&gateway).await;
    server.join().unwrap();

    assert!(result.is_ok(), "{result:?}");
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order?")));
}

#[test]
fn the_tolerance_cannot_be_enabled_outside_testnet_or_on_spot() {
    let production = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        testnet: false,
        allow_foreign_collateral: true,
        ..Default::default()
    });
    let spot = BinanceGateway::new(BinanceGatewayConfig {
        spot: true,
        testnet: true,
        allow_foreign_collateral: true,
        ..Default::default()
    });
    let testnet_futures = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        testnet: true,
        allow_foreign_collateral: true,
        ..Default::default()
    });

    assert!(matches!(production, Err(GatewayError::Config(_))));
    assert!(matches!(spot, Err(GatewayError::Config(_))));
    assert!(testnet_futures.is_ok());
}

#[tokio::test]
async fn the_gateway_acts_as_an_execution_venue_for_entries() {
    use ports::ExecutionVenue;

    let (base_url, requests, server) =
        spawn_futures_mock_server(valid_futures_responses(current_unix_time_ms().unwrap()));
    let gateway = futures_submission_test_gateway(base_url);
    let before = current_unix_time_ms().unwrap() as i64;

    let ack = gateway
        .submit_entry(
            "BTCUSDT",
            &futures_submission_test_intent(),
            dec!(10_000),
            dec!(10_000),
        )
        .await
        .unwrap();
    let after = current_unix_time_ms().unwrap() as i64;

    server.join().unwrap();
    // The send stamp is the moment the request left, inside the call and not zero.
    assert!(ack.sent_at_ms >= before && ack.sent_at_ms <= after);
    assert_eq!(ack.quantity, dec!(10));
    assert_eq!(ack.status, "NEW");
    assert!(!ack.order_id.is_empty());
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order?")));
}

#[test]
fn an_execution_without_a_cumulative_quote_is_not_an_acknowledgement() {
    // Only an order that has not executed anything may omit `cumQuote`.
    let body: serde_json::Value = serde_json::from_str(
        r#"{"orderId":1,"symbol":"BTCUSDT","status":"FILLED","clientOrderId":"x","price":"0.00","origQty":"0.002","executedQty":"0.002","side":"BUY","updateTime":1792178222682}"#,
    )
    .unwrap();
    let expected = FuturesOrderRequest {
        symbol: "BTCUSDT".to_string(),
        client_order_id: "x".to_string(),
        side: Side::Buy,
        price: Decimal::ZERO,
        quantity: dec!(0.002),
    };

    assert!(parse_futures_order_report(&body, &expected).is_err());
}

#[test]
fn the_real_market_order_acknowledgement_without_cumulative_quote_is_accepted() {
    // Captured from Binance Futures testnet: a MARKET ack reports price 0 and no `cumQuote`.
    let body: serde_json::Value = serde_json::from_str(
        r#"{"orderId":28636945020,"symbol":"BTCUSDT","status":"NEW","clientOrderId":"probe-market-1","price":"0.00","origQty":"0.0020","executedQty":"0.0000","cumQty":"0.0000","timeInForce":"GTC","type":"MARKET","reduceOnly":false,"closePosition":false,"side":"BUY","positionSide":"BOTH","stopPrice":"0.00","workingType":"CONTRACT_PRICE","priceProtect":false,"origType":"MARKET","priceMatch":"NONE","selfTradePreventionMode":"EXPIRE_MAKER","goodTillDate":0,"updateTime":1792178222682}"#,
    )
    .unwrap();
    let expected = FuturesOrderRequest {
        symbol: "BTCUSDT".to_string(),
        client_order_id: "probe-market-1".to_string(),
        side: Side::Buy,
        price: Decimal::ZERO,
        quantity: dec!(0.002),
    };

    let report = parse_futures_order_report(&body, &expected).unwrap();

    assert_eq!(report.status, "NEW");
    assert_eq!(report.original_qty, dec!(0.002));
    assert_eq!(report.cumulative_quote_qty, Decimal::ZERO);
}

#[tokio::test]
async fn futures_entries_are_market_orders_without_a_limit_price() {
    let (base_url, requests, server) =
        spawn_futures_mock_server(valid_futures_responses(current_unix_time_ms().unwrap()));
    let gateway = futures_submission_test_gateway(base_url);

    let report = submit_futures_test_order(&gateway).await.unwrap();

    server.join().unwrap();
    let requests = requests.lock().unwrap();
    let post = requests
        .iter()
        .find(|request| request.starts_with("POST /fapi/v1/order?"))
        .expect("order should be submitted");
    assert_eq!(request_parameter(post, "type").as_deref(), Some("MARKET"));
    assert_eq!(request_parameter(post, "price"), None);
    assert_eq!(request_parameter(post, "timeInForce"), None);
    assert_eq!(report.original_qty, dec!(10));
}

#[tokio::test]
async fn a_market_entry_does_not_require_a_tick_aligned_reference_price() {
    let (base_url, _requests, server) =
        spawn_futures_mock_server(valid_futures_responses(current_unix_time_ms().unwrap()));
    let gateway = futures_submission_test_gateway(base_url);
    let intent = OrderIntent::new(1, Side::Buy, dec!(100.005), dec!(90), dec!(110), 5).unwrap();

    let report = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    server.join().unwrap();
    assert!(report.is_ok(), "market entry rejected: {report:?}");
}

#[tokio::test]
async fn futures_order_quantity_uses_the_configured_risk_per_trade() {
    let (base_url, requests, server) =
        spawn_futures_mock_server(valid_futures_responses(current_unix_time_ms().unwrap()));
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        risk_per_trade_pct: dec!(0.02),
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let report = gateway
        .place_order("BTCUSDT", &intent, dec!(5_000), dec!(5_000), Decimal::ZERO)
        .await
        .unwrap();

    // 2% of 5_000 over a 10-unit stop distance is 10; the default 1% would size only 5.
    assert_eq!(report.original_qty, dec!(10));
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    let post = requests
        .iter()
        .find(|request| request.starts_with("POST /fapi/v1/order?"))
        .expect("order should be submitted");
    let submitted: Decimal = request_parameter(post, "quantity")
        .expect("order should carry a quantity")
        .parse()
        .unwrap();
    assert_eq!(submitted, dec!(10));
}

fn futures_submission_test_gateway(base_url: String) -> BinanceGateway {
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    gateway
}

fn futures_submission_test_intent() -> OrderIntent {
    OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap()
}

async fn submit_futures_test_order(
    gateway: &BinanceGateway,
) -> Result<OrderExecutionReport, GatewayError> {
    gateway
        .place_order(
            "BTCUSDT",
            &futures_submission_test_intent(),
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await
}

#[tokio::test]
async fn futures_submission_recovers_lost_response_by_client_order_id() {
    let behavior = MockFuturesOrderBehavior {
        post: MockFuturesPostBehavior::DropResponse,
        query: MockFuturesQueryBehavior::Found,
        ..Default::default()
    };
    let (base_url, requests, server) = spawn_futures_mock_server_with_order_behavior(
        valid_futures_responses(current_unix_time_ms().unwrap()),
        behavior,
    );
    let gateway = futures_submission_test_gateway(base_url);

    let report = submit_futures_test_order(&gateway).await.unwrap();

    server.join().unwrap();
    let requests = requests.lock().unwrap();
    let post = requests
        .iter()
        .find(|request| request.starts_with("POST /fapi/v1/order?"))
        .expect("order should be submitted once");
    let query = requests
        .iter()
        .find(|request| request.starts_with("GET /fapi/v1/order?"))
        .expect("lost response should trigger an order lookup");
    let submitted_id = request_parameter(post, "newClientOrderId")
        .expect("submission should have a client order ID");
    assert!(!submitted_id.is_empty());
    assert!(submitted_id.len() <= 36);
    assert!(submitted_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"_:/.-".contains(&byte)));
    assert_eq!(
        request_parameter(query, "origClientOrderId"),
        Some(submitted_id.clone())
    );
    assert_eq!(report.client_order_id, submitted_id);
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("POST /fapi/v1/order?"))
            .count(),
        1
    );
}

#[tokio::test]
async fn futures_submission_remains_blocked_when_order_lookup_stays_missing() {
    let behavior = MockFuturesOrderBehavior {
        post: MockFuturesPostBehavior::DropResponse,
        query: MockFuturesQueryBehavior::NotFound,
        ..Default::default()
    };
    let (base_url, requests, server) = spawn_futures_mock_server_with_order_behavior(
        valid_futures_responses(current_unix_time_ms().unwrap()),
        behavior,
    );
    let gateway = futures_submission_test_gateway(base_url);

    let first = submit_futures_test_order(&gateway).await;
    let client_order_id = match first {
        Err(GatewayError::FuturesSubmissionUncertain {
            symbol,
            client_order_id,
        }) => {
            assert_eq!(symbol, "BTCUSDT");
            client_order_id
        }
        other => panic!("expected an uncertain submission, got {other:?}"),
    };
    let retry = submit_futures_test_order(&gateway).await;

    assert!(matches!(
        retry,
        Err(GatewayError::FuturesSubmissionUncertain {
            symbol,
            client_order_id: retry_id,
        }) if symbol == "BTCUSDT" && retry_id == client_order_id
    ));
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("POST /fapi/v1/order?"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("GET /fapi/v1/order?"))
            .count(),
        3
    );
}

#[tokio::test]
async fn futures_submission_remains_blocked_when_order_lookup_is_malformed() {
    let behavior = MockFuturesOrderBehavior {
        post: MockFuturesPostBehavior::DropResponse,
        query: MockFuturesQueryBehavior::Malformed,
        ..Default::default()
    };
    let (base_url, requests, server) = spawn_futures_mock_server_with_order_behavior(
        valid_futures_responses(current_unix_time_ms().unwrap()),
        behavior,
    );
    let gateway = futures_submission_test_gateway(base_url);

    assert!(matches!(
        submit_futures_test_order(&gateway).await,
        Err(GatewayError::FuturesSubmissionUncertain { .. })
    ));
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("GET /fapi/v1/order?"))
            .count(),
        3
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("POST /fapi/v1/order?"))
            .count(),
        1
    );
}

#[tokio::test]
async fn futures_submission_can_be_reconciled_later_without_resubmitting_it() {
    let behavior = MockFuturesOrderBehavior {
        post: MockFuturesPostBehavior::DropResponse,
        query: MockFuturesQueryBehavior::FoundAfter(4),
        ..Default::default()
    };
    let (base_url, requests, server) = spawn_futures_mock_server_with_order_behavior(
        valid_futures_responses(current_unix_time_ms().unwrap()),
        behavior,
    );
    let gateway = futures_submission_test_gateway(base_url);

    let uncertain_id = match submit_futures_test_order(&gateway).await {
        Err(GatewayError::FuturesSubmissionUncertain {
            client_order_id, ..
        }) => client_order_id,
        other => panic!("expected uncertainty after three missing lookups, got {other:?}"),
    };
    let reconciled = gateway
        .reconcile_uncertain_futures_submission()
        .await
        .unwrap();
    assert_eq!(reconciled.client_order_id, uncertain_id);

    let next_order = submit_futures_test_order(&gateway).await.unwrap();
    assert_ne!(next_order.client_order_id, uncertain_id);
    server.join().unwrap();
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("POST /fapi/v1/order?"))
            .count(),
        2
    );
}

#[tokio::test]
async fn futures_submission_recovers_malformed_success_response_from_order_lookup() {
    let behavior = MockFuturesOrderBehavior {
        post: MockFuturesPostBehavior::MalformedResponse,
        query: MockFuturesQueryBehavior::Found,
        ..Default::default()
    };
    let (base_url, requests, server) = spawn_futures_mock_server_with_order_behavior(
        valid_futures_responses(current_unix_time_ms().unwrap()),
        behavior,
    );
    let gateway = futures_submission_test_gateway(base_url);

    let report = submit_futures_test_order(&gateway).await.unwrap();

    assert_eq!(report.order_id, 1);
    server.join().unwrap();
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("GET /fapi/v1/order?")));
}

#[tokio::test]
async fn futures_submission_releases_reservation_after_definitive_rejection() {
    let behavior = MockFuturesOrderBehavior {
        post: MockFuturesPostBehavior::Rejected,
        ..Default::default()
    };
    let (base_url, requests, server) = spawn_futures_mock_server_with_order_behavior(
        valid_futures_responses(current_unix_time_ms().unwrap()),
        behavior,
    );
    let gateway = futures_submission_test_gateway(base_url);

    for _ in 0..2 {
        assert!(matches!(
            submit_futures_test_order(&gateway).await,
            Err(GatewayError::BinanceApi { code: -2010, .. })
        ));
    }

    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("POST /fapi/v1/order?"))
            .count(),
        2
    );
    assert!(!requests
        .iter()
        .any(|r| r.starts_with("GET /fapi/v1/order?")));
}

#[tokio::test]
async fn futures_submission_cancellation_after_dispatch_keeps_reservation_uncertain() {
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let behavior = MockFuturesOrderBehavior {
        post: MockFuturesPostBehavior::Accepted,
        query: MockFuturesQueryBehavior::Found,
        post_gate: Some(Arc::clone(&gate)),
    };
    let (base_url, requests, server) = spawn_futures_mock_server_with_order_behavior(
        valid_futures_responses(current_unix_time_ms().unwrap()),
        behavior,
    );
    let gateway = Arc::new(futures_submission_test_gateway(base_url));
    let first_gateway = Arc::clone(&gateway);
    let first = tokio::spawn(async move { submit_futures_test_order(&first_gateway).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.starts_with("POST /fapi/v1/order?"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("submission should reach its gated dispatch");

    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(matches!(
        submit_futures_test_order(&gateway).await,
        Err(GatewayError::FuturesSubmissionUncertain { .. })
    ));

    let (released, condition) = &*gate;
    *released.lock().unwrap() = true;
    condition.notify_all();
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /fapi/v1/order?"))
            .count(),
        1
    );
    assert!(!requests
        .iter()
        .any(|request| request.starts_with("GET /fapi/v1/order?")));
}

#[tokio::test]
async fn futures_submission_blocks_overlapping_orders_before_dispatch() {
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let behavior = MockFuturesOrderBehavior {
        post: MockFuturesPostBehavior::Accepted,
        query: MockFuturesQueryBehavior::Found,
        post_gate: Some(Arc::clone(&gate)),
    };
    let (base_url, requests, server) = spawn_futures_mock_server_with_order_behavior(
        valid_futures_responses(current_unix_time_ms().unwrap()),
        behavior,
    );
    let gateway = Arc::new(futures_submission_test_gateway(base_url));
    let first_gateway = Arc::clone(&gateway);
    let first = tokio::spawn(async move { submit_futures_test_order(&first_gateway).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.starts_with("POST /fapi/v1/order?"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first submission should reach its gated dispatch");

    let second_gateway = Arc::clone(&gateway);
    let mut second = tokio::spawn(async move { submit_futures_test_order(&second_gateway).await });
    let second_result_while_gated = tokio::time::timeout(Duration::from_millis(250), &mut second)
        .await
        .ok();
    let second_completed_while_gated = second_result_while_gated.is_some();

    let (released, condition) = &*gate;
    *released.lock().unwrap() = true;
    condition.notify_all();
    let first_result = first.await.unwrap();
    let second_result = match second_result_while_gated {
        Some(result) => result.unwrap(),
        None => second.await.unwrap(),
    };

    assert!(first_result.is_ok());
    assert!(second_completed_while_gated);
    assert!(matches!(
        second_result,
        Err(GatewayError::FuturesSubmissionInProgress { .. })
    ));
    server.join().unwrap();
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("POST /fapi/v1/order?"))
            .count(),
        1
    );
}

#[tokio::test]
async fn futures_preflight_preserves_leverage_with_existing_position_and_order() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert(
        "/fapi/v3/positionRisk",
        r#"[{"symbol":"BTCUSDT","positionSide":"BOTH","positionAmt":"1","notional":"100","marginAsset":"USDT","isolatedWallet":"50","updateTime":1}]"#.to_string(),
    );
    responses.insert(
        "/fapi/v1/openOrders",
        r#"[{"symbol":"BTCUSDT","positionSide":"BOTH","type":"LIMIT","side":"BUY","price":"100","origQty":"1","executedQty":"0"}]"#.to_string(),
    );
    let (base_url, requests, server) = spawn_futures_mock_server(responses);
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let result = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    assert!(result.is_ok());
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert!(!requests
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/leverage?")));
    assert!(requests
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order?")));
}

#[tokio::test]
async fn futures_preflight_accepts_an_account_response_without_can_trade() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert(
        "/fapi/v3/account",
        r#"{"availableBalance":"500.4","assets":[{"asset":"USDT","availableBalance":"500.4","walletBalance":"500.4"}]}"#.to_string(),
    );
    let (base_url, requests, server) = spawn_futures_mock_server(responses);
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;

    let result = gateway.fetch_futures_preflight_snapshot("BTCUSDT").await;

    server.join().unwrap();
    drop(requests);
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn futures_preflight_rejects_insufficient_margin_without_dispatch() {
    let mut responses = valid_futures_responses(current_unix_time_ms().unwrap());
    responses.insert(
        "/fapi/v3/account",
        r#"{"canTrade":true,"availableBalance":"20","assets":[{"asset":"USDT","availableBalance":"20","walletBalance":"20"}]}"#.to_string(),
    );
    let (base_url, requests, server) = spawn_futures_mock_server(responses);
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        spot: false,
        ..Default::default()
    })
    .unwrap();
    gateway.base_url = base_url;
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(90), dec!(110), 5).unwrap();

    let result = gateway
        .place_order(
            "BTCUSDT",
            &intent,
            dec!(10_000),
            dec!(10_000),
            Decimal::ZERO,
        )
        .await;

    assert!(matches!(result, Err(GatewayError::FuturesPreflight(_))));
    server.join().unwrap();
    assert!(!requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("POST /fapi/v1/order")));
}

fn preflight_snapshot_with_mark_receipt(
    mark_time_ms: u64,
    mark_received_ms: u64,
) -> Result<FuturesPreflightSnapshot, GatewayError> {
    let responses = valid_futures_responses(mark_time_ms);
    let json = |path: &str| -> serde_json::Value {
        serde_json::from_str(&responses[path]).expect("valid fixture JSON")
    };
    let observed = std::time::Instant::now();
    parse_futures_preflight_snapshot(FuturesPreflightInput {
        symbol: "BTCUSDT",
        observed_at: [observed; 5],
        account: &json("/fapi/v3/account"),
        positions: &json("/fapi/v3/positionRisk"),
        open_orders: &json("/fapi/v1/openOrders"),
        algo_orders: &json("/fapi/v1/openAlgoOrders"),
        exchange_info: &json("/fapi/v1/exchangeInfo"),
        mark: &json("/fapi/v1/premiumIndex"),
        leverage_brackets: &json("/fapi/v1/leverageBracket"),
        commission: &json("/fapi/v1/commissionRate"),
        symbol_config: &json("/fapi/v1/symbolConfig"),
        mark_received_ms,
        allow_foreign_collateral: false,
    })
}

fn preflight_snapshot_with_overrides(
    overrides: &[(&str, &str)],
) -> Result<FuturesPreflightSnapshot, GatewayError> {
    let mark_time = 1_700_000_000_000;
    let mut responses = valid_futures_responses(mark_time);
    for (path, body) in overrides {
        responses.insert(path, body.to_string());
    }
    let json = |path: &str| -> serde_json::Value {
        serde_json::from_str(&responses[path]).expect("valid fixture JSON")
    };
    let observed = std::time::Instant::now();
    parse_futures_preflight_snapshot(FuturesPreflightInput {
        symbol: "BTCUSDT",
        observed_at: [observed; 5],
        account: &json("/fapi/v3/account"),
        positions: &json("/fapi/v3/positionRisk"),
        open_orders: &json("/fapi/v1/openOrders"),
        algo_orders: &json("/fapi/v1/openAlgoOrders"),
        exchange_info: &json("/fapi/v1/exchangeInfo"),
        mark: &json("/fapi/v1/premiumIndex"),
        leverage_brackets: &json("/fapi/v1/leverageBracket"),
        commission: &json("/fapi/v1/commissionRate"),
        symbol_config: &json("/fapi/v1/symbolConfig"),
        mark_received_ms: mark_time + 100,
        allow_foreign_collateral: false,
    })
}

const UNSET_LEVERAGE_CONFIG: &str = r#"[{"symbol":"BTCUSDT","marginType":"isolated","leverage":0,"maxNotionalValue":"500000000","isAutoAddMargin":"false"}]"#;

#[test]
fn a_symbol_whose_leverage_was_never_set_is_accepted_while_the_account_is_flat() {
    // Testnet reports leverage 0 for a symbol the account never configured; the gateway
    // chooses and sets the leverage of every order itself.
    let snapshot =
        preflight_snapshot_with_overrides(&[("/fapi/v1/symbolConfig", UNSET_LEVERAGE_CONFIG)])
            .unwrap();

    assert_eq!(snapshot.current_leverage, 0);
    assert_eq!(snapshot.maximum_notional, dec!(500_000_000));
}

#[test]
fn an_unset_leverage_is_still_refused_when_a_position_or_orders_exist() {
    // With exposure the exchange's current leverage is what the margin math runs on.
    let with_position = preflight_snapshot_with_overrides(&[
        ("/fapi/v1/symbolConfig", UNSET_LEVERAGE_CONFIG),
        (
            "/fapi/v3/positionRisk",
            r#"[{"symbol":"BTCUSDT","positionSide":"BOTH","positionAmt":"1","notional":"100","marginAsset":"USDT","isolatedWallet":"50","updateTime":1}]"#,
        ),
    ]);

    assert!(
        matches!(&with_position, Err(GatewayError::FuturesPreflight(r)) if r.contains("invalid leverage")),
        "{with_position:?}"
    );
}

#[test]
fn a_non_positive_notional_bound_is_always_refused() {
    let zero_notional = preflight_snapshot_with_overrides(&[(
        "/fapi/v1/symbolConfig",
        r#"[{"symbol":"BTCUSDT","marginType":"isolated","leverage":20,"maxNotionalValue":"0","isAutoAddMargin":"false"}]"#,
    )]);

    assert!(zero_notional.is_err());
}

#[test]
fn the_mark_price_age_is_judged_when_it_was_received_not_when_the_slowest_call_returned() {
    let mark_time = 1_700_000_000_000;

    // Received 1.5 s after its exchange timestamp; the slowest preflight call returned 3.5 s
    // after it. The snapshot age guard bounds that wait separately, so this is accepted.
    let snapshot = preflight_snapshot_with_mark_receipt(mark_time, mark_time + 1_500);

    assert!(snapshot.is_ok(), "{snapshot:?}");
    assert_eq!(snapshot.unwrap().mark_received_ms, mark_time + 1_500);
}

#[test]
fn a_mark_price_already_stale_or_ahead_when_received_is_rejected() {
    let mark_time = 1_700_000_000_000;

    let stale = preflight_snapshot_with_mark_receipt(mark_time, mark_time + 2_001);
    let ahead = preflight_snapshot_with_mark_receipt(mark_time, mark_time - 1);

    assert!(
        matches!(stale, Err(GatewayError::FuturesPreflight(reason)) if reason.contains("stale"))
    );
    assert!(ahead.is_err());
}

#[tokio::test]
async fn futures_freshness_accepts_two_seconds_and_rejects_older_inputs() {
    let now = std::time::Instant::now();
    let fresh = [now - FUTURES_MAX_DATA_AGE; 5];
    let stale = [now - FUTURES_MAX_DATA_AGE - std::time::Duration::from_nanos(1); 5];

    assert!(validate_futures_snapshot_age(&fresh, now).is_ok());
    assert!(validate_futures_snapshot_age(&stale, now).is_err());
    assert!(validate_futures_mark_timestamp(18_000, 20_000).is_ok());
    assert!(validate_futures_mark_timestamp(17_999, 20_000).is_err());
    assert!(validate_futures_mark_timestamp(20_001, 20_000).is_err());
}

#[test]
fn futures_bracket_boundary_uses_the_tighter_tier() {
    let brackets = [
        FuturesLeverageBracket {
            notional_floor: Decimal::ZERO,
            notional_cap: dec!(50_000),
            maintenance_rate: dec!(0.005),
            maintenance_amount: Decimal::ZERO,
            max_leverage: 20,
        },
        FuturesLeverageBracket {
            notional_floor: dec!(50_000),
            notional_cap: dec!(250_000),
            maintenance_rate: dec!(0.01),
            maintenance_amount: dec!(250),
            max_leverage: 10,
        },
    ];

    let bracket = bracket_for_notional(&brackets, dec!(50_000)).unwrap();

    assert_eq!(bracket.max_leverage, 10);
    assert_eq!(bracket.maintenance_rate, dec!(0.01));
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
        let (stop_loss, take_profit) = match side {
            Side::Buy => (dec!(49), dec!(51)),
            Side::Sell => (dec!(51), dec!(49)),
        };
        let intent = OrderIntent::new(1, side, dec!(50), stop_loss, take_profit, 5).unwrap();

        let result = gateway
            .place_order("BTCUSDT", &intent, dec!(100), dec!(100), Decimal::ZERO)
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
        .place_order("BTCUSDT", &intent, dec!(100), dec!(100), Decimal::ZERO)
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
        .place_order("BTCUSDT", &intent, dec!(100), dec!(100), Decimal::ZERO)
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
    assert!(requests.iter().any(|request| {
        request.starts_with("POST /api/v3/order") && request.contains("quantity=1")
    }));
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
        .place_order("BTCUSDT", &intent, dec!(10_000), dec!(10_000), dec!(0))
        .await;
    assert!(matches!(
        res,
        Err(GatewayError::RiskViolation(
            RiskError::OrderSizeExceeded { .. }
        ))
    ));

    // 2. Circuit breaker blocks order at 5% session drawdown.
    let res_circuit = gateway
        .place_order("BTCUSDT", &intent, dec!(950), dec!(1_000), dec!(0))
        .await;
    assert!(matches!(
        res_circuit,
        Err(GatewayError::RiskViolation(
            RiskError::CircuitBreakerTriggered { .. }
        ))
    ));
}

#[test]
fn spot_lot_rules_are_read_from_exchange_filters() {
    let item: serde_json::Value = serde_json::from_str(
        r#"{"filters":[{"filterType":"LOT_SIZE","minQty":"1","stepSize":"1"},{"filterType":"NOTIONAL","minNotional":"5.0"}]}"#,
    )
    .unwrap();

    assert_eq!(
        parse_spot_lot_rules(&item),
        Some((dec!(1), dec!(1), dec!(5.0)))
    );
    assert_eq!(parse_spot_lot_rules(&serde_json::json!({})), None);
}

#[test]
fn spot_sizing_policy_uses_symbol_filters_and_no_leverage() {
    let gateway = BinanceGateway::new(BinanceGatewayConfig::default()).unwrap();
    let info = SpotSymbolInfo {
        base_asset: "DOGE".to_string(),
        quote_asset: "USDT".to_string(),
        lot_rules: Some((dec!(1), dec!(1), dec!(5))),
    };

    let policy = gateway.exchange_sized_policy(None, Some(&info)).unwrap();

    assert_eq!(policy.max_leverage, Decimal::ONE);
    assert_eq!(policy.quantity_step_size, dec!(1));
    assert_eq!(policy.min_order_quantity, dec!(1));
}
