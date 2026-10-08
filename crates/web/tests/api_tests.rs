use std::fs;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use backtest::PaperTradingEvent;
use http_body_util::BodyExt;
use rust_decimal_macros::dec;
use tokio::sync::broadcast;
use tower::ServiceExt;
use web::{create_router, create_router_with_security, AppState, DashboardSecurity};

fn security_test_router() -> axum::Router {
    let (tx, _) = broadcast::channel(100);
    create_router(AppState::new(tx, "unused-reports".into()), None)
}

fn websocket_request(origins: &[&str], host: &str) -> Request<Body> {
    let mut request = Request::builder()
        .uri("/ws/telemetry")
        .header(header::HOST, host)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==");
    for origin in origins {
        request = request.header(header::ORIGIN, *origin);
    }
    request.body(Body::empty()).unwrap()
}

async fn assert_websocket_forbidden(origins: &[&str], host: &str) {
    let response = security_test_router()
        .oneshot(websocket_request(origins, host))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origins:?}");
}

#[tokio::test]
async fn websocket_rejects_untrusted_origin_even_with_matching_host() {
    assert_websocket_forbidden(&["https://attacker.example"], "attacker.example").await;
}

#[tokio::test]
async fn websocket_rejects_missing_origin() {
    assert_websocket_forbidden(&[], "localhost:3000").await;
}

#[tokio::test]
async fn websocket_rejects_null_origin() {
    assert_websocket_forbidden(&["null"], "localhost:3000").await;
}

#[tokio::test]
async fn websocket_rejects_malformed_and_duplicate_origins() {
    for origin in [
        "not-an-origin",
        "http://localhost:3000/",
        "http://localhost:3000/path",
        "http://localhost:3000?query",
        "http://localhost:3000#fragment",
        "http://user@localhost:3000",
        "http://localhost:3000 https://attacker.example",
        "http://localhost:65536",
        "http://localhost:",
        "http://localhost:abc",
        "http://localhost:+3000",
        "http://localhost:3000, http://localhost:3000",
    ] {
        assert_websocket_forbidden(&[origin], "localhost:3000").await;
    }
    assert_websocket_forbidden(
        &["http://localhost:3000", "http://localhost:3000"],
        "localhost:3000",
    )
    .await;
}

#[tokio::test]
async fn websocket_rejects_non_utf8_origin() {
    let mut request = websocket_request(&[], "localhost:3000");
    request.headers_mut().insert(
        header::ORIGIN,
        axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
    );
    let response = security_test_router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn websocket_rejects_wrong_scheme_and_port() {
    for origin in [
        "https://localhost:3000",
        "http://localhost:3001",
        "http://localhost",
        "ws://localhost:3000",
        "http://127.0.0.2:3000",
    ] {
        assert_websocket_forbidden(&[origin], "localhost:3000").await;
    }
}

#[tokio::test]
async fn websocket_rejects_spoofed_host_with_trusted_origin() {
    assert_websocket_forbidden(&["http://localhost:3000"], "attacker.example").await;
}

#[tokio::test]
async fn trusted_local_origins_reach_websocket_upgrade_extractor() {
    for origin in [
        "http://localhost:3000",
        "http://127.0.0.1:3000",
        "http://[::1]:3000",
    ] {
        let response = security_test_router()
            .oneshot(websocket_request(&[origin], "localhost:3000"))
            .await
            .unwrap();
        // Tower oneshot has no Hyper OnUpgrade extension. Reaching this rejection
        // proves the perimeter allowed the request, not that a socket was opened.
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    }
}

#[tokio::test]
async fn rest_rejects_untrusted_origin_and_rebinding_host() {
    for (host, origin) in [
        ("localhost:3000", Some("https://attacker.example")),
        ("attacker.example:3000", None),
    ] {
        let mut request = Request::builder()
            .uri("/api/state")
            .header(header::HOST, host);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = security_test_router()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }
}

#[tokio::test]
async fn cors_allows_only_explicit_origin_and_get_preflight() {
    let router = security_test_router();
    let request = Request::builder()
        .method("OPTIONS")
        .uri("/api/state")
        .header(header::HOST, "localhost:3000")
        .header(header::ORIGIN, "http://localhost:3000")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://localhost:3000"
    );
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_METHODS],
        "GET"
    );
    assert!(!response
        .headers()
        .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
}

