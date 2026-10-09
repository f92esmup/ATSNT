//! GCP Authentication & Token Cascade Resolver for ATSNT.
//!
//! Provides transparent, multi-tier OAuth2 access token and project ID resolution:
//! 1. Static environment variables (`GCP_AUTH_TOKEN`, `GCP_PROJECT_ID`).
//! 2. GCE Metadata Server (for zero-configuration 24/7 execution inside GCP Compute Engine VMs).
//! 3. Local Developer CLI Fallback (`gcloud auth print-access-token`, `gcloud config get-value project`).
//! 4. Fail-Open graceful fallback (offline safe, logs with `tracing` without crashing).

use std::time::Duration;
use tracing::{debug, info};

/// Metadata Server URL for instance default service account access token.
const GCE_METADATA_TOKEN_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";

/// Metadata Server URL for GCP project ID.
const GCE_METADATA_PROJECT_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/project/project-id";

/// JSON payload structure returned by the GCE metadata server token endpoint.
#[derive(Debug, serde::Deserialize)]
struct MetadataTokenResponse {
    access_token: String,
}

/// Cascade credential and project resolver for GCP cloud-first telemetry.
#[derive(Debug, Clone, Default)]
pub struct GcpAuthResolver;

impl GcpAuthResolver {
    /// Resolves an active GCP OAuth2 access token through the multi-tier cascade.
    pub async fn resolve_token(client: &reqwest::Client) -> Option<String> {
        // Tier 1: Explicit environment variable (CI/CD, manual overrides)
        if let Ok(token) = std::env::var("GCP_AUTH_TOKEN") {
            let trimmed = token.trim();
            if !trimmed.is_empty() {
                debug!(target: "gcp_auth", "GCP access token resolved via GCP_AUTH_TOKEN env");
                return Some(trimmed.to_string());
            }
        }

        // Tier 2: GCE Metadata Server (Compute Engine e2-micro VM)
        if let Some(token) = Self::fetch_metadata_token(client).await {
            debug!(target: "gcp_auth", "GCP access token resolved via GCE Metadata Server");
            return Some(token);
        }

        // Tier 3: Local workstation gcloud CLI fallback
        if let Some(token) = Self::fetch_gcloud_token().await {
            debug!(target: "gcp_auth", "GCP access token resolved via local gcloud CLI");
            return Some(token);
        }

        info!(
            target: "gcp_auth",
            "No GCP access token could be resolved; proceeding in offline/fail-open mode"
        );
        None
    }

    /// Resolves the target GCP Project ID through the multi-tier cascade.
    pub async fn resolve_project_id(client: &reqwest::Client) -> Option<String> {
        // Tier 1: Explicit environment variable
        if let Ok(proj) = std::env::var("GCP_PROJECT_ID") {
            let trimmed = proj.trim();
            if !trimmed.is_empty() && trimmed != "(unset)" {
                return Some(trimmed.to_string());
            }
        }

        // Tier 2: GCE Metadata Server
        if let Some(proj) = Self::fetch_metadata_project(client).await {
            return Some(proj);
        }

        // Tier 3: Local gcloud CLI config
        if let Some(proj) = Self::fetch_gcloud_project().await {
            return Some(proj);
        }

        None
    }

    /// Queries the GCE Metadata Server for an access token with a fast timeout.
    async fn fetch_metadata_token(client: &reqwest::Client) -> Option<String> {
        let res = client
            .get(GCE_METADATA_TOKEN_URL)
            .header("Metadata-Flavor", "Google")
            .timeout(Duration::from_millis(600))
            .send()
            .await
            .ok()?;

        if res.status().is_success() {
            let body: MetadataTokenResponse = res.json().await.ok()?;
            let token = body.access_token.trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
        None
    }

    /// Queries the GCE Metadata Server for the instance's GCP Project ID.
    async fn fetch_metadata_project(client: &reqwest::Client) -> Option<String> {
        let res = client
            .get(GCE_METADATA_PROJECT_URL)
            .header("Metadata-Flavor", "Google")
            .timeout(Duration::from_millis(600))
            .send()
            .await
            .ok()?;

        if res.status().is_success() {
            let proj = res.text().await.ok()?.trim().to_string();
            if !proj.is_empty() {
                return Some(proj);
            }
        }
        None
    }

    /// Invokes `gcloud auth print-access-token` in a blocking thread.
    async fn fetch_gcloud_token() -> Option<String> {
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("gcloud")
                .args(["auth", "print-access-token"])
                .output()
                .ok()
                .and_then(|out| {
                    if out.status.success() {
                        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if !token.is_empty() {
                            Some(token)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
        })
        .await
        .ok()
        .flatten()
    }

    /// Invokes `gcloud config get-value project` in a blocking thread.
    async fn fetch_gcloud_project() -> Option<String> {
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("gcloud")
                .args(["config", "get-value", "project"])
                .output()
                .ok()
                .and_then(|out| {
                    if out.status.success() {
                        let proj = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if !proj.is_empty() && proj != "(unset)" {
                            Some(proj)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
        })
        .await
        .ok()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolver_returns_explicit_env_token() {
        std::env::set_var("GCP_AUTH_TOKEN", "test_token_123");
        let client = reqwest::Client::new();
        let token = GcpAuthResolver::resolve_token(&client).await;
        assert_eq!(token, Some("test_token_123".to_string()));
        std::env::remove_var("GCP_AUTH_TOKEN");
    }

    #[tokio::test]
    async fn resolver_returns_explicit_env_project() {
        std::env::set_var("GCP_PROJECT_ID", "test-project-456");
        let client = reqwest::Client::new();
        let proj = GcpAuthResolver::resolve_project_id(&client).await;
        assert_eq!(proj, Some("test-project-456".to_string()));
        std::env::remove_var("GCP_PROJECT_ID");
    }
}
