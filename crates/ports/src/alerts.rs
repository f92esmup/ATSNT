/// Outbound alerting port for immediate push notifications (Telegram, Discord, Webhooks).
pub trait AlertNotifier: Send + Sync {
    /// Error reported by the notification channel.
    type Error: std::fmt::Display;

    /// Dispatches an alert message to the configured notification channel.
    #[allow(async_fn_in_trait)]
    async fn send_alert(&self, message: &str) -> Result<(), Self::Error>;
}