#[tokio::test]
async fn explicit_tunnel_origins_use_exact_scheme_and_port() {
    let (tx, _) = broadcast::channel(100);
    let policy = DashboardSecurity::new(
        "127.0.0.1:3001".parse().unwrap(),
        &[
            "https://dashboard.example:443".into(),
            "https://dashboard.example:8443".into(),
        ],
    )
    .unwrap();
    let router =
        create_router_with_security(AppState::new(tx, "unused-reports".into()), None, policy);
    for (origin, host, expected) in [
        (
            "https://dashboard.example",
            "dashboard.example",
            StatusCode::UPGRADE_REQUIRED,
        ),
        (
            "https://dashboard.example:443",
            "dashboard.example:443",
            StatusCode::UPGRADE_REQUIRED,
        ),
        (
            "https://dashboard.example:8443",
            "dashboard.example:8443",
            StatusCode::UPGRADE_REQUIRED,
        ),
        (
            "http://localhost:3001",
            "localhost:3001",
            StatusCode::UPGRADE_REQUIRED,
        ),
        (
            "http://localhost:3000",
            "localhost:3001",
            StatusCode::FORBIDDEN,
        ),
        (
            "http://dashboard.example",
            "dashboard.example",
            StatusCode::FORBIDDEN,
        ),
        (
            "https://dashboard.example:8444",
            "dashboard.example",
            StatusCode::FORBIDDEN,
        ),
        (
            "https://sub.dashboard.example",
            "dashboard.example",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(websocket_request(&[origin], host))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{origin} / {host}");
        if expected == StatusCode::FORBIDDEN {
            assert!(!response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        } else {
            assert_eq!(
                response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
                origin
            );
        }
    }
}

#[tokio::test]
async fn rejected_preflight_has_no_cors_permission() {
    for origin in ["https://attacker.example", "null", "http://localhost:3001"] {
        let request = Request::builder()
            .method("OPTIONS")
            .uri("/api/state")
            .header(header::HOST, "localhost:3000")
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .body(Body::empty())
            .unwrap();
        let response = security_test_router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }
}

#[tokio::test]
async fn native_http_requires_trusted_authority_but_not_origin() {
    let request = Request::builder()
        .uri("/api/health")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let response = security_test_router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response
        .headers()
        .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));

    for uri in [
        "/api/health",
        "/index.html",
        "http://attacker.example:3000/api/health",
    ] {
        let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = security_test_router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let request = Request::builder()
        .uri("http://localhost:3000/api/health")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        security_test_router()
            .oneshot(request)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn forwarded_headers_and_duplicate_host_cannot_establish_trust() {
    for (uri, host) in [
        ("/api/state", "attacker.example:3000"),
        ("http://attacker.example:3000/api/state", "localhost:3000"),
        ("/index.html", "attacker.example:3000"),
    ] {
        let request = Request::builder()
            .uri(uri)
            .header(header::HOST, host)
            .header("x-forwarded-host", "localhost:3000")
            .header("x-forwarded-proto", "http")
            .header("forwarded", "host=localhost:3000;proto=http")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            security_test_router()
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let request = Request::builder()
        .uri("/api/state")
        .header(header::HOST, "localhost:3000")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        security_test_router()
            .oneshot(request)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn test_health_endpoint() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx, temp_dir.path().to_path_buf());
    let router = create_router(app_state, None);

    let request = Request::builder()
        .uri("/api/health")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert!(json["uptime_secs"].is_number());
    assert!(json["version"].is_string());
}

#[tokio::test]
async fn test_state_and_strategies_endpoints() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx, temp_dir.path().to_path_buf());
    let router = create_router(app_state, None);

    // Test /api/state
    let req = Request::builder()
        .uri("/api/state")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["portfolio_value"], "10000.00");
    // Without a configured producer, do not attribute the snapshot to a demo session.
    assert_eq!(json["active_symbol"], "");
    assert_eq!(json["active_strategy"], "");
    assert_eq!(json["timestamp"], serde_json::Value::Null);

    // Test /api/strategies
    let req2 = Request::builder()
        .uri("/api/strategies")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res2 = router.oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);

    let body2 = res2.into_body().collect().await.unwrap().to_bytes();
    let json2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
    assert!(json2.is_array());
    assert_eq!(json2[0]["id"], "dollar_bars_cusum");
    assert_eq!(json2[0]["status"], "ACTIVE");
}

