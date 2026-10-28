//! Google Cloud Platform (GCP) and Outbound Alerting Adapters for ATSNT.
//!
//! Provides cloud-first streaming telemetry, serverless data warehousing, and mobile push notifications:
//! - [`bigquery`]: [`crate::RunRecorder`] implementation streaming run analytics (`runs`, `run_metrics`, `trades`, `run_series`) for downstream consumers.
//! - [`gcs`]: Uploads of strategy deployment artifacts to Cloud Storage.
//! - [`telegram`]: Mobile push alerting via official Telegram Bot API webhooks.

pub mod auth;
pub mod bigquery;
pub mod gcs;
pub mod retry;
pub mod secret_manager;
pub mod telegram;
#[cfg(test)]
pub(crate) mod test_http;

pub use auth::GcpAuthResolver;
pub use bigquery::{BigQueryRecord, BigQuerySink};
pub use gcs::GcsArtifactSink;
pub use retry::{RetryPolicy, RETRY_BASE_MS_ENV};
pub use secret_manager::GcpSecretManager;
pub use telegram::{datetime_in_madrid, now_in_madrid, SymbolPerformanceSummary, TelegramNotifier};

/// Formats a Unix timestamp in milliseconds as an RFC3339 UTC string.
pub fn format_unix_ms_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_rfc3339_correctness() {
        let ts = 1_700_000_000_000;
        let formatted = format_unix_ms_rfc3339(ts);
        assert!(formatted.starts_with("2023-11-14T22:13:20"));
    }
}
