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
        "the data window gives the swarm less than {warm_up_days} days of warm-up: the data runs \
         from {data_start_ms:?} to {data_end_ms:?} ms and the first feature is at \
         {first_feature_ms:?} ms; start the data at least {warm_up_days} days before the period \
         to evaluate"
    )]
    InsufficientWarmUp {
        /// Required warm-up, in days.
        warm_up_days: i64,
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
