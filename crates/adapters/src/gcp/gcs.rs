//! Google Cloud Storage (GCS) Columnar Parquet Sink for ATSNT.
//!
//! Stores simulation datasets, Monte Carlo trajectory ensembles, and historical Dollar Bars
//! as compressed, immutable Apache Parquet objects on GCS buckets for cost-effective long-term
//! archiving and external querying via BigQuery.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::error::AdapterError;

/// Schema for Monte Carlo equity curve trajectories in Apache Parquet.
pub fn monte_carlo_trajectory_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("run_id", DataType::Utf8, false),
        Field::new("label", DataType::Utf8, false),
        Field::new("step_index", DataType::Int64, false),
        Field::new("equity", DataType::Utf8, false),
    ]))
}

/// GCS Parquet exporter and local staging sink.
#[derive(Debug, Clone)]
pub struct GcsParquetSink {
    client: reqwest::Client,
    bucket_name: Option<String>,
    auth_token: Option<String>,
    staging_dir: PathBuf,
}

impl GcsParquetSink {
    /// Creates a sink targeting a local staging directory and optional GCS bucket.
    pub fn new(
        bucket_name: Option<String>,
        auth_token: Option<String>,
        staging_dir: PathBuf,
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            bucket_name,
            auth_token,
            staging_dir,
        }
    }

    /// Initializes from environment variables `GCS_BUCKET_NAME` and `GCP_AUTH_TOKEN`.
    pub fn from_env<P: AsRef<Path>>(staging_dir: P) -> Self {
        let bucket = std::env::var("GCS_BUCKET_NAME")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let token = std::env::var("GCP_AUTH_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::new(bucket, token, staging_dir.as_ref().to_path_buf())
    }

    /// Returns `true` if GCS cloud upload is configured and active.
    #[inline]
    pub fn is_cloud_enabled(&self) -> bool {
        self.bucket_name.is_some() && self.auth_token.is_some()
    }

    /// Writes Monte Carlo simulation trajectories to a compressed Parquet file and optionally
    /// uploads it to the configured GCS bucket.
    pub async fn export_monte_carlo_trajectories(
        &self,
        run_id: &str,
        trajectories: &[(&str, &[Decimal])],
    ) -> Result<PathBuf, AdapterError> {
        std::fs::create_dir_all(&self.staging_dir)?;
        let file_name = format!("monte_carlo_{}.parquet", run_id);
        let file_path = self.staging_dir.join(&file_name);

        let file = File::create(&file_path)?;
        let schema = monte_carlo_trajectory_schema();
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(props))
            .map_err(|e| AdapterError::General(e.to_string()))?;

        let mut run_ids: Vec<&str> = Vec::new();
        let mut labels: Vec<&str> = Vec::new();
        let mut step_indices: Vec<i64> = Vec::new();
        let mut equities: Vec<String> = Vec::new();

        for (label, curve) in trajectories {
            for (step, equity) in curve.iter().enumerate() {
                run_ids.push(run_id);
                labels.push(*label);
                step_indices.push(step as i64);
                equities.push(equity.to_string());
            }
        }

        let run_id_arr: ArrayRef = Arc::new(StringArray::from(run_ids));
        let label_arr: ArrayRef = Arc::new(StringArray::from(labels));
        let step_arr: ArrayRef = Arc::new(Int64Array::from(step_indices));
        let equity_arr: ArrayRef = Arc::new(StringArray::from(equities));

        let batch = RecordBatch::try_new(schema, vec![run_id_arr, label_arr, step_arr, equity_arr])
            .map_err(|e| AdapterError::General(e.to_string()))?;

        writer
            .write(&batch)
            .map_err(|e| AdapterError::General(e.to_string()))?;
        writer
            .close()
            .map_err(|e| AdapterError::General(e.to_string()))?;

        info!(
            target: "gcs",
            path = %file_path.display(),
            "Monte Carlo trajectories saved to Parquet"
        );

        // Upload to GCS if credentials and bucket are present
        if let (Some(bucket), Some(token)) = (&self.bucket_name, &self.auth_token) {
            let bytes = std::fs::read(&file_path)?;
            let object_name = format!("simulations/{}", file_name);
            let url = format!(
                "https://storage.googleapis.com/upload/storage/v1/b/{}/o?uploadType=media&name={}",
                bucket, object_name
            );

            let res = self
                .client
                .post(&url)
                .bearer_auth(token)
                .header("Content-Type", "application/octet-stream")
                .body(bytes)
                .send()
                .await?;

            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                warn!(
                    target: "gcs",
                    status = %status,
                    body = %body,
                    "GCS Parquet upload returned non-success"
                );
            } else {
                info!(
                    target: "gcs",
                    bucket = %bucket,
                    object = %object_name,
                    "Parquet file successfully uploaded to GCS"
                );
            }
        }

        Ok(file_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use tempfile::tempdir;

    #[tokio::test]
    async fn export_monte_carlo_trajectories_writes_valid_parquet_file() {
        let temp = tempdir().unwrap();
        let sink = GcsParquetSink::new(None, None, temp.path().to_path_buf());
        assert!(!sink.is_cloud_enabled());

        let t1 = [dec!(10000), dec!(10050), dec!(10120)];
        let t2 = [dec!(10000), dec!(9950), dec!(9890)];
        let trajectories = [("Realized", &t1[..]), ("P05_Worst", &t2[..])];

        let path = sink
            .export_monte_carlo_trajectories("test_run_42", &trajectories)
            .await
            .unwrap();

        assert!(path.exists());
        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 0);
    }
}