#[tokio::test]
async fn test_reports_endpoints_and_traversal_protection() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let reports_path = temp_dir.path().to_path_buf();

    // Write a dummy report file
    let test_report = serde_json::json!({
        "symbol": "BTCUSDT",
        "initial_capital": "10000",
        "final_equity": "10500",
        "metrics": {
            "net_profit": "500",
            "win_rate": "0.65",
            "sortino_ratio": "2.45",
            "total_trades": 12
        }
    });

    let report_file = reports_path.join("paper_trading_1001.json");
    fs::write(&report_file, test_report.to_string()).unwrap();

    let app_state = AppState::new(tx, reports_path);
    let router = create_router(app_state, None);

    // 1. List reports
    let req = Request::builder()
        .uri("/api/reports")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["filename"], "paper_trading_1001.json");
    assert_eq!(list[0]["report_type"], "paper_trading");
    assert_eq!(list[0]["net_profit"], "500");

    // 2. Get existing report by ID
    let req2 = Request::builder()
        .uri("/api/reports/paper_trading_1001.json")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res2 = router.clone().oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);

    // 3. Get non-existent report
    let req3 = Request::builder()
        .uri("/api/reports/missing_report.json")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res3 = router.clone().oneshot(req3).await.unwrap();
    assert_eq!(res3.status(), StatusCode::NOT_FOUND);

    // 4. Directory traversal attempt
    let req4 = Request::builder()
        .uri("/api/reports/..%2F..%2Fetc%2Fpasswd")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res4 = router.oneshot(req4).await.unwrap();
    assert_eq!(res4.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_event_broadcasting_updates_telemetry_state() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx.clone(), temp_dir.path().to_path_buf());
    let router = create_router(app_state, None);

    // Publish MarkToMarket event
    let event = PaperTradingEvent::MarkToMarket {
        current_price: dec!(65200.50),
        unrealized_pnl: dec!(150.25),
        total_equity: dec!(10150.25),
        drawdown_pct: dec!(0.015),
    };

    tx.send(
        backtest::TelemetryConfig {
            symbol: "ETHUSDT".into(),
            strategy_id: "configured-cusum".into(),
        }
        .envelope(123456, event),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Check /api/state reflects updated price & equity
    let req = Request::builder()
        .uri("/api/state")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["last_price"], "65200.50");
    assert_eq!(json["unrealized_pnl"], "150.25");
    assert_eq!(json["portfolio_value"], "10150.25");
    assert_eq!(json["drawdown_pct"], "0.015");
    assert_eq!(json["active_symbol"], "ETHUSDT");
    assert_eq!(json["active_strategy"], "configured-cusum");
    assert_eq!(json["timestamp"], 123456);
}

// Connect through the real HTTP upgrade and decode unmasked server text frames.
// This uses only existing Tokio dependencies, not a Tower upgrade rejection.
async fn connect_telemetry(
    state: AppState,
) -> (tokio::net::TcpStream, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let security = DashboardSecurity::new(addr, &[]).unwrap();
    let router = create_router_with_security(state, None, security);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    socket.write_all(format!("GET /ws/telemetry HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !response.ends_with(b"\r\n\r\n") {
            response.push(socket.read_u8().await.unwrap());
            assert!(response.len() < 8192);
        }
    })
    .await
    .unwrap();
    assert!(String::from_utf8(response)
        .unwrap()
        .starts_with("HTTP/1.1 101"));
    (socket, server)
}

