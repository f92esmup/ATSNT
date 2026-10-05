use std::fs;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use backtest::PaperTradingEvent;
use http_body_util::BodyExt;
use rust_decimal_macros::dec;
use tokio::sync::broadcast;
use tower::ServiceExt;
use web::{create_router, AppState};

#[tokio::test]
async fn test_health_endpoint() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx, temp_dir.path().to_path_buf());
    let router = create_router(app_state, None);

    let request = Request::builder()
        .uri("/api/health")
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
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["portfolio_value"], "10000.00");
    assert_eq!(json["active_symbol"], "BTCUSDT");

    // Test /api/strategies
    let req2 = Request::builder()
        .uri("/api/strategies")
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
        .body(Body::empty())
        .unwrap();
    let res2 = router.clone().oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);

    // 3. Get non-existent report
    let req3 = Request::builder()
        .uri("/api/reports/missing_report.json")
        .body(Body::empty())
        .unwrap();
    let res3 = router.clone().oneshot(req3).await.unwrap();
    assert_eq!(res3.status(), StatusCode::NOT_FOUND);

    // 4. Directory traversal attempt
    let req4 = Request::builder()
        .uri("/api/reports/..%2F..%2Fetc%2Fpasswd")
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

    tx.send(event).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Check /api/state reflects updated price & equity
    let req = Request::builder()
        .uri("/api/state")
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
}

#[tokio::test]
async fn test_static_asset_serving() {
    let (tx, _) = broadcast::channel(100);
    let temp_dir = tempfile::tempdir().unwrap();
    let app_state = AppState::new(tx, temp_dir.path().to_path_buf());
    let router = create_router(app_state, Some("crates/web/static".into()));

    let req = Request::builder()
        .uri("/index.html")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let req_css = Request::builder()
        .uri("/styles.css")
        .body(Body::empty())
        .unwrap();
    let res_css = router.oneshot(req_css).await.unwrap();
    assert_eq!(res_css.status(), StatusCode::OK);
}
