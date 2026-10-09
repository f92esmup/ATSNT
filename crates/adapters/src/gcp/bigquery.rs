//! Google Cloud BigQuery Streaming Ingestion Adapter for ATSNT.
//!
//! Provides direct, serverless streaming writes to BigQuery tables (`trades`, `equity_snapshots`,
//! `hpo_evaluations`, `monte_carlo_runs`) so that Looker Studio can display real-time analytics
//! and historical metrics with zero frontend code.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use super::auth::GcpAuthResolver;
use crate::error::AdapterError;

/// Individual closed trade audit record for BigQuery `trades` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeRow {
    pub trade_id: String,
    pub session_id: String,
    pub strategy_id: String,
    pub symbol: String,
    pub side: String,
    pub entry_timestamp: String,
    pub exit_timestamp: String,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub quantity: Decimal,
    pub gross_pnl: Decimal,
    pub fees_paid: Decimal,
    pub net_pnl: Decimal,
    pub exit_reason: String,
    pub holding_duration_seconds: i64,
}

/// Periodic mark-to-market account valuation for BigQuery `equity_snapshots` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquitySnapshotRow {
    pub timestamp: String,
    pub session_id: String,
    pub symbol: String,
    pub cash_equity: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_equity: Decimal,
    pub drawdown_pct: Decimal,
    pub active_position_side: Option<String>,
    pub active_position_qty: Option<Decimal>,
}

/// Parameter optimization evaluation record for BigQuery `hpo_evaluations` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HpoEvaluationRow {
    pub run_id: String,
    pub timestamp: String,
    pub strategy_id: String,
    pub parameter_space: String,
    pub is_sharpe: Decimal,
    pub oos_sharpe: Decimal,
    pub deflated_sharpe_ratio: Decimal,
    pub parameter_stability_score: Decimal,
    pub total_trades: u64,
    pub win_rate: Decimal,
    pub max_drawdown_pct: Decimal,
}