async fn read_envelope(socket: &mut tokio::net::TcpStream) -> serde_json::Value {
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(6), async {
        assert_eq!(socket.read_u8().await.unwrap(), 0x81, "complete text frame");
        let length = socket.read_u8().await.unwrap();
        assert_eq!(length & 0x80, 0, "server frames are unmasked");
        let length = match length {
            126 => u64::from(socket.read_u16().await.unwrap()),
            127 => socket.read_u64().await.unwrap(),
            n => u64::from(n),
        };
        assert!(length < 65536);
        let mut bytes = vec![0; length as usize];
        socket.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn websocket_delivers_configured_paper_event_and_snapshot_envelopes() {
    use backtest::{PaperTradingConfig, PaperTradingSession, TelemetryConfig};
    use domain::{Side, Trade};
    let metadata = TelemetryConfig {
        symbol: "ETHUSDT".into(),
        strategy_id: "paper-17".into(),
    };
    let mut session = PaperTradingSession::new_with_telemetry(
        PaperTradingConfig {
            dollar_bar_threshold: dec!(100),
            strategy: strategies::DollarBarsCusumConfig {
                rolling_window_len: 4,
                cusum_vol_multiplier: dec!(1),
                z_entry_threshold: dec!(1.5),
                z_stop_threshold: dec!(3),
                time_barrier_bars: 10,
            },
            ..Default::default()
        },
        metadata.clone(),
    )
    .unwrap();
    let mut events = session.subscribe_telemetry().unwrap();
    let (tx, _) = broadcast::channel(100);
    let state = AppState::with_telemetry(tx.clone(), "unused-reports".into(), metadata);
    let (mut socket, server) = connect_telemetry(state.clone()).await;
    let initial = read_envelope(&mut socket).await;
    assert_eq!(initial.as_object().unwrap().len(), 5);
    assert_eq!(initial["timestamp"], serde_json::Value::Null);
    assert_eq!(initial["strategy_id"], "paper-17");
    assert_eq!(initial["symbol"], "ETHUSDT");
    assert_eq!(initial["event_type"], "InitialSnapshot");
    assert_eq!(initial["payload"]["active_symbol"], "ETHUSDT");

    let mut kinds = std::collections::BTreeSet::new();
    for (timestamp, price, quantity) in [
        (1000, dec!(100), dec!(1)),
        (2000, dec!(101), dec!(1)),
        (3000, dec!(99), dec!(1.1)),
        (4000, dec!(100), dec!(1)),
        (5000, dec!(90), dec!(1.5)),
        (5500, dec!(91), dec!(0.1)),
        (6000, dec!(101), dec!(1)),
    ] {
        let raw =
            session.process_trade(&Trade::new(timestamp, price, quantity, Side::Buy).unwrap());
        for payload in raw {
            let event = events.try_recv().unwrap();
            let expected = serde_json::to_value(&event).unwrap();
            kinds.insert(event.event_type.clone());
            tx.send(event).unwrap();
            let delivered = read_envelope(&mut socket).await;
            assert_eq!(delivered, expected);
            assert_eq!(delivered.as_object().unwrap().len(), 5);
            assert_eq!(delivered["timestamp"], timestamp);
            assert_eq!(delivered["symbol"], "ETHUSDT");
            assert_eq!(delivered["strategy_id"], "paper-17");
            assert_eq!(delivered["event_type"], payload.event_type());
            assert_eq!(delivered["payload"], serde_json::to_value(payload).unwrap());
        }
    }
    assert_eq!(
        kinds,
        [
            "BarFormed",
            "SignalGenerated",
            "PositionOpened",
            "MarkToMarket",
            "PositionClosed"
        ]
        .map(String::from)
        .into_iter()
        .collect()
    );

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            {
                let snapshot = state.state.read().await;
                if snapshot.timestamp == Some(6000) && snapshot.active_position.is_none() {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (mut second, second_server) = connect_telemetry(state).await;
    let snapshot = read_envelope(&mut second).await;
    assert_eq!(snapshot["timestamp"], 6000);
    assert_eq!(snapshot["symbol"], "ETHUSDT");
    assert_eq!(snapshot["strategy_id"], "paper-17");
    assert_eq!(snapshot["payload"]["last_price"], "101");
    server.abort();
    second_server.abort();
}

#[tokio::test]
async fn websocket_delivers_mock_metadata_and_observed_tick_timestamp() {
    use backtest::TelemetryConfig;
    let metadata = TelemetryConfig {
        symbol: "SOLUSDT".into(),
        strategy_id: "demo-9".into(),
    };
    let (tx, _) = broadcast::channel(100);
    let state = AppState::with_telemetry(tx.clone(), "unused-reports".into(), metadata.clone());
    let (mut socket, server) = connect_telemetry(state).await;
    read_envelope(&mut socket).await;
    let ticker = web::mock::spawn_mock_ticker_with_config(tx, metadata);
    let event = read_envelope(&mut socket).await;
    assert_eq!(event.as_object().unwrap().len(), 5);
    assert_eq!(event["symbol"], "SOLUSDT");
    assert_eq!(event["strategy_id"], "demo-9");
    assert_eq!(event["event_type"], "BarFormed");
    assert!(event["timestamp"].as_i64().unwrap() > 0);
    assert_eq!(
        event["timestamp"],
        event["payload"]["BarFormed"]["end_time"]
    );
    ticker.abort();
    server.abort();
}

#[tokio::test]
async fn mock_startup_identity_is_configured_before_producer_events() {
    // Guard the binary wiring without spawning a process or a timed demo producer.
    let startup = include_str!("../src/main.rs");
    let state_creation = startup.find("let app_state =").unwrap();
    let producer_start = startup.find("mock::spawn_mock_ticker").unwrap();
    assert!(
        state_creation < producer_start,
        "state must precede producer startup"
    );
    assert!(startup.contains("AppState::with_telemetry("));
    assert!(startup.contains("mock::demo_telemetry_config()"));

    let (tx, _) = broadcast::channel(2);
    let metadata = web::mock::demo_telemetry_config();
    let state = AppState::with_telemetry(tx.clone(), "unused-reports".into(), metadata.clone());
    {
        let snapshot = state.state.read().await;
        assert_eq!(snapshot.active_symbol, "BTCUSDT");
        assert_eq!(snapshot.active_strategy, "SyntheticDemo");
        assert_eq!(snapshot.timestamp, None);
    }
    tokio::task::yield_now().await;
    assert_eq!(state.state.read().await.timestamp, None);
    tx.send(metadata.envelope(
        1234,
        PaperTradingEvent::MarkToMarket {
            current_price: dec!(100),
            unrealized_pnl: dec!(0),
            total_equity: dec!(10000),
            drawdown_pct: dec!(0),
        },
    ))
    .unwrap();
    wait_snapshot_time(&state, 1234).await;
    let snapshot = state.state.read().await;
    assert_eq!(snapshot.active_symbol, "BTCUSDT");
    assert_eq!(snapshot.active_strategy, "SyntheticDemo");
    assert_eq!(snapshot.timestamp, Some(1234));
}

async fn wait_snapshot_time(state: &AppState, timestamp: i64) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state.state.read().await.timestamp == Some(timestamp) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn snapshot_individually_lagging_client_is_uncertain() {
    let (tx, _) = broadcast::channel(2);
    let metadata = backtest::TelemetryConfig {
        symbol: "SOLUSDT".into(),
        strategy_id: "demo-9".into(),
    };
    let state = AppState::with_telemetry(tx.clone(), "unused-reports".into(), metadata.clone());
    let mut client = tx.subscribe();
    for timestamp in 1..=4 {
        tx.send(metadata.envelope(
            timestamp,
            PaperTradingEvent::MarkToMarket {
                current_price: dec!(105),
                unrealized_pnl: dec!(10),
                total_equity: dec!(10010),
                drawdown_pct: dec!(0.02),
            },
        ))
        .unwrap();
        wait_snapshot_time(&state, timestamp).await;
    }
    let message = web::ws::receive_client_message(&mut client, &state)
        .await
        .unwrap();
    assert_eq!(message["event_type"], "InitialSnapshot");
    assert_eq!(message["payload"]["stale"], true);
    assert_eq!(message["timestamp"], 4);
    assert!(!state.state.read().await.stale);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            web::ws::receive_client_message(&mut client, &state),
        )
        .await
        .is_err(),
        "pre-snapshot backlog must not be forwarded"
    );
    let (mut socket, server) = connect_telemetry(state).await;
    let reconnect = read_envelope(&mut socket).await;
    assert_eq!(reconnect["timestamp"], 4);
    assert_eq!(reconnect["payload"]["last_price"], "105");
    server.abort();
}

#[tokio::test]
async fn lag_shared_aggregator_notifies_idle_connected_client() {
    let (tx, _) = broadcast::channel(2);
    let metadata = web::mock::demo_telemetry_config();
    let state = AppState::with_telemetry(tx.clone(), "unused-reports".into(), metadata.clone());
    let (mut socket, server) = connect_telemetry(state.clone()).await;
    assert_eq!(read_envelope(&mut socket).await["payload"]["stale"], false);
    // Block only the projection updater, while the client drains every event.
    let guard = state.state.write().await;
    for timestamp in 1..=5 {
        tx.send(metadata.envelope(
            timestamp,
            PaperTradingEvent::MarkToMarket {
                current_price: dec!(105),
                unrealized_pnl: dec!(10),
                total_equity: dec!(10010),
                drawdown_pct: dec!(0.02),
            },
        ))
        .unwrap();
        assert_eq!(read_envelope(&mut socket).await["timestamp"], timestamp);
    }
    drop(guard);
    // No further producer event: stale must independently wake the socket.
    let notice = read_envelope(&mut socket).await;
    assert_eq!(notice["event_type"], "InitialSnapshot");
    assert_eq!(notice["payload"]["stale"], true);
    let (mut reconnect, second_server) = connect_telemetry(state).await;
    assert_eq!(
        read_envelope(&mut reconnect).await["payload"]["stale"],
        true
    );
    server.abort();
    second_server.abort();
}

#[tokio::test]
async fn snapshot_retains_uncertainty_and_consumes_after_aggregator_gap() {
    let (tx, _) = broadcast::channel(2);
    let metadata = backtest::TelemetryConfig {
        symbol: "SOLUSDT".into(),
        strategy_id: "demo-9".into(),
    };
    let state = AppState::with_telemetry(tx.clone(), "unused-reports".into(), metadata.clone());
    let initial = serde_json::to_value(&*state.state.read().await).unwrap();
    assert_eq!(initial["timestamp"], serde_json::Value::Null);
    assert_eq!(initial["active_symbol"], "SOLUSDT");
    tx.send(metadata.envelope(
        1,
        PaperTradingEvent::PositionOpened {
            side: domain::PositionSide::Long,
            entry_price: dec!(100),
            quantity: dec!(2),
            stop_loss: dec!(90),
            take_profit: dec!(120),
        },
    ))
    .unwrap();
    wait_snapshot_time(&state, 1).await;
    // No await: on this single-thread runtime the updater must miss events.
    for timestamp in 2..=5 {
        tx.send(metadata.envelope(
            timestamp,
            PaperTradingEvent::MarkToMarket {
                current_price: dec!(105),
                unrealized_pnl: dec!(10),
                total_equity: dec!(10010),
                drawdown_pct: dec!(0.02),
            },
        ))
        .unwrap();
    }
    tokio::task::yield_now().await;
    let snapshot = serde_json::to_value(&*state.state.read().await).unwrap();
    assert_eq!(snapshot["stale"], true);
    wait_snapshot_time(&state, 5).await;
    let (mut socket, server) = connect_telemetry(state.clone()).await;
    let snapshot = read_envelope(&mut socket).await;
    let p = &snapshot["payload"];
    assert_eq!(p["stale"], true);
    assert_eq!(p["portfolio_value"], "10010");
    assert_eq!(p["cash_balance"], "10000.00");
    assert_eq!(p["unrealized_pnl"], "10");
    assert_eq!(p["drawdown_pct"], "0.02");
    assert_eq!(p["last_price"], "105");
    assert_eq!(
        p["active_position"],
        serde_json::json!({
            "side": "Long", "entry_price": "100", "quantity": "2",
            "stop_loss": "90", "take_profit": "120"
        })
    );
    tx.send(metadata.envelope(
        6,
        PaperTradingEvent::PositionClosed {
            exit_reason: "TakeProfit".into(),
            exit_price: dec!(120),
            net_pnl: dec!(40),
            total_equity: dec!(10040),
        },
    ))
    .unwrap();
    wait_snapshot_time(&state, 6).await;
    let router = create_router(state, None);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/state")
                .header(header::HOST, "localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let flat: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(flat["active_position"], serde_json::Value::Null);
    assert_eq!(flat["unrealized_pnl"], "0");
    assert_eq!(flat["cash_balance"], "10040");
    assert_eq!(flat["stale"], true);
    server.abort();
}

#[tokio::test]
async fn test_static_asset_serving() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx, temp_dir.path().to_path_buf());
    let router = create_router(app_state, Some("crates/web/static".into()));

    let req = Request::builder()
        .uri("/index.html")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let req_css = Request::builder()
        .uri("/styles.css")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res_css = router.oneshot(req_css).await.unwrap();
    assert_eq!(res_css.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_contexts_endpoint() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx, temp_dir.path().to_path_buf());
    let router = create_router(app_state, None);

    let req = Request::builder()
        .uri("/api/contexts")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 2);

    let spot = &json[0];
    assert_eq!(spot["id"], "spot");
    assert_eq!(spot["margin_mode"], "cash");
    assert_eq!(spot["position_mode"], "cash");
    assert_eq!(spot["is_supported"], true);
    assert_eq!(spot["sole_order_writer"], false);

    let futures = &json[1];
    assert_eq!(futures["id"], "usdm_futures");
    assert_eq!(futures["margin_mode"], "isolated");
    assert_eq!(futures["position_mode"], "one_way");
    assert_eq!(futures["is_supported"], true);
    assert_eq!(futures["sole_order_writer"], true);
}

