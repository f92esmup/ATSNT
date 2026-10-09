//! Google Cloud Platform (GCP) and Outbound Alerting Adapters for ATSNT.
//!
//! Provides cloud-first streaming telemetry, serverless data warehousing, and mobile push notifications:
//! - [`bigquery`]: Direct streaming inserts to BigQuery tables (`trades`, `equity_snapshots`, `hpo_evaluations`, `monte_carlo_runs`) for zero-code Looker Studio dashboards.
//! - [`gcs`]: Columnar Apache Parquet persistence for simulation trajectories and historical data.
//! - [`telegram`]: Mobile push alerting via official Telegram Bot API webhooks.

pub mod auth;
pub mod bigquery;
pub mod gcs;
pub mod telegram;

pub use auth::GcpAuthResolver;
pub use bigquery::{BigQuerySink, EquitySnapshotRow, HpoEvaluationRow, MonteCarloRow, TradeRow};
pub use gcs::GcsParquetSink;
pub use telegram::TelegramNotifier;

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
