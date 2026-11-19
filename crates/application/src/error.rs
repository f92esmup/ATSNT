use thiserror::Error;

/// Error types for the backtesting and optimization engine.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BacktestError {
    #[error("insufficient data: needed at least {needed} bars, but found {found}")]
    InsufficientData { needed: usize, found: usize },

    #[error("invalid fold configuration: {0}")]
    InvalidFoldConfig(String),

    #[error("strategy error: {0}")]
    Strategy(String),

    #[error("domain error: {0}")]
    Domain(#[from] domain::DomainError),

    /// A price bucket width that is not a positive number of milliseconds.
    #[error("price bucket must be a positive number of milliseconds, got {0}")]
    InvalidPriceBucket(i64),

    /// Data that does not give a trade-consuming strategy its warm-up before the first
    /// evaluated timestamp (strategy spec §15.2).
    #[error(
        "the data window gives the strategy less than {} of warm-up: the data runs from \
         {data_start_ms:?} to {data_end_ms:?} ms and the first feature is at \
         {first_feature_ms:?} ms; start the data at least that long before the period to evaluate",
        warm_up_text(*warm_up_ms)
    )]
    InsufficientWarmUp {
        /// Required warm-up, in milliseconds.
        warm_up_ms: i64,
        /// First trade of the data, if any.
        data_start_ms: Option<i64>,
        /// Last trade of the data, if any.
        data_end_ms: Option<i64>,
        /// First feature the extractor produced, if any.
        first_feature_ms: Option<i64>,
    },

    /// A shared configuration calibrated on data that overlaps the evaluated data (strategy
    /// spec §15.2).
    #[error(
        "the shared configuration's calibration overlaps the data: calibration_end_ms \
         {calibration_end_ms} is not earlier than the first trade at {first_trade_ms} ms; \
         calibrate on a period that ends before the data starts"
    )]
    CalibrationOverlap {
        /// Last timestamp the calibration used.
        calibration_end_ms: i64,
        /// First trade of the data.
        first_trade_ms: i64,
    },

    #[error("{0}")]
    General(String),
}

/// A warm-up in the largest whole unit that states it exactly (days, hours, minutes, seconds),
/// or in milliseconds.
fn warm_up_text(warm_up_ms: i64) -> String {
    const UNITS: [(i64, &str); 4] = [
        (86_400_000, "days"),
        (3_600_000, "hours"),
        (60_000, "minutes"),
        (1_000, "seconds"),
    ];
    UNITS
        .iter()
        .find(|(unit, _)| warm_up_ms >= *unit && warm_up_ms % unit == 0)
        .map(|(unit, name)| format!("{} {name}", warm_up_ms / unit))
        .unwrap_or_else(|| format!("{warm_up_ms} ms"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warm_up_is_stated_in_its_largest_exact_unit() {
        assert_eq!(warm_up_text(70 * 86_400_000), "70 days");
        assert_eq!(warm_up_text(36 * 3_600_000), "36 hours");
        assert_eq!(warm_up_text(90_000), "90 seconds");
        assert_eq!(warm_up_text(1_500), "1500 ms");
        assert_eq!(warm_up_text(0), "0 ms");
    }
}
