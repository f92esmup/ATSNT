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

    /// The default policy, with the base delay from [`RETRY_BASE_MS_ENV`] when it is set.
    pub fn from_env() -> Self {
        let policy = Self::default();
        match std::env::var(RETRY_BASE_MS_ENV)
            .ok()
            .and_then(|v| v.trim().parse().ok())
        {
            Some(ms) => Self {
                base_delay: Duration::from_millis(ms),
                ..policy
            },
            None => policy,
        }
    }
}

/// Whether a response status is worth another attempt.
fn is_transient(status: reqwest::StatusCode) -> bool {
    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// Sends the request built by `build` until it gets a non-transient answer or the attempts
/// run out; the last response or transport error is returned as is.
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
        let reason = match &outcome {
            Ok(response) if is_transient(response.status()) => {
                format!("HTTP {}", response.status())
            }
            Err(error) if error.is_timeout() || error.is_connect() => error.to_string(),
            _ => return outcome,
        };
        if attempt >= attempts {
            return outcome;
        }
        println!(
            "# [GCP :: {service}] {reason}; retrying ({}/{attempts})",
            attempt + 1
        );
        warn!(target: "gcp", service, attempt, %reason, "transient GCP failure, retrying");
        tokio::time::sleep(delay).await;
        delay *= 2;
        attempt += 1;
    }
}
