//! Operator alerting for unattended paper and live sessions.
//!
//! The trading loop decides *what* to say with the pure monitors below and hands the text to an
//! [`AlertQueue`], which delivers it from its own task: a slow or unreachable messaging service
//! never delays a trade.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ports::AlertNotifier;
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::alert_throttle::AlertThrottle;

/// Alerts waiting for delivery before new ones are dropped.
pub const DEFAULT_ALERT_QUEUE_CAPACITY: usize = 64;

/// Bounded, non-blocking alert outbox drained by a dedicated task.
pub struct AlertQueue {
    sender: mpsc::Sender<String>,
    dropped: Arc<AtomicU64>,
    drain: JoinHandle<()>,
}

impl AlertQueue {
    /// Starts the delivery task for `notifier`, holding at most `capacity` pending alerts.
    pub fn spawn<N: AlertNotifier + 'static>(notifier: N, capacity: usize) -> Self {
        let (sender, mut receiver) = mpsc::channel::<String>(capacity.max(1));
        let drain = tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                if let Err(error) = notifier.send_alert(&message).await {
                    tracing::warn!(target: "telegram", %error, "Alert could not be delivered");
                }
            }
        });
        Self {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
            drain,
        }
    }

    /// Queues `message` without waiting. When the queue is full the message is dropped and
    /// counted, so a stalled messaging service can never hold the caller.
    pub fn send(&self, message: impl Into<String>) {
        if self.sender.try_send(message.into()).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(target: "telegram", dropped, "Alert queue full; alert dropped");
        }
    }

    /// Alerts dropped so far because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stops accepting alerts and gives the pending ones up to `grace` to be delivered.
    pub async fn shutdown(self, grace: Duration) {
        let Self { sender, drain, .. } = self;
        drop(sender);
        if tokio::time::timeout(grace, drain).await.is_err() {
            tracing::warn!(target: "telegram", "Pending alerts were not delivered before shutdown");
        }
    }
}

/// Market feed state worth telling an operator about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedStatus {
    /// No trade for too long, or the connection keeps failing.
    Down {
        silent_secs: i64,
        failed_reconnects: u32,
    },
    /// Trades flow again after a reported outage.
    Recovered { silent_secs: i64 },
}

impl FeedStatus {
    /// Telegram text for this status.
    pub fn message(&self, symbol: &str) -> String {
        match self {
            Self::Down {
                silent_secs,
                failed_reconnects,
            } => format!(
                "⚠️ *ATSNT FEED DOWN* `{symbol}`: no market trade for {silent_secs} s, \
                 {failed_reconnects} failed reconnect(s) in a row. No new signals until it recovers."
            ),
            Self::Recovered { silent_secs } => format!(
                "✅ *ATSNT FEED BACK* `{symbol}`: market trades flow again after {silent_secs} s."
            ),
        }
    }
}

/// Decides when a quiet or failing market feed must be reported.
///
/// Down after more than [`FeedMonitor::STALE_AFTER_MS`] without a trade or after
/// [`FeedMonitor::FAILED_RECONNECTS`] failed reconnects in a row; repeated at most once per
/// [`FeedMonitor::REPEAT_MS`]; the first trade afterwards reports the recovery once.
#[derive(Debug)]
pub struct FeedMonitor {
    last_trade_ms: i64,
    down: bool,
    repeats: AlertThrottle,
}

impl FeedMonitor {
    /// Silence that counts as an outage.
    pub const STALE_AFTER_MS: i64 = 60_000;
    /// Failed reconnects in a row that count as an outage.
    pub const FAILED_RECONNECTS: u32 = 3;
    /// Shortest interval between two outage alerts.
    pub const REPEAT_MS: i64 = 600_000;

    /// A monitor whose feed counts as fresh at `now_ms` (session start).
    pub fn new(now_ms: i64) -> Self {
        Self {
            last_trade_ms: now_ms,
            down: false,
            repeats: AlertThrottle::new(Self::REPEAT_MS),
        }
    }

