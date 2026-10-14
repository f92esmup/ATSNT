//! Google Cloud Storage (GCS) artifact uploader for ATSNT.
//!
//! Uploads the strategy deployment artifact produced by HPO so deployments can
//! fetch the exact configuration that was optimized. Analytics data lives in
//! BigQuery, not in GCS.

use std::path::Path;

use tracing::{info, warn};

use super::auth::GcpAuthResolver;
use crate::error::AdapterError;

/// Public Cloud Storage JSON API endpoint.
const DEFAULT_ENDPOINT: &str = "https://storage.googleapis.com";

/// Environment variable overriding the Cloud Storage API endpoint (emulators and tests).
pub const ENDPOINT_ENV: &str = "ATSNT_GCS_ENDPOINT";

/// Uploader of small artifact files to a GCS bucket.
///
/// Upload failures are logged and tolerated by default; in strict mode
/// ([`GcsArtifactSink::with_strict`]) they are returned as errors.
#[derive(Debug, Clone)]
pub struct GcsArtifactSink {
    client: reqwest::Client,
    bucket_name: Option<String>,
    auth_token: Option<String>,
    /// Whether missing credentials may be resolved from the environment
    /// (metadata server, gcloud CLI). `false` keeps the sink fully offline.
    ambient_auth: bool,
    /// Whether upload failures are returned as errors instead of being logged.
    strict: bool,
    /// Base URL of the Cloud Storage JSON API.
    endpoint: String,
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
            strict: false,
            endpoint: DEFAULT_ENDPOINT.to_string(),
        }
    }

    /// Makes upload failures (unresolved credentials, HTTP errors) return `Err`
    /// instead of only being logged.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Overrides the Cloud Storage API base URL (e.g. an emulator).
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.trim_end_matches('/').to_string();
        self
    }

    /// Returns `Ok` in tolerant mode and the failure as an error in strict mode.
    fn tolerate(&self, failure: String) -> Result<(), AdapterError> {
        if self.strict {
            Err(AdapterError::General(failure))
        } else {
            Ok(())
        }
    }

    /// Sink that never resolves ambient credentials (tests and offline runs).
    pub fn offline() -> Self {
        Self {
            ambient_auth: false,
            ..Self::new(None, None)
        }
    }

    /// Initializes from environment variables `GCS_BUCKET_NAME`, `GCP_AUTH_TOKEN`
    /// and the optional endpoint override [`ENDPOINT_ENV`].
    pub fn from_env() -> Self {
        let bucket = std::env::var("GCS_BUCKET_NAME")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let token = std::env::var("GCP_AUTH_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let sink = Self::new(bucket, token);
        match std::env::var(ENDPOINT_ENV) {
            Ok(endpoint) if !endpoint.trim().is_empty() => sink.with_endpoint(endpoint.trim()),
            _ => sink,
        }
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
            return self.tolerate(format!(
                "GCS upload of {} failed: credentials or bucket not resolved",
                object_name
            ));
        };

        let url = format!(
            "{}/upload/storage/v1/b/{}/o?uploadType=media&name={}",
            self.endpoint, bucket, object_name
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
            return self.tolerate(format!(
                "GCS upload of gs://{}/{} failed with HTTP {}: {}",
                bucket, object_name, status, body
            ));
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
    use super::super::test_http;
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

    fn configured_sink(base_url: &str) -> GcsArtifactSink {
        GcsArtifactSink::new(Some("bucket".into()), Some("t".into())).with_endpoint(base_url)
    }

    #[tokio::test]
    async fn strict_sink_fails_when_credentials_are_not_resolved() {
        let sink = GcsArtifactSink::offline().with_strict(true);
        let err = sink
            .upload_bytes(b"{}".to_vec(), "artifacts/test.json", "application/json")
            .await;
        assert!(err
            .unwrap_err()
            .to_string()
            .contains("credentials or bucket not resolved"));
    }

    #[tokio::test]
    async fn default_sink_tolerates_http_errors() {
        let server = test_http::OneShotServer::start("403 Forbidden", "{}");
        let sink = configured_sink(&server.base_url);
        let result = sink
            .upload_bytes(b"{}".to_vec(), "artifacts/test.json", "application/json")
            .await;
        assert!(result.is_ok());
        assert!(server.request_line().starts_with(
            "POST /upload/storage/v1/b/bucket/o?uploadType=media&name=artifacts/test.json"
        ));
    }

    #[tokio::test]
    async fn strict_sink_fails_on_http_errors() {
        let server = test_http::OneShotServer::start("403 Forbidden", "{}");
        let sink = configured_sink(&server.base_url).with_strict(true);
        let err = sink
            .upload_bytes(b"{}".to_vec(), "artifacts/test.json", "application/json")
            .await;
        assert!(err.unwrap_err().to_string().contains("403"));
        server.request_line();
    }

    #[tokio::test]
    async fn strict_sink_succeeds_on_clean_upload() {
        let server = test_http::OneShotServer::start("200 OK", "{}");
        let sink = configured_sink(&server.base_url).with_strict(true);
        let result = sink
            .upload_bytes(b"{}".to_vec(), "artifacts/test.json", "application/json")
            .await;
        assert!(result.is_ok());
        server.request_line();
    }
}
