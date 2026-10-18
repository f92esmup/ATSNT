//! USD-M user-data-stream listen-key lifecycle: creation, keep-alive and cleanup.

use super::*;
use crate::binance_private_stream::ListenKeyRenewer;

const USER_STREAM_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl BinanceGateway {
    /// Creates a new User Data Stream `listenKey` for streaming execution reports.
    pub async fn create_listen_key(&self) -> Result<String, GatewayError> {
        if self.config.spot {
            return Err(GatewayError::Config(
                "Spot user streams use the signed WebSocket API subscription".to_string(),
            ));
        }
        let path = "/fapi/v1/listenKey";
        let url = format!("{}{path}", self.base_url);

        let resp = tokio::time::timeout(
            USER_STREAM_HTTP_TIMEOUT,
            self.client
                .post(&url)
                .timeout(USER_STREAM_HTTP_TIMEOUT)
                .send(),
        )
        .await
        .map_err(|_| GatewayError::Config("USD-M listen-key creation timed out".to_string()))?
        .map_err(|_| GatewayError::Config("USD-M listen-key creation failed".to_string()))?;
        if !resp.status().is_success() {
            return Err(GatewayError::Config(
                "USD-M listen-key creation failed".to_string(),
            ));
        }
        let body: serde_json::Value = resp.json().await.map_err(|_| {
            GatewayError::Config("USD-M listen-key response was malformed".to_string())
        })?;

        let listen_key = body
            .get("listenKey")
            .and_then(|k| k.as_str())
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                GatewayError::Config("USD-M response omitted its listen key".to_string())
            })?
            .to_string();

        Ok(listen_key)
    }

    /// Pings the `listenKey` to prevent expiration (should be called every ~30 minutes).
    pub async fn keep_alive_listen_key(&self, listen_key: &str) -> Result<(), GatewayError> {
        if self.config.spot || listen_key.is_empty() || listen_key.chars().any(char::is_control) {
            return Err(GatewayError::Config(
                "USD-M listen-key renewal configuration is invalid".to_string(),
            ));
        }
        let path = "/fapi/v1/listenKey";
        let url = format!("{}{path}", self.base_url);

        let resp = tokio::time::timeout(
            USER_STREAM_HTTP_TIMEOUT,
            self.client
                .put(&url)
                .query(&[("listenKey", listen_key)])
                .timeout(USER_STREAM_HTTP_TIMEOUT)
                .send(),
        )
        .await
        .map_err(|_| GatewayError::Config("USD-M listen-key renewal timed out".to_string()))?
        .map_err(|_| GatewayError::Config("USD-M listen-key renewal failed".to_string()))?;
        if !resp.status().is_success() {
            return Err(GatewayError::Config(
                "USD-M listen-key renewal failed".to_string(),
            ));
        }
        let body: serde_json::Value = resp.json().await.map_err(|_| {
            GatewayError::Config("USD-M listen-key renewal response was malformed".to_string())
        })?;
        if !body.is_object() || body.get("code").is_some() || body.get("msg").is_some() {
            return Err(GatewayError::Config(
                "USD-M listen-key renewal response was invalid".to_string(),
            ));
        }
        Ok(())
    }

    /// Closes a USD-M listen key during private-stream shutdown.
    pub async fn close_listen_key(&self, listen_key: &str) -> Result<(), GatewayError> {
        if self.config.spot || listen_key.is_empty() || listen_key.chars().any(char::is_control) {
            return Err(GatewayError::Config(
                "USD-M listen-key cleanup configuration is invalid".to_string(),
            ));
        }
        let url = format!("{}/fapi/v1/listenKey", self.base_url);
        let response = tokio::time::timeout(
            USER_STREAM_HTTP_TIMEOUT,
            self.client
                .delete(&url)
                .query(&[("listenKey", listen_key)])
                .timeout(USER_STREAM_HTTP_TIMEOUT)
                .send(),
        )
        .await
        .map_err(|_| GatewayError::Config("USD-M listen-key cleanup timed out".to_string()))?
        .map_err(|_| GatewayError::Config("USD-M listen-key cleanup failed".to_string()))?;
        if !response.status().is_success() {
            return Err(GatewayError::Config(
                "USD-M listen-key cleanup failed".to_string(),
            ));
        }
        Ok(())
    }
}

impl ListenKeyRenewer for BinanceGateway {
    fn renew<'a>(
        &'a self,
        listen_key: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + 'a>> {
        Box::pin(async move { self.keep_alive_listen_key(listen_key).await.map_err(|_| ()) })
    }

    fn close<'a>(
        &'a self,
        listen_key: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + 'a>> {
        Box::pin(async move { self.close_listen_key(listen_key).await.map_err(|_| ()) })
    }
}