    /// Records a market trade received at `now_ms`.
    pub fn on_trade(&mut self, now_ms: i64) -> Option<FeedStatus> {
        let silent_secs = (now_ms - self.last_trade_ms) / 1_000;
        self.last_trade_ms = now_ms;
        if !self.down {
            return None;
        }
        self.down = false;
        self.repeats.reset();
        Some(FeedStatus::Recovered { silent_secs })
    }

    /// Periodic check at `now_ms` with the adapter's count of failed reconnects in a row.
    pub fn on_check(&mut self, now_ms: i64, failed_reconnects: u32) -> Option<FeedStatus> {
        let silent_ms = now_ms - self.last_trade_ms;
        if silent_ms <= Self::STALE_AFTER_MS && failed_reconnects < Self::FAILED_RECONNECTS {
            return None;
        }
        self.down = true;
        self.repeats.allow(now_ms).then_some(FeedStatus::Down {
            silent_secs: silent_ms / 1_000,
            failed_reconnects,
        })
    }
}

/// Remembers which rejected orders were already reported.
#[derive(Debug, Default)]
pub struct RejectionAlerts {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl RejectionAlerts {
    /// Order ids remembered; older ones are forgotten first.
    const CAPACITY: usize = 256;

    /// Whether `order_id` is reported for the first time (and remembers it).
    pub fn first(&mut self, order_id: &str) -> bool {
        if !self.seen.insert(order_id.to_string()) {
            return false;
        }
        self.order.push_back(order_id.to_string());
        if self.order.len() > Self::CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }
}

/// Interval between two heartbeats.
pub const HEARTBEAT_INTERVAL_MS: i64 = 12 * 60 * 60 * 1_000;

/// Telegram text of the periodic heartbeat: the session is alive and what it holds.
pub fn heartbeat_message(
    symbol: &str,
    equity: Decimal,
    position: &str,
    last_tick_age_secs: Option<i64>,
) -> String {
    let last_tick = match last_tick_age_secs {
        Some(age) => format!("{age} s ago"),
        None => "no trade received yet".to_string(),
    };
    format!(
        "💓 *ATSNT HEARTBEAT* `{symbol}`\n\
         • *Equity*: `${equity:.2}`\n\
         • *Position*: {position}\n\
         • *Last market trade*: {last_tick}"
    )
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use ports::AlertNotifier;
    use rust_decimal_macros::dec;

    use super::*;

    /// Records every alert it is asked to send; fails the ones listed in `fail`.
    #[derive(Clone, Default)]
    struct Recorder {
        sent: Arc<Mutex<Vec<String>>>,
        fail: Vec<String>,
    }

    impl AlertNotifier for Recorder {
        type Error = String;

        fn send_alert(&self, message: &str) -> impl Future<Output = Result<(), String>> + Send {
            let sent = Arc::clone(&self.sent);
            let fails = self.fail.iter().any(|f| f == message);
            let message = message.to_string();
            async move {
                if fails {
                    return Err("channel down".to_string());
                }
                sent.lock().expect("recorder lock").push(message);
                Ok(())
            }
        }
    }

    /// A messaging service that never answers.
    struct Hung;

    impl AlertNotifier for Hung {
        type Error = String;

        fn send_alert(&self, _message: &str) -> impl Future<Output = Result<(), String>> + Send {
            std::future::pending()
        }
    }

    #[tokio::test]
    async fn queued_alerts_are_delivered_in_order() {
        let recorder = Recorder::default();
        let queue = AlertQueue::spawn(recorder.clone(), 8);

        queue.send("first");
        queue.send("second");
        queue.shutdown(Duration::from_secs(1)).await;

        assert_eq!(
            *recorder.sent.lock().expect("lock"),
            vec!["first", "second"]
        );
    }

    #[tokio::test]
    async fn a_hung_channel_never_blocks_the_sender_and_overflow_is_dropped_and_counted() {
        let queue = AlertQueue::spawn(Hung, 2);

        // No await between sends: the drain task has not taken anything yet.
        for i in 0..5 {
            queue.send(format!("alert {i}"));
        }

        assert_eq!(queue.dropped(), 3);
    }

    #[tokio::test]
    async fn a_failed_delivery_does_not_stop_later_alerts() {
        let recorder = Recorder {
            fail: vec!["broken".to_string()],
            ..Recorder::default()
        };
        let queue = AlertQueue::spawn(recorder.clone(), 8);

        queue.send("broken");
        queue.send("after");
        queue.shutdown(Duration::from_secs(1)).await;

        assert_eq!(*recorder.sent.lock().expect("lock"), vec!["after"]);
    }

    #[test]
    fn a_flowing_feed_raises_nothing() {
        let mut feed = FeedMonitor::new(0);

        assert_eq!(feed.on_trade(30_000), None);
        assert_eq!(feed.on_check(80_000, 0), None);
    }

    #[test]
    fn more_than_sixty_seconds_without_trades_alerts_once_per_ten_minutes() {
        let mut feed = FeedMonitor::new(0);

        assert_eq!(feed.on_check(60_000, 0), None);
        assert_eq!(
            feed.on_check(60_001, 0),
            Some(FeedStatus::Down {
                silent_secs: 60,
                failed_reconnects: 0
            })
        );
        assert_eq!(feed.on_check(600_000, 0), None);
        assert!(matches!(
            feed.on_check(660_001, 0),
            Some(FeedStatus::Down {
                silent_secs: 660,
                ..
            })
        ));
    }

    #[test]
    fn three_failed_reconnects_alert_before_the_sixty_second_gap() {
        let mut feed = FeedMonitor::new(0);

        assert_eq!(feed.on_check(5_000, 2), None);
        assert_eq!(
            feed.on_check(6_000, 3),
            Some(FeedStatus::Down {
                silent_secs: 6,
                failed_reconnects: 3
            })
        );
    }

    #[test]
    fn the_first_trade_after_an_outage_reports_the_recovery_once() {
        let mut feed = FeedMonitor::new(0);
        assert!(feed.on_check(90_000, 0).is_some());

        assert_eq!(
            feed.on_trade(120_000),
            Some(FeedStatus::Recovered { silent_secs: 120 })
        );
        assert_eq!(feed.on_trade(121_000), None);
        // The repeat limit restarts: a new outage alerts at once.
        assert!(feed.on_check(182_000, 0).is_some());
    }

    #[test]
    fn feed_messages_name_the_symbol_and_the_outage() {
        let down = FeedStatus::Down {
            silent_secs: 75,
            failed_reconnects: 4,
        }
        .message("BTCUSDT");
        assert!(down.contains("BTCUSDT") && down.contains("75 s") && down.contains("4"));

        let back = FeedStatus::Recovered { silent_secs: 120 }.message("BTCUSDT");
        assert!(back.contains("BTCUSDT") && back.contains("120 s"));
    }

    #[test]
    fn a_rejected_order_alerts_once_per_order_id() {
        let mut rejections = RejectionAlerts::default();

        assert!(rejections.first("42"));
        assert!(!rejections.first("42"));
        assert!(rejections.first("43"));
    }

    #[test]
    fn the_heartbeat_carries_equity_position_and_last_tick_age() {
        let message = heartbeat_message("BTCUSDT", dec!(512.34), "Flat", Some(3));

        assert!(message.contains("BTCUSDT"));
        assert!(message.contains("512.34"));
        assert!(message.contains("Flat"));
        assert!(message.contains("3 s"));

        let no_tick = heartbeat_message("BTCUSDT", dec!(500), "Flat", None);
        assert!(no_tick.contains("no trade received yet"));
    }
}