/// Statistical risk summary for BigQuery `monte_carlo_runs` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonteCarloRow {
    pub run_id: String,
    pub timestamp: String,
    pub strategy_id: String,
    pub iterations: u64,
    pub resample_method: String,
    pub historical_max_drawdown: Decimal,
    pub p50_max_drawdown: Decimal,
    pub p95_max_drawdown: Decimal,
    pub p99_max_drawdown: Decimal,
    pub probability_of_ruin_pct: Decimal,
}

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
        self.project_id.is_some() && self.auth_token.is_some()
    }

    /// Streaming insert of generic rows into a BigQuery table.
    pub async fn insert_rows<T: Serialize>(
        &self,
        table_id: &str,
        rows: &[T],
    ) -> Result<(), AdapterError> {
        if rows.is_empty() {
            return Ok(());
        }

        let project_id = match &self.project_id {
            Some(p) => Some(p.clone()),
            None => GcpAuthResolver::resolve_project_id(&self.client).await,
        };
        let token = match &self.auth_token {
            Some(t) => Some(t.clone()),
            None => GcpAuthResolver::resolve_token(&self.client).await,
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

        let row_envelopes: Vec<_> = rows.iter().map(|r| json!({ "json": r })).collect();
        let payload = json!({
            "kind": "bigquery#tableDataInsertAllRequest",
            "rows": row_envelopes,
        });

        let response = self
            .client
            .post(&url)
            .bearer_auth(&token)
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
                    .post(&url)
                    .bearer_auth(fresh_token)
                    .json(&payload)
                    .send()
                    .await?;
                if retry_resp.status().is_success() {
                    if table_id == "equity_snapshots" {
                        println!(
                            "# [GCP :: BigQuery] Telemetry snapshot streamed -> {}:{}.{} ({} row)",
                            project_id,
                            self.dataset_id,
                            table_id,
                            rows.len()
                        );
                    } else {
                        println!(
                            "\n############################################################\n\
                             # [GCP :: BigQuery] STREAMING INSERT SUCCESSFUL            #\n\
                             # Service: Google Cloud BigQuery                          #\n\
                             # Table:   {}:{}.{}\n\
                             # Rows:    {} row(s) streamed (token renewed)            #\n\
                             ############################################################\n",
                            project_id,
                            self.dataset_id,
                            table_id,
                            rows.len()
                        );
                    }
                    info!(
                        target: "bigquery",
                        table = %table_id,
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
                "\n############################################################\n\
                 # [GCP :: BigQuery ERROR] Streaming Insert Failed          #\n\
                 # Table:   {}:{}.{}\n\
                 # Status:  {}\n\
                 # Details: {}\n\
                 ############################################################\n",
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
                            "\n############################################################\n\
                             # [GCP :: BigQuery WARNING] Row Validation Errors          #\n\
                             # Table:   {}:{}.{}\n\
                             # Errors:  {}\n\
                             ############################################################\n",
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

            if table_id == "equity_snapshots" {
                println!(
                    "# [GCP :: BigQuery] Telemetry snapshot streamed -> {}:{}.{} ({} row)",
                    project_id,
                    self.dataset_id,
                    table_id,
                    rows.len()
                );
            } else {
                println!(
                    "\n############################################################\n\
                     # [GCP :: BigQuery] STREAMING INSERT SUCCESSFUL            #\n\
                     # Service: Google Cloud BigQuery                          #\n\
                     # Table:   {}:{}.{}\n\
                     # Rows:    {} row(s) streamed successfully                 #\n\
                     ############################################################\n",
                    project_id,
                    self.dataset_id,
                    table_id,
                    rows.len()
                );
            }
        }

        Ok(())
    }

    /// Convenience wrapper for inserting trade audit records.
    pub async fn insert_trades(&self, rows: &[TradeRow]) -> Result<(), AdapterError> {
        self.insert_rows("trades", rows).await
    }

    /// Convenience wrapper for inserting equity snapshots.
    pub async fn insert_equity_snapshots(
        &self,
        rows: &[EquitySnapshotRow],
    ) -> Result<(), AdapterError> {
        self.insert_rows("equity_snapshots", rows).await
    }

    /// Convenience wrapper for inserting HPO parameter evaluations.
    pub async fn insert_hpo_evaluations(
        &self,
        rows: &[HpoEvaluationRow],
    ) -> Result<(), AdapterError> {
        self.insert_rows("hpo_evaluations", rows).await
    }

    /// Convenience wrapper for inserting Monte Carlo summary metrics.
    pub async fn insert_monte_carlo_runs(
        &self,
        rows: &[MonteCarloRow],
    ) -> Result<(), AdapterError> {
        self.insert_rows("monte_carlo_runs", rows).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn disabled_bigquery_sink_succeeds_without_panic() {
        let sink = BigQuerySink::new(None, "atsnt_bi".into(), None);
        assert!(!sink.is_enabled());

        let trade = TradeRow {
            trade_id: "trade_1".into(),
            session_id: "sess_1".into(),
            strategy_id: "dollar_bars_cusum".into(),
            symbol: "btcusdt".into(),
            side: "Buy".into(),
            entry_timestamp: "2026-10-08T12:00:00Z".into(),
            exit_timestamp: "2026-10-08T12:15:00Z".into(),
            entry_price: dec!(65000),
            exit_price: dec!(66000),
            quantity: dec!(0.5),
            gross_pnl: dec!(500),
            fees_paid: dec!(10),
            net_pnl: dec!(490),
            exit_reason: "TakeProfit".into(),
            holding_duration_seconds: 900,
        };

        assert!(sink.insert_trades(&[trade]).await.is_ok());
    }

    #[test]
    fn row_serialization_produces_valid_json() {
        let snapshot = EquitySnapshotRow {
            timestamp: "2026-10-08T12:00:00Z".into(),
            session_id: "sess_1".into(),
            symbol: "btcusdt".into(),
            cash_equity: dec!(10000),
            unrealized_pnl: dec!(150),
            total_equity: dec!(10150),
            drawdown_pct: dec!(0.02),
            active_position_side: Some("Long".into()),
            active_position_qty: Some(dec!(0.2)),
        };

        let json_val = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json_val["symbol"], "btcusdt");
        assert_eq!(json_val["cash_equity"], "10000");
        assert_eq!(json_val["active_position_side"], "Long");
    }
}
