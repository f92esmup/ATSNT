//! The machine's wall clock, for the binaries that stamp their runs.

/// Current wall-clock time in Unix milliseconds; `0` if the clock is before the epoch.
pub fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wall_clock_reads_a_time_after_2026() {
        // 2026-01-01T00:00:00Z.
        assert!(now_unix_ms() > 1_767_225_600_000);
    }
}
