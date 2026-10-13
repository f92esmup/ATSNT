//! Google Cloud Storage (GCS) artifact uploader for ATSNT.
//!
//! Uploads the strategy deployment artifact produced by HPO so deployments can
//! fetch the exact configuration that was optimized. Analytics data lives in
//! BigQuery, not in GCS.

use std::path::Path;

use tracing::{info, warn};

use super::auth::GcpAuthResolver;
use crate::error::AdapterError;

/// Uploader of small artifact files to a GCS bucket.
#[derive(Debug, Clone)]
pub struct GcsArtifactSink {
    client: reqwest::Client,
    bucket_name: Option<String>,
    auth_token: Option<String>,
    /// Whether missing credentials may be resolved from the environment
    /// (metadata server, gcloud CLI). `false` keeps the sink fully offline.
    ambient_auth: bool,
}

impl GcsArtifactSink {
    /// Creates a sink for an optional bucket and token, resolving missing
    /// values from the environment at upload time.
    pub fn new(bucket_name: Option<String>, auth_token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            bucket_name,
            auth_token,
            ambient_auth: true,
        }
    }

    /// Sink that never resolves ambient credentials (tests and offline runs).
    pub fn offline() -> Self {
        Self {
            ambient_auth: false,
            ..Self::new(None, None)
        }
    }

    /// Initializes from environment variables `GCS_BUCKET_NAME` and `GCP_AUTH_TOKEN`.
    pub fn from_env() -> Self {
        let bucket = std::env::var("GCS_BUCKET_NAME")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let token = std::env::var("GCP_AUTH_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::new(bucket, token)
    }

    /// Returns `true` if GCS cloud upload is explicitly configured.
    #[inline]
    pub fn is_cloud_enabled(&self) -> bool {
        self.bucket_name.is_some() && self.auth_token.is_some()
    }

    /// Uploads raw bytes directly to a GCS object in the configured or resolved bucket.
    pub async fn upload_bytes(
        &self,
        bytes: Vec<u8>,
        object_name: &str,
        content_type: &str,
    ) -> Result<(), AdapterError> {
        let bucket = match &self.bucket_name {
            Some(b) => Some(b.clone()),
            None if self.ambient_auth => GcpAuthResolver::resolve_project_id(&self.client)
                .await
                .map(|p| format!("atsnt-lake-{}", p)),
            None => None,
        };

        let token = match &self.auth_token {
            Some(t) => Some(t.clone()),
            None if self.ambient_auth => GcpAuthResolver::resolve_token(&self.client).await,
            None => None,
        };

        let (Some(bucket), Some(token)) = (bucket, token) else {
            println!(
                "# [GCP :: Cloud Storage] Upload skipped for {} (credentials or bucket not resolved)",
                object_name
            );
            info!(
                target: "gcs",
                object = %object_name,
                "GCS upload skipped (GCP credentials/bucket not resolved)"
            );
            return Ok(());
        };

        let url = format!(
            "https://storage.googleapis.com/upload/storage/v1/b/{}/o?uploadType=media&name={}",
            bucket, object_name
        );

        let res = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .header("Content-Type", content_type)
            .body(bytes.clone())
            .send()
            .await?;

        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            warn!(
                target: "gcs",
                object = %object_name,
                "GCS returned 401 Unauthorized; attempting token renewal..."
            );
            if let Some(fresh_token) = GcpAuthResolver::resolve_token(&self.client).await {
                let retry = self
                    .client
                    .post(&url)
                    .bearer_auth(fresh_token)
                    .header("Content-Type", content_type)
                    .body(bytes.clone())
                    .send()
                    .await?;
                if retry.status().is_success() {
                    let size_kb = bytes.len() as f64 / 1024.0;
                    println!(
                        "\n############################################################\n\
                         # [GCP :: Cloud Storage] UPLOAD COMPLETE (Token Renewed)   #\n\
                         # Service: Google Cloud Storage (GCS)                     #\n\
                         # Target:  gs://{}/{}\n\
                         # Size:    {} bytes ({:.2} KB)\n\
                         # Type:    {}\n\
                         ############################################################\n",
                        bucket,
                        object_name,
                        bytes.len(),
                        size_kb,
                        content_type
                    );
                    info!(
                        target: "gcs",
                        bucket = %bucket,
                        object = %object_name,
                        "GCS upload succeeded after token renewal"
                    );
                    return Ok(());
                }
            }
        }

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            println!(
                "\n############################################################\n\
                 # [GCP :: Cloud Storage ERROR] Upload Failed               #\n\
                 # Target: gs://{}/{}\n\
                 # Status: {}\n\
                 # Body:   {}\n\
                 ############################################################\n",
                bucket, object_name, status, body
            );
            warn!(
                target: "gcs",
                status = %status,
                object = %object_name,
                body = %body,
                "GCS upload returned non-success"
            );
        } else {
            let size_kb = bytes.len() as f64 / 1024.0;
            println!(
                "\n############################################################\n\
                 # [GCP :: Cloud Storage] UPLOAD COMPLETE                   #\n\
                 # Service: Google Cloud Storage (GCS)                     #\n\
                 # Target:  gs://{}/{}\n\
                 # Size:    {} bytes ({:.2} KB)\n\
                 # Type:    {}\n\
                 ############################################################\n",
                bucket,
                object_name,
                bytes.len(),
                size_kb,
                content_type
            );
            info!(
                target: "gcs",
                bucket = %bucket,
                object = %object_name,
                "File successfully uploaded to GCS"
            );
        }

        Ok(())
    }

    /// Uploads an arbitrary local file to GCS.
    pub async fn upload_file<P: AsRef<Path>>(
        &self,
        local_path: P,
        object_name: &str,
        content_type: &str,
    ) -> Result<(), AdapterError> {
        let bytes = std::fs::read(local_path.as_ref())?;
        self.upload_bytes(bytes, object_name, content_type).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn offline_sink_skips_upload_without_resolving_credentials() {
        let sink = GcsArtifactSink::offline();
        assert!(!sink.is_cloud_enabled());
        let result = sink
            .upload_bytes(b"{}".to_vec(), "artifacts/test.json", "application/json")
            .await;
        assert!(result.is_ok());
    }
}
