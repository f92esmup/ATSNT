//! Recent aggregated trades from the public Binance USD-M REST API.
//!
//! Used to warm a strategy's rolling windows before the live stream starts. The endpoint
//! serves at most one hour per query and 1000 trades per page, so a window is fetched hour by
//! hour and page by page with `fromId`.

use domain::{Side, Trade};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

use crate::error::AdapterError;

const PAGE_LIMIT: usize = 1_000;
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_ATTEMPTS: u32 = 4;
const WINDOW_MS: i64 = 60 * 60 * 1_000;
const TESTNET_BASE_URL: &str = "https://testnet.binancefuture.com";
const PRODUCTION_BASE_URL: &str = "https://fapi.binance.com";

#[derive(Debug, Deserialize)]
struct AggTradeRecord {
    #[serde(rename = "a")]
    id: i64,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    quantity: String,
    #[serde(rename = "T")]
    time_ms: i64,
    /// `true` when the buyer is the maker, i.e. the aggressor sold.
    #[serde(rename = "m")]
    buyer_is_maker: bool,
}

impl AggTradeRecord {
    fn into_trade(self) -> Result<Trade, AdapterError> {
        let parse = |raw: &str| {
            Decimal::from_str(raw).map_err(|error| {
                AdapterError::General(format!("invalid trade value {raw}: {error}"))
            })
        };
        let side = if self.buyer_is_maker {
            Side::Sell
        } else {
            Side::Buy
        };
        Ok(Trade::new(
            self.time_ms,
            parse(&self.price)?,
            parse(&self.quantity)?,
            side,
        )?)
    }
}

/// Public REST client for historical aggregated trades of one futures market.
pub struct BinanceRecentTrades {
    base_url: String,
    client: reqwest::Client,
}

impl BinanceRecentTrades {
    /// The futures market the live stream of the same environment trades.
    pub fn futures(testnet: bool) -> Self {
        Self::with_base_url(if testnet {
            TESTNET_BASE_URL
        } else {
            PRODUCTION_BASE_URL
        })
    }

    /// A client against an explicit endpoint (a mock server in tests).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Endpoint this client queries.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// One page, retried with a growing pause on transport errors and 5xx. Any other HTTP
    /// error is final, rate limits (429, 418) included: retrying inside a rate-limit window only
    /// adds requests and can turn it into an IP ban that would hit order traffic too.
    async fn get_page(&self, url: &str) -> Result<Vec<AggTradeRecord>, AdapterError> {
        let mut attempt = 1;
        loop {
            let outcome = async {
                self.client
                    .get(url)
                    // Set per request too, so the bound survives a client built without it.
                    .timeout(REQUEST_TIMEOUT)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Vec<AggTradeRecord>>()
                    .await
            }
            .await;
            match outcome {
                Ok(page) => return Ok(page),
                Err(error) => {
                    let transient = error.is_timeout()
                        || error.is_connect()
                        || error.is_request()
                        || error.is_body()
                        || error
                            .status()
                            .is_some_and(|status| status.is_server_error());
                    if !transient || attempt >= MAX_ATTEMPTS {
                        return Err(error.into());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(250 * u64::from(attempt)))
                        .await;
                    attempt += 1;
                }
            }
        }
    }

