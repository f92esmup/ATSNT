//! The system clock: the adapter of the clock port.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_reads_a_time_after_2026() {
        // 2026-01-01T00:00:00Z: the machine's clock must not be before the project's data.
        assert!(SystemClock.now_ms() > 1_767_225_600_000);
    }
}
