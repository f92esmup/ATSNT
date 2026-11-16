//! The adapters of the clock port: the machine's clock and a simulated one.

use ports::Clock;

/// Reads the wall clock of the machine.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .unwrap_or(0)
    }
}

/// A clock set by whoever replays the market: the adapter of the clock port for simulations.
///
/// It starts at 0 and never reads the machine's time, so a replay sees the same instants every
/// time it runs.
#[derive(Debug, Default)]
pub struct SimClock {
    now_ms: std::sync::atomic::AtomicI64,
}

impl SimClock {
    /// A clock stopped at `now_ms`.
    pub fn at(now_ms: i64) -> Self {
        Self {
            now_ms: std::sync::atomic::AtomicI64::new(now_ms),
        }
    }

    /// Moves the clock to `now_ms`. Time never runs backwards: an earlier instant is ignored.
    pub fn advance_to(&self, now_ms: i64) {
        self.now_ms
            .fetch_max(now_ms, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Clock for SimClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simulated_clock_moves_only_forwards_and_only_when_told() {
        let clock = SimClock::at(1_000);
        assert_eq!(clock.now_ms(), 1_000);
        clock.advance_to(5_000);
        clock.advance_to(2_000);
        assert_eq!(clock.now_ms(), 5_000);
        assert_eq!(SimClock::default().now_ms(), 0);
    }

    #[test]
    fn the_system_clock_reads_a_time_after_2026() {
        // 2026-01-01T00:00:00Z: the machine's clock must not be before the project's data.
        assert!(SystemClock.now_ms() > 1_767_225_600_000);
    }
}
