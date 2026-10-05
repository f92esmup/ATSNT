//! Cryptographic authentication and HMAC-SHA256 request signing for Binance API.
//!
//! Enforces zero-allocation byte conversion where possible and validates signatures against
//! official Binance test vectors.

use std::fmt::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use ring::hmac::{Key, Tag, HMAC_SHA256};

/// Manages Binance API credentials and calculates HMAC-SHA256 request signatures.
#[derive(Debug, Clone)]
pub struct BinanceAuth {
    api_key: String,
    secret_key: String,
}

impl BinanceAuth {
    /// Creates a new [`BinanceAuth`] with the provided API key and secret.
    pub fn new(api_key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            secret_key: secret_key.into(),
        }
    }

    /// Access the API key for `X-MBX-APIKEY` headers.
    #[inline]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Access the secret key.
    #[inline]
    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }

    /// Computes the hex-encoded HMAC-SHA256 signature for the given raw payload.
    pub fn sign(&self, payload: &str) -> String {
        let key = Key::new(HMAC_SHA256, self.secret_key.as_bytes());
        let tag: Tag = ring::hmac::sign(&key, payload.as_bytes());
        bytes_to_hex(tag.as_ref())
    }

    /// Appends current timestamp and HMAC-SHA256 signature to a query string.
    ///
    /// If `query` is empty, produces `timestamp=<ms>&signature=<hex>`.
    /// Otherwise produces `<query>&timestamp=<ms>&signature=<hex>`.
    pub fn sign_query(&self, query: &str, recv_window: Option<u64>) -> String {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut signed_payload = String::with_capacity(query.len() + 64);
        if !query.is_empty() {
            signed_payload.push_str(query);
            signed_payload.push('&');
        }

        if let Some(recv) = recv_window {
            let _ = write!(signed_payload, "recvWindow={recv}&");
        }

        let _ = write!(signed_payload, "timestamp={now_ms}");
        let signature = self.sign(&signed_payload);

        let mut final_query = String::with_capacity(signed_payload.len() + 75);
        final_query.push_str(&signed_payload);
        final_query.push_str("&signature=");
        final_query.push_str(&signature);

        final_query
    }
}

/// Converts raw bytes into a lowercase hexadecimal string without external dependencies.
fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_binance_official_hmac_test_vector() {
        // Official test vector from Binance API Spot documentation:
        // https://binance-docs.github.io/apidocs/spot/en/#signed-trade-user_data-and-margin-endpoint-security
        let secret = "NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j";
        let api_key = "vmPUZE6mv9SD5VNHk4HlWFsOr6aKE2zvsw0MuIgwCIPy6utIco14y7Ju91duEh8A";
        let auth = BinanceAuth::new(api_key, secret);

        let payload = "symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559";
        let signature = auth.sign(payload);

        assert_eq!(
            signature,
            "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
        );
    }

    #[test]
    fn test_sign_query_adds_timestamp_and_signature() {
        let auth = BinanceAuth::new("test_key", "test_secret");
        let signed = auth.sign_query("symbol=BTCUSDT", Some(5000));

        assert!(signed.starts_with("symbol=BTCUSDT&recvWindow=5000&timestamp="));
        assert!(signed.contains("&signature="));
        let parts: Vec<&str> = signed.split("&signature=").collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].len(), 64); // 32 bytes hex encoded = 64 characters
    }
}
