//! Rate limit for repeating operator alerts.

use std::cell::Cell;

/// Lets an alert through at most once per interval, however often its cause repeats.
///
/// A failure that persists (an exchange outage, say) is retried every few seconds; without a
/// limit each retry would send another message and wait on the messaging service inside the
/// trading loop.
#[derive(Debug)]
pub struct AlertThrottle {
    interval_ms: i64,
    last_sent_ms: Cell<Option<i64>>,
}

impl AlertThrottle {
    /// A throttle that allows one alert per `interval_ms`.
    pub fn new(interval_ms: i64) -> Self {
        Self {
            interval_ms,
            last_sent_ms: Cell::new(None),
        }
    }

    /// Whether an alert may be sent at `now_ms`; when it may, the interval restarts.
    pub fn allow(&self, now_ms: i64) -> bool {
        let due = match self.last_sent_ms.get() {
            None => true,
            Some(last) => now_ms - last >= self.interval_ms,
        };
        if due {
            self.last_sent_ms.set(Some(now_ms));
        }
        due
    }

    /// Forgets the last alert, so the next one goes out at once (the cause was resolved).
    pub fn reset(&self) {
        self.last_sent_ms.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_alert_passes_and_repeats_inside_the_interval_do_not() {
        let throttle = AlertThrottle::new(60_000);

        assert!(throttle.allow(1_000));
        assert!(!throttle.allow(5_000));
        assert!(!throttle.allow(60_999));
        assert!(throttle.allow(61_000));
    }

    #[test]
    fn a_resolved_cause_alerts_again_at_once() {
        let throttle = AlertThrottle::new(60_000);
        assert!(throttle.allow(1_000));

        throttle.reset();

        assert!(throttle.allow(2_000));
    }
}
