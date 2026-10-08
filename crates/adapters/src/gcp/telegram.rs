//! Telegram Bot Alerting Adapter for ATSNT.
//!
//! Provides outbound push notifications for real-time execution events, Stop Loss / Take Profit
//! triggers, and risk circuit breakers directly to mobile devices.

use rust_decimal::Decimal;
use serde_json::json;
use tracing::{info, warn};

use crate::error::AdapterError;
use crate::traits::AlertNotifier;

/// Telegram bot alert notifier.
///
/// Sends formatted markdown notifications to a Telegram chat/channel via the official Bot API.
/// If `bot_token` or `chat_id` are not configured, calls succeed gracefully with a tracing log
/// to prevent halting simulation or trading loops.
#[derive(Debug, Clone)]
pub struct TelegramNotifier {
    client: reqwest::Client,
    bot_token: Option<String>,
    chat_id: Option<String>,
}

impl TelegramNotifier {
    /// Creates a new notifier with explicit credentials.
    pub fn new(bot_token: Option<String>, chat_id: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            bot_token,
            chat_id,
        }
    }

    /// Initializes from environment variables `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID`.
    pub fn from_env() -> Self {
        let token = std::env::var("TELEGRAM_BOT_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let chat = std::env::var("TELEGRAM_CHAT_ID")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::new(token, chat)
    }

    /// Returns `true` if valid credentials are configured.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.bot_token.is_some() && self.chat_id.is_some()
    }

    /// Formats a position opened execution notification.
    pub fn format_position_opened(
        symbol: &str,
        side: &str,
        entry_price: Decimal,
        quantity: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
        equity: Decimal,
    ) -> String {
        format!(
            "🟢 *ATSNT: Position Opened*\n\
             • *Instrument*: `{}`\n\
             • *Side*: `{}`\n\
             • *Entry Price*: `${}`\n\
             • *Quantity*: `{}`\n\
             • *Stop Loss*: `${}`\n\
             • *Take Profit*: `${}`\n\
             • *Cash Equity*: `${:.2}`",
            symbol.to_uppercase(),
            side.to_uppercase(),
            entry_price,
            quantity,
            stop_loss,
            take_profit,
            equity
        )
    }

    /// Formats a position closed (barrier exit) notification.
    pub fn format_position_closed(
        symbol: &str,
        exit_reason: &str,
        exit_price: Decimal,
        net_pnl: Decimal,
        total_equity: Decimal,
    ) -> String {
        let emoji = if net_pnl >= Decimal::ZERO {
            "🎯"
        } else {
            "🛑"
        };
        format!(
            "{} *ATSNT: Position Closed*\n\
             • *Instrument*: `{}`\n\
             • *Exit Reason*: `{}`\n\
             • *Exit Price*: `${}`\n\
             • *Realized Net PnL*: `${:.2}`\n\
             • *Total Equity*: `${:.2}`",
            emoji,
            symbol.to_uppercase(),
            exit_reason,
            exit_price,
            net_pnl,
            total_equity
        )
    }

    /// Formats a circuit breaker risk warning.
    pub fn format_circuit_breaker(
        symbol: &str,
        current_drawdown_pct: Decimal,
        limit_pct: Decimal,
    ) -> String {
        format!(
            "🚨 *ATSNT RISK ALERT: Circuit Breaker Triggered*\n\
             • *Instrument*: `{}`\n\
             • *Current Drawdown*: `{:.2}%`\n\
             • *Drawdown Limit*: `{:.2}%`\n\
             • *Action*: All trading activity halted immediately.",
            symbol.to_uppercase(),
            current_drawdown_pct,
            limit_pct
        )
    }
}

impl AlertNotifier for TelegramNotifier {
    async fn send_alert(&self, message: &str) -> Result<(), AdapterError> {
        let (Some(token), Some(chat_id)) = (&self.bot_token, &self.chat_id) else {
            info!(target: "telegram", "Alert omitted (Telegram credentials not set): {}", message);
            return Ok(());
        };

        let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
        let payload = json!({
            "chat_id": chat_id,
            "text": message,
            "parse_mode": "Markdown",
            "disable_web_page_preview": true
        });

        let response = self.client.post(&url).json(&payload).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            warn!(
                target: "telegram",
                status = %status,
                body = %body,
                "Telegram notification dispatch returned non-success"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn disabled_notifier_does_not_error() {
        let notifier = TelegramNotifier::new(None, None);
        assert!(!notifier.is_enabled());
        let res = notifier.send_alert("test message").await;
        assert!(res.is_ok());
    }

    #[test]
    fn format_position_opened_contains_all_fields() {
        let msg = TelegramNotifier::format_position_opened(
            "btcusdt",
            "Long",
            dec!(65000),
            dec!(0.15),
            dec!(63500),
            dec!(68000),
            dec!(10000),
        );
        assert!(msg.contains("BTCUSDT"));
        assert!(msg.contains("LONG"));
        assert!(msg.contains("65000"));
        assert!(msg.contains("63500"));
        assert!(msg.contains("68000"));
    }

    #[test]
    fn format_position_closed_shows_correct_emojis() {
        let msg_profit = TelegramNotifier::format_position_closed(
            "btcusdt",
            "TakeProfit",
            dec!(68000),
            dec!(450),
            dec!(10450),
        );
        assert!(msg_profit.contains("🎯"));
        assert!(msg_profit.contains("TakeProfit"));

        let msg_loss = TelegramNotifier::format_position_closed(
            "btcusdt",
            "StopLoss",
            dec!(63500),
            dec!(-225),
            dec!(9775),
        );
        assert!(msg_loss.contains("🛑"));
        assert!(msg_loss.contains("StopLoss"));
    }
}
