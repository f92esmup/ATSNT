//! GCP Secret Manager Client for ATSNT.
//!
//! Fetches secrets directly from Google Cloud Secret Manager via REST API
//! using credentials resolved by [`super::auth::GcpAuthResolver`].
//!
//! Supports fallback to environment variables and local `.env` file for
//! offline development and testing.

use base64::prelude::*;
use std::time::Duration;
use tracing::{debug, warn};

use super::auth::GcpAuthResolver;

/// Client for accessing Google Cloud Secret Manager secrets.
#[derive(Debug, Clone)]
pub struct GcpSecretManager {
    client: reqwest::Client,
    project_id: Option<String>,
    token: Option<String>,
}

impl Default for GcpSecretManager {
    fn default() -> Self {
        Self::from_env()
    }
}

impl GcpSecretManager {
    /// Creates a new Secret Manager client with optional explicit project and token.
    pub fn new(project_id: Option<String>, token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            project_id,
            token,
        }
    }

    /// Creates a new Secret Manager client from the environment.
    pub fn from_env() -> Self {
        let project_id = std::env::var("GCP_PROJECT_ID")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let token = std::env::var("GCP_AUTH_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::new(project_id, token)
    }

    /// Accesses a secret by name (`secret_id`) from Secret Manager.
    ///
    /// Resolves `latest` version by default.
    /// Returns the decoded plaintext secret string if found.
    pub async fn get_secret(&self, secret_id: &str) -> Option<String> {
        let project_id = match &self.project_id {
            Some(p) => Some(p.clone()),
            None => GcpAuthResolver::resolve_project_id(&self.client).await,
        };

        let token = match &self.token {
            Some(t) => Some(t.clone()),
            None => GcpAuthResolver::resolve_token(&self.client).await,
        };

        let (Some(project_id), Some(token)) = (project_id, token) else {
            debug!(
                target: "gcp_secrets",
                secret = %secret_id,
                "Secret Manager skipped: project_id or auth token not resolved"
            );
            return None;
        };

        let url = format!(
            "https://secretmanager.googleapis.com/v1/projects/{}/secrets/{}/versions/latest:access",
            project_id, secret_id
        );

        let res = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .ok()?;

        if !res.status().is_success() {
            let status = res.status();
            warn!(
                target: "gcp_secrets",
                secret = %secret_id,
                status = %status,
                "Failed to fetch secret from GCP Secret Manager"
            );
            return None;
        }

        let body: serde_json::Value = res.json().await.ok()?;
        let encoded_data = body
            .get("payload")
            .and_then(|p| p.get("data"))
            .and_then(|d| d.as_str())?;

        let decoded_bytes = BASE64_STANDARD.decode(encoded_data.trim()).ok()?;
        let secret_value = String::from_utf8(decoded_bytes).ok()?;
        let trimmed = secret_value.trim().to_string();

        if !trimmed.is_empty() {
            println!(
                "############################################################\n\
                 # [GCP :: Secret Manager] SECRET RESOLVED                  #\n\
                 # Secret:  {:<47} #\n\
                 # Project: {:<47} #\n\
                 # Status:  SUCCESS (Retrieved from Cloud)                  #\n\
                 ############################################################",
                secret_id, project_id
            );
            Some(trimmed)
        } else {
            None
        }
    }

    /// Resolves a secret by checking:
    /// 1. Google Cloud Secret Manager (if configured and reachable).
    /// 2. Explicit environment variable / `.env` fallback.
    pub async fn resolve_secret(&self, key: &str) -> Option<String> {
        // Priority 1: Google Cloud Secret Manager
        if let Some(secret) = self.get_secret(key).await {
            return Some(secret);
        }

        // Priority 2: Fallback to environment variable
        if let Ok(val) = std::env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }

        None
    }
}
