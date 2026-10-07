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
