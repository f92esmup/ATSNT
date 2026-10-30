use std::future::Future;

/// Outbound alerting port for immediate push notifications (messaging services, webhooks).
pub trait AlertNotifier: Send + Sync {
    /// Error reported by the notification channel.
    type Error: std::fmt::Display;

    /// Dispatches an alert message to the configured notification channel.
    ///
    /// The future is `Send` so that a dedicated task can deliver alerts without the trading
    /// loop ever waiting on the messaging service.
    fn send_alert(&self, message: &str) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
