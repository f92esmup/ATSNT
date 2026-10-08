use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn read_http_headers(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = socket
            .read(&mut chunk)
            .await
            .expect("local HTTP request should be readable");
        assert_ne!(count, 0, "local HTTP request should contain headers");
        bytes.extend_from_slice(&chunk[..count]);
    }
    String::from_utf8(bytes).expect("HTTP headers are UTF-8")
}

async fn respond_ok(socket: &mut tokio::net::TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(response.as_bytes())
        .await
        .expect("local HTTP response should be writable");
}

#[tokio::test]
async fn spot_listen_key_creation_is_refused_without_network_access() {
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        api_key: "test-api-key".into(),
        secret_key: "test-secret".into(),
        spot: true,
        ..Default::default()
    })
    .expect("test gateway should build");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local test listener should bind");
    let address = listener.local_addr().expect("listener has local address");
    gateway.base_url = format!("http://{address}");

    assert!(gateway.create_listen_key().await.is_err());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_futures_renewal_does_not_expose_the_listen_key_in_errors() {
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        api_key: "test-api-key".into(),
        secret_key: "test-secret".into(),
        spot: false,
        ..Default::default()
    })
    .expect("test gateway should build");
    gateway.base_url = "http://127.0.0.1:9".into();

    let error = gateway
        .keep_alive_listen_key("private-listen-key")
        .await
        .expect_err("unavailable local gateway must fail");
    assert!(!error.to_string().contains("private-listen-key"));
}

#[tokio::test]
async fn futures_listen_key_creation_validates_and_returns_the_key_from_local_http() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local test listener should bind");
    let address = listener.local_addr().expect("listener has local address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("gateway request connects");
        let request = read_http_headers(&mut socket).await;
        respond_ok(&mut socket, r#"{"listenKey":"synthetic-key"}"#).await;
        request
    });
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        api_key: "test-api-key".into(),
        secret_key: "test-secret".into(),
        spot: false,
        ..Default::default()
    })
    .expect("test gateway should build");
    gateway.base_url = format!("http://{address}");

    assert_eq!(
        gateway
            .create_listen_key()
            .await
            .expect("valid local Futures response should parse"),
        "synthetic-key"
    );
    let request = server.await.expect("local server task joins");
    assert!(request.starts_with("POST /fapi/v1/listenKey HTTP/1.1"));
    assert!(request
        .to_ascii_lowercase()
        .contains("x-mbx-apikey: test-api-key"));
}

#[tokio::test]
async fn futures_listen_key_creation_rejects_a_missing_key_field() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local test listener should bind");
    let address = listener.local_addr().expect("listener has local address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("gateway request connects");
        let _ = read_http_headers(&mut socket).await;
        respond_ok(&mut socket, "{}").await;
    });
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        api_key: "test-api-key".into(),
        secret_key: "test-secret".into(),
        spot: false,
        ..Default::default()
    })
    .expect("test gateway should build");
    gateway.base_url = format!("http://{address}");

    assert!(gateway.create_listen_key().await.is_err());
    server.await.expect("local server task joins");
}

#[tokio::test]
async fn futures_renewal_rejects_malformed_success_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local test listener should bind");
    let address = listener.local_addr().expect("listener has local address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("gateway request connects");
        let _ = read_http_headers(&mut socket).await;
        respond_ok(&mut socket, "not-json").await;
    });
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        api_key: "test-api-key".into(),
        secret_key: "test-secret".into(),
        spot: false,
        ..Default::default()
    })
    .expect("test gateway should build");
    gateway.base_url = format!("http://{address}");

    let error = gateway
        .keep_alive_listen_key("synthetic-key")
        .await
        .expect_err("malformed success body must fail renewal");
    assert!(!error.to_string().contains("not-json"));
    server.await.expect("local server task joins");
}

#[tokio::test]
async fn futures_renewal_rejects_exchange_error_body_even_with_success_status() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local test listener should bind");
    let address = listener.local_addr().expect("listener has local address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("gateway request connects");
        let _ = read_http_headers(&mut socket).await;
        respond_ok(
            &mut socket,
            r#"{"code":-1125,"msg":"listenKey does not exist"}"#,
        )
        .await;
    });
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        api_key: "test-api-key".into(),
        secret_key: "test-secret".into(),
        spot: false,
        ..Default::default()
    })
    .expect("test gateway should build");
    gateway.base_url = format!("http://{address}");

    assert!(gateway
        .keep_alive_listen_key("synthetic-key")
        .await
        .is_err());
    server.await.expect("local server task joins");
}

#[tokio::test]
async fn futures_renewal_and_shutdown_use_bounded_local_http_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local test listener should bind");
    let address = listener.local_addr().expect("listener has local address");
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.expect("gateway request connects");
            requests.push(read_http_headers(&mut socket).await);
            respond_ok(&mut socket, "{}").await;
        }
        requests
    });
    let mut gateway = BinanceGateway::new(BinanceGatewayConfig {
        api_key: "test-api-key".into(),
        secret_key: "test-secret".into(),
        spot: false,
        ..Default::default()
    })
    .expect("test gateway should build");
    gateway.base_url = format!("http://{address}");

    gateway
        .keep_alive_listen_key("synthetic-key")
        .await
        .expect("local listen-key renewal succeeds");
    gateway
        .close_listen_key("synthetic-key")
        .await
        .expect("local listen-key cleanup succeeds");
    let requests = server.await.expect("local server task joins");

    assert!(requests[0].starts_with("PUT /fapi/v1/listenKey?listenKey=synthetic-key HTTP/1.1"));
    assert!(requests[1].starts_with("DELETE /fapi/v1/listenKey?listenKey=synthetic-key HTTP/1.1"));
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("x-mbx-apikey: test-api-key")));
}