#[tokio::test]
async fn test_enhanced_strategy_metadata_contract() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx, temp_dir.path().to_path_buf());
    let router = create_router(app_state, None);

    let req = Request::builder()
        .uri("/api/strategies")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json[0]["id"], "dollar_bars_cusum");
    assert_eq!(json[0]["version"], "1.0.0");
    assert_eq!(
        json[0]["compatible_markets"],
        serde_json::json!(["usdm_futures"])
    );
    assert_eq!(json[0]["is_futures_only"], true);
}

#[tokio::test]
async fn test_read_only_negative_mutation_routes() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx, temp_dir.path().to_path_buf());
    let router = create_router(app_state, None);

    // POST /api/state must be rejected
    let req_post_state = Request::builder()
        .method("POST")
        .uri("/api/state")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res_post_state = router.clone().oneshot(req_post_state).await.unwrap();
    assert_eq!(res_post_state.status(), StatusCode::METHOD_NOT_ALLOWED);

    // POST /api/orders (non-existent order mutation endpoint) must be rejected
    let req_post_orders = Request::builder()
        .method("POST")
        .uri("/api/orders")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res_post_orders = router.clone().oneshot(req_post_orders).await.unwrap();
    assert!(
        res_post_orders.status() == StatusCode::NOT_FOUND
            || res_post_orders.status() == StatusCode::METHOD_NOT_ALLOWED
    );

    // DELETE /api/positions must be rejected
    let req_del = Request::builder()
        .method("DELETE")
        .uri("/api/positions")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res_del = router.oneshot(req_del).await.unwrap();
    assert!(
        res_del.status() == StatusCode::NOT_FOUND
            || res_del.status() == StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn test_monte_carlo_fan_chart_normalization() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let reports_path = temp_dir.path().to_path_buf();

    // Create report with fan_chart_trajectories
    let rep1_path = reports_path.join("mc_trajectories.json");
    fs::write(
        &rep1_path,
        r#"{"symbol":"BTCUSDT","fan_chart_trajectories":[[10000,10500],[10000,9800]]}"#,
    )
    .unwrap();

    // Create report with fan_chart_curves
    let rep2_path = reports_path.join("mc_curves.json");
    fs::write(
        &rep2_path,
        r#"{"symbol":"BTCUSDT","fan_chart_curves":[[10000,10200]]}"#,
    )
    .unwrap();

    // Create a corrupted/malformed JSON file
    let corrupt_path = reports_path.join("corrupt.json");
    fs::write(&corrupt_path, r#"{"invalid_json": true"#).unwrap();

    let app_state = AppState::new(tx, reports_path);
    let router = create_router(app_state, None);

    // Test list reports skips corrupt file and lists valid ones
    let req_list = Request::builder()
        .uri("/api/reports")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res_list = router.clone().oneshot(req_list).await.unwrap();
    assert_eq!(res_list.status(), StatusCode::OK);
    let list_body = res_list.into_body().collect().await.unwrap().to_bytes();
    let list_json: serde_json::Value = serde_json::from_slice(&list_body).unwrap();
    assert_eq!(list_json.as_array().unwrap().len(), 2);

    // Test mc_trajectories normalizes fan_chart_curves
    let req_get1 = Request::builder()
        .uri("/api/reports/mc_trajectories")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res_get1 = router.clone().oneshot(req_get1).await.unwrap();
    assert_eq!(res_get1.status(), StatusCode::OK);
    let body1 = res_get1.into_body().collect().await.unwrap().to_bytes();
    let json1: serde_json::Value = serde_json::from_slice(&body1).unwrap();
    assert!(json1.get("fan_chart_trajectories").is_some());
    assert!(json1.get("fan_chart_curves").is_some());

    // Test mc_curves normalizes fan_chart_trajectories
    let req_get2 = Request::builder()
        .uri("/api/reports/mc_curves")
        .header(header::HOST, "localhost:3000")
        .body(Body::empty())
        .unwrap();
    let res_get2 = router.oneshot(req_get2).await.unwrap();
    assert_eq!(res_get2.status(), StatusCode::OK);
    let body2 = res_get2.into_body().collect().await.unwrap().to_bytes();
    let json2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
    assert!(json2.get("fan_chart_curves").is_some());
    assert!(json2.get("fan_chart_trajectories").is_some());
}
