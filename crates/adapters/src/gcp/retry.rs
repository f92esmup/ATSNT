//! Bounded retries for transient GCP API failures (server errors, rate limits, timeouts).
//!
//! A BigQuery or GCS write runs after a whole optimisation; failing it on one transient 5xx
//! would discard that work. Client errors (4xx other than 429) are final: retrying a
//! permission or validation error only repeats it.

use std::time::Duration;

use tracing::warn;

/// Environment variable overriding the base delay between attempts, in milliseconds
/// (tests set it to 0).
pub const RETRY_BASE_MS_ENV: &str = "ATSNT_GCP_RETRY_BASE_MS";

/// Largest base delay [`RETRY_BASE_MS_ENV`] may set.
pub const MAX_BASE_DELAY: Duration = Duration::from_secs(60);

/// Longest wait a server's `Retry-After` may impose on one retry.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Parses a [`RETRY_BASE_MS_ENV`] value: whole milliseconds from 0 to [`MAX_BASE_DELAY`].
pub fn base_delay_setting(raw: &str) -> Result<Duration, String> {
    let ms: u64 = raw.trim().parse().map_err(|_| {
        format!("{RETRY_BASE_MS_ENV}={raw:?} is not a whole number of milliseconds")
    })?;
    let delay = Duration::from_millis(ms);
    if delay > MAX_BASE_DELAY {
        return Err(format!(
            "{RETRY_BASE_MS_ENV}={ms} exceeds {} ms",
            MAX_BASE_DELAY.as_millis()
        ));
    }
    Ok(delay)
}

/// How many times a request is sent and how long to wait between attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts, the first one included (at least 1).
    pub attempts: u32,
    /// Wait before the second attempt; it doubles before each later one.
    pub base_delay: Duration,
}

impl Default for RetryPolicy {
    /// Three attempts, waiting 1 s and then 2 s.
    fn default() -> Self {
        Self {
            attempts: 3,
            base_delay: Duration::from_secs(1),
        }
    }
}

impl RetryPolicy {
    /// `attempts` tries with no wait between them (tests).
    pub fn immediate(attempts: u32) -> Self {
        Self {
            attempts,
            base_delay: Duration::ZERO,
        }
    }

    /// The default policy, with the base delay from [`RETRY_BASE_MS_ENV`] when it is set. An
    /// invalid value is reported and the default delay is kept.
    pub fn from_env() -> Self {
        let policy = Self::default();
        let Ok(raw) = std::env::var(RETRY_BASE_MS_ENV) else {
            return policy;
        };
        match base_delay_setting(&raw) {
            Ok(base_delay) => Self {
                base_delay,
                ..policy
            },
            Err(problem) => {
                println!(
                    "# [GCP] {problem}; keeping the default {} ms",
                    policy.base_delay.as_millis()
                );
                warn!(target: "gcp", %problem, "invalid retry delay setting ignored");
                policy
            }
        }
    }
}

/// Whether a response status is worth another attempt.
fn is_transient(status: reqwest::StatusCode) -> bool {
    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// The wait a `Retry-After: <seconds>` header asks for, capped at [`MAX_RETRY_AFTER`].
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let seconds: u64 = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

/// Longest wait between two attempts, whatever the base delay and the attempt count.
const MAX_WAIT: Duration = Duration::from_secs(120);

/// The wait before the attempt after one that waited `delay`: twice as long, at most
/// [`MAX_WAIT`] (so it never reaches a duration the timer cannot represent).
fn next_delay(delay: Duration) -> Duration {
    delay.checked_mul(2).unwrap_or(MAX_WAIT).min(MAX_WAIT)
}

/// Sends the request built by `build` until it gets a non-transient answer or the attempts
/// run out; the last response or transport error is returned as is. Transient means HTTP 5xx,
/// 429 (waiting at least its `Retry-After`), a timeout or a refused connection.
pub(crate) async fn send_with_retry<F>(
    policy: RetryPolicy,
    service: &str,
    build: F,
) -> Result<reqwest::Response, reqwest::Error>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let attempts = policy.attempts.max(1);
    let mut delay = policy.base_delay;
    let mut attempt = 1;
    loop {
        let outcome = build().send().await;
        let (reason, asked) = match &outcome {
            Ok(response) if is_transient(response.status()) => {
                let asked = (response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS)
                    .then(|| retry_after(response))
                    .flatten();
                (format!("HTTP {}", response.status()), asked)
            }
            Err(error) if error.is_timeout() || error.is_connect() => (error.to_string(), None),
            _ => return outcome,
        };
        if attempt >= attempts {
            return outcome;
        }
        // Like the sinks' other messages: stdout is the job log an operator reads, tracing is
        // the structured log.
        println!(
            "# [GCP :: {service}] {reason}; retrying ({}/{attempts})",
            attempt + 1
        );
        warn!(target: "gcp", service, attempt, %reason, "transient GCP failure, retrying");
        tokio::time::sleep(asked.map_or(delay, |wait| wait.max(delay))).await;
        delay = next_delay(delay);
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_http::ScriptedServer;
    use super::*;

    #[test]
    fn the_base_delay_setting_accepts_milliseconds_up_to_a_minute() {
        assert_eq!(base_delay_setting("0"), Ok(Duration::ZERO));
        assert_eq!(base_delay_setting(" 250 "), Ok(Duration::from_millis(250)));
        assert_eq!(base_delay_setting("60000"), Ok(MAX_BASE_DELAY));
    }

    #[test]
    fn a_malformed_or_huge_base_delay_setting_is_refused() {
        assert!(base_delay_setting("fast").is_err());
        assert!(base_delay_setting("-5").is_err());
        assert!(base_delay_setting("60001").is_err());
        assert!(base_delay_setting(&u64::MAX.to_string()).is_err());
    }

    #[test]
    fn the_doubling_wait_saturates_instead_of_overflowing() {
        assert_eq!(next_delay(Duration::from_secs(1)), Duration::from_secs(2));
        assert_eq!(next_delay(Duration::ZERO), Duration::ZERO);
        assert_eq!(next_delay(Duration::from_secs(100)), MAX_WAIT);
        assert_eq!(next_delay(Duration::MAX), MAX_WAIT);
    }

    #[tokio::test]
    async fn a_server_error_keeps_the_backoff_schedule_even_with_a_retry_after() {
        // Retry-After is honoured for 429 only, as documented; a 503 keeps the 1 s/2 s schedule.
        let server = ScriptedServer::start(&[
            ("503 Service Unavailable\r\nRetry-After: 30", "{}"),
            ("200 OK", "{}"),
        ]);
        let client = reqwest::Client::new();
        let started = std::time::Instant::now();

        let response = send_with_retry(RetryPolicy::immediate(3), "test", || {
            client.get(&server.base_url)
        })
        .await
        .unwrap();

        assert!(response.status().is_success());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(server.request_lines().len(), 2);
    }

    #[tokio::test]
    async fn a_rate_limit_waits_at_least_the_retry_after_the_server_asks_for() {
        let server = ScriptedServer::start(&[
            ("429 Too Many Requests\r\nRetry-After: 1", "{}"),
            ("200 OK", "{}"),
        ]);
        let client = reqwest::Client::new();
        let started = std::time::Instant::now();

        let response = send_with_retry(RetryPolicy::immediate(3), "test", || {
            client.get(&server.base_url)
        })
        .await
        .unwrap();

        assert!(response.status().is_success());
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(server.request_lines().len(), 2);
    }
}
