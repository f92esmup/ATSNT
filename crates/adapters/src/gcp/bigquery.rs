//! Google Cloud BigQuery Streaming Ingestion Adapter for ATSNT.
//!
//! Implements the [`RunRecorder`] port with streaming writes to the run-centric
//! analytics tables (`runs`, `run_metrics`, `trades`, `run_series`) that Looker
//! Studio reads through the `v_*` views (see `scripts/gcp/bigquery_schema.sql`).

use serde::Serialize;
use serde_json::json;
use tracing::{info, warn};

use super::auth::GcpAuthResolver;
use crate::analytics::{RunMetricsRow, RunRecorder, RunRow, RunTradeRow, SeriesPointRow};
use crate::error::AdapterError;

/// Trait representing rows destined for BigQuery streaming tables, providing deduplication keys.
pub trait BigQueryRecord: Serialize {
    /// Unique identifier attached as `insertId` in `tabledata.insertAll` for ingestion idempotency.
    fn insert_id(&self) -> Option<String> {
        None
    }
}

impl BigQueryRecord for RunRow {
    fn insert_id(&self) -> Option<String> {
        Some(format!("{}_{}", self.run_id, self.status))
    }
}

impl BigQueryRecord for RunMetricsRow {
    fn insert_id(&self) -> Option<String> {
        Some(format!("{}_{}", self.run_id, self.scope))
    }
}

impl BigQueryRecord for RunTradeRow {
    fn insert_id(&self) -> Option<String> {
        Some(self.trade_id.clone())
    }
}

impl BigQueryRecord for SeriesPointRow {
    fn insert_id(&self) -> Option<String> {
        Some(format!(
            "{}_{}_{}_{}_{}_{}",
            self.run_id,
            self.series,
            self.event,
            self.ts.as_deref().unwrap_or(""),
            self.x.map(|x| x.to_string()).unwrap_or_default(),
            self.trade_id.as_deref().unwrap_or("")
        ))
    }
}

/// Rows per `insertAll` request, following BigQuery's recommended batch size.
const MAX_ROWS_PER_REQUEST: usize = 500;

/// BigQuery streaming data sink.
///
/// Dispatches rows directly to the BigQuery `tabledata.insertAll` REST API.
/// If `project_id` or `auth_token` are not configured (e.g. during local tests or offline mode),
/// writes complete gracefully with an informational log.
#[derive(Debug, Clone)]
pub struct BigQuerySink {
    client: reqwest::Client,
    project_id: Option<String>,
    dataset_id: String,
    auth_token: Option<String>,
    /// Whether missing credentials may be resolved from the environment
    /// (metadata server, gcloud CLI). `false` keeps the sink fully offline.
    ambient_auth: bool,
}

