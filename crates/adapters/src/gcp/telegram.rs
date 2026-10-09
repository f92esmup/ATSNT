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

    /// Initializes from environment variables with alias fallbacks (`TELEGRAM_BOT_TOKEN`, `BOT_TOKEN`, `TELEGRAM_CHAT_ID`, `CHAT_ID`).
    pub fn from_env() -> Self {
        let token = std::env::var("TELEGRAM_BOT_TOKEN")
            .or_else(|_| std::env::var("BOT_TOKEN"))
            .ok()
            .filter(|s| !s.trim().is_empty());
        let chat = std::env::var("TELEGRAM_CHAT_ID")
            .or_else(|_| std::env::var("CHAT_ID"))
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::new(token, chat)
    }

    /// Initializes from Google Cloud Secret Manager with automatic fallback to environment variables.
    pub async fn from_gcp_or_env(secret_mgr: &crate::gcp::GcpSecretManager) -> Self {
        let token = match secret_mgr.resolve_secret("TELEGRAM_BOT_TOKEN").await {
            Some(t) => Some(t),
            None => secret_mgr.resolve_secret("BOT_TOKEN").await,
        };
        let chat = match secret_mgr.resolve_secret("TELEGRAM_CHAT_ID").await {
            Some(c) => Some(c),
            None => secret_mgr.resolve_secret("CHAT_ID").await,
        };
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
             ━━━━━━━━━━━━━━━━━━━━━━\n\
             • *Instrument*: `{}`\n\
             • *Side*: `{}`\n\
             • *Entry Price*: `${}`\n\
             • *Quantity*: `{}`\n\
             • *Stop Loss*: `${}`\n\
             • *Take Profit*: `${}`\n\
             • *Cash Equity*: `${:.2}`\n\
             ━━━━━━━━━━━━━━━━━━━━━━",
            symbol.to_uppercase(),
            side.to_uppercase(),
            entry_price,
            quantity,
            stop_loss,
            take_profit,
            equity
        )
    }

    /// Formats a comprehensive trade summary notification when a position closes.
    #[allow(clippy::too_many_arguments)]
    pub fn format_trade_summary(
        symbol: &str,
        side: &str,
        exit_reason: &str,
        entry_price: Decimal,
        exit_price: Decimal,
        quantity: Decimal,
        duration_seconds: i64,
        gross_pnl: Decimal,
        fees_paid: Decimal,
        net_pnl: Decimal,
        return_pct: Decimal,
        total_equity: Decimal,
    ) -> String {
        let emoji = if net_pnl > Decimal::ZERO {
            "🎯 [WIN]"
        } else if net_pnl < Decimal::ZERO {
            "🛑 [LOSS]"
        } else {
            "⚪ [BREAKEVEN]"
        };

        let duration_str = if duration_seconds >= 3600 {
            format!(
                "{}h {}m",
                duration_seconds / 3600,
                (duration_seconds % 3600) / 60
            )
        } else if duration_seconds >= 60 {
            format!("{}m {}s", duration_seconds / 60, duration_seconds % 60)
        } else {
            format!("{}s", duration_seconds)
        };

        let pnl_sign = if net_pnl >= Decimal::ZERO { "+" } else { "" };
        let ret_sign = if return_pct >= Decimal::ZERO { "+" } else { "" };

        format!(
            "{} *ATSNT: Trade Closed ({})*\n\
             ━━━━━━━━━━━━━━━━━━━━━━\n\
             • *Instrument*: `{}`\n\
             • *Direction*: `{}`\n\
             • *Exit Reason*: `{}`\n\
             • *Entry Price*: `${}`\n\
             • *Exit Price*: `${}`\n\
             • *Quantity*: `{}`\n\
             • *Duration*: `{}`\n\
             ──────────────────────\n\
             • *Gross PnL*: `${:.2}`\n\
             • *Fees & Friction*: `${:.2}`\n\
             • *Realized Net PnL*: `{}{:.2} USD` ({}{:.2}%)\n\
             • *Current Equity*: `${:.2}`\n\
             ━━━━━━━━━━━━━━━━━━━━━━",
            emoji,
            symbol.to_uppercase(),
            symbol.to_uppercase(),
            side.to_uppercase(),
            exit_reason,
            entry_price,
            exit_price,
            quantity,
            duration_str,
            gross_pnl,
            fees_paid,
            pnl_sign,
            net_pnl,
            ret_sign,
            return_pct * Decimal::from(100),
            total_equity
        )
    }

    /// Formats a basic position closed notification (maintained for compatibility).
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

    /// Formats the daily 8:00 PM Madrid executive performance summary.
    #[allow(clippy::too_many_arguments)]
    pub fn format_daily_summary(
        date_str: &str,
        mode: &str,
        symbol: &str,
        trades_count: usize,
        winning_trades: usize,
        losing_trades: usize,
        gross_pnl: Decimal,
        fees_paid: Decimal,
        net_pnl: Decimal,
        cash_equity: Decimal,
        total_equity: Decimal,
        drawdown_pct: Decimal,
        active_position_str: &str,
        total_ticks: u64,
        total_bars: u64,
    ) -> String {
        let win_rate = if trades_count > 0 {
            (Decimal::from(winning_trades) / Decimal::from(trades_count)) * Decimal::from(100)
        } else {
            Decimal::ZERO
        };
        let pnl_sign = if net_pnl >= Decimal::ZERO { "+" } else { "" };
        let gross_sign = if gross_pnl >= Decimal::ZERO { "+" } else { "" };
        let emoji = if net_pnl >= Decimal::ZERO {
            "📈"
        } else {
            "📉"
        };

        format!(
            "{} *ATSNT: Daily Performance Summary (20:00 Madrid)*\n\
             ━━━━━━━━━━━━━━━━━━━━━━\n\
             📅 *Date*: `{}`\n\
             🚀 *Execution Mode*: `{}`\n\
             🏷 *Instrument*: `{}`\n\
             ━━━━━━━━━━━━━━━━━━━━━━\n\
             📊 *Trades Today*: `{}`\n\
             • *Wins*: `{}` ({:.1}%)\n\
             • *Losses*: `{}`\n\
             • *Gross PnL*: `{}{:.2} USD`\n\
             • *Fees & Friction*: `-${:.2}`\n\
             • *Net Realized PnL*: `{}{:.2} USD`\n\
             ──────────────────────\n\
             💼 *Account & Risk*:\n\
             • *Cash Balance*: `${:.2}`\n\
             • *Mark-to-Market*: `${:.2}`\n\
             • *Session Drawdown*: `{:.2}%`\n\
             • *Position*: `{}`\n\
             ──────────────────────\n\
             ⚙️ *Engine Telemetry*:\n\
             • *Ticks Processed*: `{}`\n\
             • *Dollar Bars Formed*: `{}`\n\
             ━━━━━━━━━━━━━━━━━━━━━━",
            emoji,
            date_str,
            mode,
            symbol.to_uppercase(),
            trades_count,
            winning_trades,
            win_rate,
            losing_trades,
            gross_sign,
            gross_pnl,
            fees_paid,
            pnl_sign,
            net_pnl,
            cash_equity,
            total_equity,
            drawdown_pct * Decimal::from(100),
            active_position_str,
            total_ticks,
            total_bars
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

/// Converts a UTC `DateTime` into `Europe/Madrid` timezone (CET UTC+1 or CEST UTC+2).
pub fn datetime_in_madrid(
    utc: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::FixedOffset> {
    use chrono::{Datelike, FixedOffset, NaiveDate, Utc};

    let year = utc.year();
    let month = utc.month();

    // EU Daylight Saving Time rules:
    // Starts last Sunday of March at 01:00 UTC (advances to UTC+2 / CEST).
    // Ends last Sunday of October at 01:00 UTC (reverts to UTC+1 / CET).
    let is_dst = if !(3..=10).contains(&month) {
        false
    } else if month > 3 && month < 10 {
        true
    } else if month == 3 {
        let last_sunday_day = last_sunday_of_month(year, 3);
        let transition = NaiveDate::from_ymd_opt(year, 3, last_sunday_day)
            .and_then(|d| d.and_hms_opt(1, 0, 0))
            .map(|dt| chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
        match transition {
            Some(t) => utc >= t,
            None => false,
        }
    } else {
        // month == 10
        let last_sunday_day = last_sunday_of_month(year, 10);
        let transition = NaiveDate::from_ymd_opt(year, 10, last_sunday_day)
            .and_then(|d| d.and_hms_opt(1, 0, 0))
            .map(|dt| chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
        match transition {
            Some(t) => utc < t,
            None => true,
        }
    };

    let offset_secs = if is_dst { 2 * 3600 } else { 3600 };
    let offset =
        FixedOffset::east_opt(offset_secs).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    utc.with_timezone(&offset)
}

/// Returns the current date and time in the `Europe/Madrid` timezone (CET / CEST).
pub fn now_in_madrid() -> chrono::DateTime<chrono::FixedOffset> {
    datetime_in_madrid(chrono::Utc::now())
}

fn last_sunday_of_month(year: i32, month: u32) -> u32 {
    use chrono::{Datelike, NaiveDate};
    let d31 = NaiveDate::from_ymd_opt(year, month, 31).unwrap_or_default();
    let days_from_sunday = d31.weekday().num_days_from_sunday();
    31 - days_from_sunday
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
    use chrono::{Datelike, TimeZone, Utc};
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

    #[test]
    fn format_trade_summary_contains_friction_and_pnl() {
        let msg = TelegramNotifier::format_trade_summary(
            "btcusdt",
            "Long",
            "TakeProfit",
            dec!(65000),
            dec!(67000),
            dec!(0.5),
            125,
            dec!(1000),
            dec!(15.50),
            dec!(984.50),
            dec!(0.0985),
            dec!(10984.50),
        );
        assert!(msg.contains("🎯 [WIN]"));
        assert!(msg.contains("BTCUSDT"));
        assert!(msg.contains("TakeProfit"));
        assert!(msg.contains("65000"));
        assert!(msg.contains("67000"));
        assert!(msg.contains("2m 5s"));
        assert!(msg.contains("1000.00"));
        assert!(msg.contains("15.50"));
        assert!(msg.contains("+984.50 USD"));
        assert!(msg.contains("+9.85%"));
    }

    #[test]
    fn format_daily_summary_contains_executive_metrics() {
        let msg = TelegramNotifier::format_daily_summary(
            "2026-10-09",
            "Paper Trading",
            "btcusdt",
            5,
            4,
            1,
            dec!(1200),
            dec!(45.20),
            dec!(1154.80),
            dec!(11154.80),
            dec!(11154.80),
            dec!(0.015),
            "Flat (No open position)",
            15420,
            48,
        );
        assert!(msg.contains("Daily Performance Summary (20:00 Madrid)"));
        assert!(msg.contains("2026-10-09"));
        assert!(msg.contains("Paper Trading"));
        assert!(msg.contains("Wins*: `4` (80.0%)"));
        assert!(msg.contains("+1154.80 USD"));
        assert!(msg.contains("1.50%"));
        assert!(msg.contains("15420"));
        assert!(msg.contains("48"));
    }

    #[test]
    fn test_madrid_timezone_conversion() {
        // Winter time: January 15, 2026 12:00 UTC -> CET (UTC+1) = 13:00
        let winter_utc = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let winter_madrid = datetime_in_madrid(winter_utc);
        assert_eq!(winter_madrid.offset().local_minus_utc(), 3600);

        // Summer time: July 15, 2026 12:00 UTC -> CEST (UTC+2) = 14:00
        let summer_utc = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        let summer_madrid = datetime_in_madrid(summer_utc);
        assert_eq!(summer_madrid.offset().local_minus_utc(), 7200);

        // Current Madrid time can be queried without panic
        let now = now_in_madrid();
        assert!(now.year() >= 2026);
    }
}