    /// Trades of `symbol` in `[from_ms, to_ms]`, oldest first.
    pub async fn fetch(
        &self,
        symbol: &str,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<Trade>, AdapterError> {
        let symbol = symbol.to_ascii_uppercase();
        let mut trades = Vec::new();
        let mut window_start = from_ms;
        while window_start <= to_ms {
            let window_end = (window_start + WINDOW_MS - 1).min(to_ms);
            let mut next_id: Option<i64> = None;
            loop {
                let query = match next_id {
                    Some(id) => format!("symbol={symbol}&fromId={id}&limit={PAGE_LIMIT}"),
                    None => format!(
                        "symbol={symbol}&startTime={window_start}&endTime={window_end}&limit={PAGE_LIMIT}"
                    ),
                };
                let url = format!("{}/fapi/v1/aggTrades?{query}", self.base_url);
                let page = self.get_page(&url).await?;
                let full = page.len() >= PAGE_LIMIT;
                let last_id = page.last().map(|record| record.id);
                let mut past_window = false;
                for record in page {
                    if record.time_ms > window_end {
                        past_window = true;
                        break;
                    }
                    trades.push(record.into_trade()?);
                }
                match (full && !past_window, last_id) {
                    (true, Some(id)) => next_id = Some(id + 1),
                    _ => break,
                }
            }
            window_start = window_end + 1;
        }
        Ok(trades)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Serves the given response bodies with status 200, one per request, and records the
    /// request lines.
    fn spawn_server(
        bodies: Vec<String>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        spawn_server_with_status(bodies.into_iter().map(|body| (200, body)).collect())
    }

    /// Like `spawn_server`, with the HTTP status of each response.
    fn spawn_server_with_status(
        bodies: Vec<(u16, String)>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            let mut queue = bodies.into_iter();
            let mut last = Instant::now();
            while last.elapsed() < Duration::from_millis(900) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                last = Instant::now();
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let mut request = Vec::new();
                let mut chunk = [0_u8; 2048];
                while let Ok(read) = stream.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                captured.lock().unwrap().push(line);
                let (status, body) = queue.next().unwrap_or((200, "[]".to_string()));
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{address}"), requests, handle)
    }

    fn record(id: i64, time_ms: i64, price: &str, buyer_is_maker: bool) -> String {
        format!(
            r#"{{"a":{id},"p":"{price}","q":"0.5","f":{id},"l":{id},"T":{time_ms},"m":{buyer_is_maker}}}"#
        )
    }

    #[test]
    fn the_environment_picks_the_matching_endpoint() {
        assert_eq!(
            BinanceRecentTrades::futures(true).base_url(),
            "https://testnet.binancefuture.com"
        );
        assert_eq!(
            BinanceRecentTrades::futures(false).base_url(),
            "https://fapi.binance.com"
        );
    }

    #[tokio::test]
    async fn trades_are_parsed_with_the_aggressor_side() {
        let body = format!(
            "[{},{}]",
            record(1, 1_000, "100.5", false),
            record(2, 1_100, "100.4", true)
        );
        let (url, requests, handle) = spawn_server(vec![body]);

        let trades = BinanceRecentTrades::with_base_url(url)
            .fetch("btcusdt", 1_000, 2_000)
            .await
            .unwrap();
        handle.join().unwrap();

        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].price, dec!(100.5));
        assert_eq!(trades[0].quantity, dec!(0.5));
        assert_eq!(trades[0].side, Side::Buy);
        assert_eq!(trades[1].side, Side::Sell);
        let request = requests.lock().unwrap()[0].clone();
        assert!(request
            .contains("/fapi/v1/aggTrades?symbol=BTCUSDT&startTime=1000&endTime=2000&limit=1000"));
    }

    #[tokio::test]
    async fn a_full_page_continues_from_the_next_trade_id() {
        let first: Vec<String> = (1..=1000)
            .map(|i| record(i, 1_000 + i, "100", false))
            .collect();
        let second = format!("[{}]", record(1001, 2_500, "101", false));
        let (url, requests, handle) = spawn_server(vec![format!("[{}]", first.join(",")), second]);

        let trades = BinanceRecentTrades::with_base_url(url)
            .fetch("BTCUSDT", 1_000, 5_000)
            .await
            .unwrap();
        handle.join().unwrap();

        assert_eq!(trades.len(), 1001);
        assert_eq!(trades[1000].price, dec!(101));
        let lines = requests.lock().unwrap().clone();
        assert!(lines[1].contains("fromId=1001"));
    }

    #[tokio::test]
    async fn a_range_longer_than_an_hour_is_fetched_in_hourly_windows() {
        let (url, requests, handle) = spawn_server(vec![
            format!("[{}]", record(1, 10, "100", false)),
            format!("[{}]", record(2, 3_600_010, "101", false)),
        ]);

        let trades = BinanceRecentTrades::with_base_url(url)
            .fetch("BTCUSDT", 0, 3_700_000)
            .await
            .unwrap();
        handle.join().unwrap();

        assert_eq!(trades.len(), 2);
        let lines = requests.lock().unwrap().clone();
        assert!(lines[0].contains("startTime=0&endTime=3599999"));
        assert!(lines[1].contains("startTime=3600000&endTime=3700000"));
    }

    #[tokio::test]
    async fn an_http_error_is_reported() {
        // Nothing listens on the port the first listener released.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        let result = BinanceRecentTrades::with_base_url(format!("http://{address}"))
            .fetch("BTCUSDT", 0, 1)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a_failing_server_is_retried_but_a_rate_limit_is_not() {
        let page = format!("[{}]", record(1, 1_000, "100", false));
        let (url, requests, handle) = spawn_server_with_status(vec![
            (503, "{}".to_string()),
            (502, "{}".to_string()),
            (200, page),
        ]);
        let trades = BinanceRecentTrades::with_base_url(url)
            .fetch("BTCUSDT", 1_000, 2_000)
            .await
            .unwrap();
        handle.join().unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(requests.lock().unwrap().len(), 3);

        // A 429 inside a rate-limit window is final: one request, no hammering.
        let (url, requests, handle) = spawn_server_with_status(vec![(429, "{}".to_string())]);
        let result = BinanceRecentTrades::with_base_url(url)
            .fetch("BTCUSDT", 1_000, 2_000)
            .await;
        handle.join().unwrap();
        assert!(result.is_err());
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_client_error_is_final_and_not_retried() {
        let (url, requests, handle) = spawn_server_with_status(vec![(400, "{}".to_string())]);

        let result = BinanceRecentTrades::with_base_url(url)
            .fetch("BTCUSDT", 1_000, 2_000)
            .await;
        handle.join().unwrap();

        assert!(result.is_err());
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
}