impl BigQuerySink {
    /// Constructs a sink with explicit configuration.
    pub fn new(project_id: Option<String>, dataset_id: String, auth_token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            project_id,
            dataset_id,
            auth_token,
            ambient_auth: true,
        }
    }

    /// Sink that never resolves ambient credentials and therefore never
    /// reaches the network unless explicit credentials are provided. Used by
    /// tests and offline runs.
    pub fn offline(dataset_id: impl Into<String>) -> Self {
        Self {
            ambient_auth: false,
            ..Self::new(None, dataset_id.into(), None)
        }
    }

    /// Initializes from environment variables `GCP_PROJECT_ID`, `GCP_DATASET_ID`, and `GCP_AUTH_TOKEN`.
    pub fn from_env() -> Self {
        let project = std::env::var("GCP_PROJECT_ID")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let dataset = std::env::var("GCP_DATASET_ID").unwrap_or_else(|_| "atsnt_bi".to_string());
        let token = std::env::var("GCP_AUTH_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::new(project, dataset, token)
    }

    /// Returns `true` if BigQuery streaming is enabled and configured.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.project_id.is_some() || self.auth_token.is_some()
    }

    /// Streaming insert of generic rows into a BigQuery table with idempotency deduplication keys.
    pub async fn insert_rows<T: BigQueryRecord>(
        &self,
        table_id: &str,
        rows: &[T],
    ) -> Result<(), AdapterError> {
        if rows.is_empty() {
            return Ok(());
        }

        let project_id = match &self.project_id {
            Some(p) => Some(p.clone()),
            None if self.ambient_auth => GcpAuthResolver::resolve_project_id(&self.client).await,
            None => None,
        };
        let token = match &self.auth_token {
            Some(t) => Some(t.clone()),
            None if self.ambient_auth => GcpAuthResolver::resolve_token(&self.client).await,
            None => None,
        };

        let (Some(project_id), Some(token)) = (project_id, token) else {
            println!(
                "# [GCP :: BigQuery] Insert skipped for {}.{} (credentials or project not resolved)",
                self.dataset_id, table_id
            );
            info!(
                target: "bigquery",
                table = %table_id,
                count = rows.len(),
                "BigQuery insert skipped (GCP credentials/project not resolved)"
            );
            return Ok(());
        };

        let url = format!(
            "https://bigquery.googleapis.com/bigquery/v2/projects/{}/datasets/{}/tables/{}/insertAll",
            project_id, self.dataset_id, table_id
        );

        for chunk in rows.chunks(MAX_ROWS_PER_REQUEST) {
            self.insert_chunk(&url, &token, &project_id, table_id, chunk)
                .await?;
        }

        Ok(())
    }

    /// Sends one `insertAll` request (at most [`MAX_ROWS_PER_REQUEST`] rows).
    async fn insert_chunk<T: BigQueryRecord>(
        &self,
        url: &str,
        token: &str,
        project_id: &str,
        table_id: &str,
        rows: &[T],
    ) -> Result<(), AdapterError> {
        let row_envelopes: Vec<_> = rows
            .iter()
            .map(|r| {
                if let Some(id) = r.insert_id() {
                    json!({
                        "insertId": id,
                        "json": r
                    })
                } else {
                    json!({
                        "json": r
                    })
                }
            })
            .collect();
        let payload = json!({
            "kind": "bigquery#tableDataInsertAllRequest",
            "rows": row_envelopes,
        });

        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            warn!(
                target: "bigquery",
                table = %table_id,
                "BigQuery returned 401 Unauthorized; attempting token renewal..."
            );
            if let Some(fresh_token) = GcpAuthResolver::resolve_token(&self.client).await {
                let retry_resp = self
                    .client
                    .post(url)
                    .bearer_auth(fresh_token)
                    .json(&payload)
                    .send()
                    .await?;
                if retry_resp.status().is_success() {
                    println!(
                        "# [GCP :: BigQuery] Streamed {} row(s) -> {}:{}.{} (token renewed)",
                        rows.len(),
                        project_id,
                        self.dataset_id,
                        table_id
                    );
                    info!(
                        target: "bigquery",
                        table = %table_id,
                        count = rows.len(),
                        "BigQuery insert succeeded after token renewal"
                    );
                    return Ok(());
                }
            }
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            println!(
                "# [GCP :: BigQuery ERROR] Streaming insert failed -> {}:{}.{} | Status: {} | Details: {}",
                project_id, self.dataset_id, table_id, status, body
            );
            warn!(
                target: "bigquery",
                status = %status,
                table = %table_id,
                body = %body,
                "BigQuery streaming insert returned non-success"
            );
        } else if let Ok(resp_json) = response.json::<serde_json::Value>().await {
            if let Some(errors) = resp_json.get("insertErrors") {
                if let Some(arr) = errors.as_array() {
                    if !arr.is_empty() {
                        println!(
                            "# [GCP :: BigQuery WARNING] Row validation errors -> {}:{}.{} | Errors: {}",
                            project_id, self.dataset_id, table_id, errors
                        );
                        warn!(
                            target: "bigquery",
                            table = %table_id,
                            errors = %errors,
                            "BigQuery streaming insert returned row validation errors"
                        );
                        return Ok(());
                    }
                }
            }

            println!(
                "# [GCP :: BigQuery] Streamed {} row(s) -> {}:{}.{}",
                rows.len(),
                project_id,
                self.dataset_id,
                table_id
            );
            info!(
                target: "bigquery",
                table = %table_id,
                count = rows.len(),
                "BigQuery streaming insert succeeded"
            );
        }

        Ok(())
    }
}

impl RunRecorder for BigQuerySink {
    async fn record_runs(&self, rows: &[RunRow]) -> Result<(), AdapterError> {
        self.insert_rows("runs", rows).await
    }

    async fn record_metrics(&self, rows: &[RunMetricsRow]) -> Result<(), AdapterError> {
        self.insert_rows("run_metrics", rows).await
    }

    async fn record_trades(&self, rows: &[RunTradeRow]) -> Result<(), AdapterError> {
        self.insert_rows("trades", rows).await
    }

    async fn record_series(&self, rows: &[SeriesPointRow]) -> Result<(), AdapterError> {
        self.insert_rows("run_series", rows).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn equity_point(event: &str, trade_id: &str) -> SeriesPointRow {
        SeriesPointRow {
            run_id: "r1".into(),
            run_type: "backtest".into(),
            series: "equity".into(),
            event: event.into(),
            ts: Some("2026-10-08T12:00:00Z".into()),
            x: None,
            value: dec!(10000),
            trade_id: Some(trade_id.into()),
        }
    }

    #[tokio::test]
    async fn offline_sink_skips_inserts_without_resolving_credentials() {
        let sink = BigQuerySink::offline("atsnt_bi");
        assert!(!sink.is_enabled());
        let metrics = RunMetricsRow::extras_only("r1", "monte_carlo", "full", "t", "{}");
        assert!(sink.record_metrics(&[metrics]).await.is_ok());
        assert!(sink
            .record_series(&[equity_point("entry", "t1")])
            .await
            .is_ok());
    }

    #[test]
    fn series_insert_ids_distinguish_events_and_trades() {
        let ids = [
            equity_point("entry", "t1").insert_id(),
            equity_point("exit", "t1").insert_id(),
            equity_point("entry", "t2").insert_id(),
        ];
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[0], ids[2]);
    }
}
