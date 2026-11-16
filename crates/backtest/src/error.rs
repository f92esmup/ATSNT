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

/// The text of a configuration, with where it came from for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigText<'a> {
    /// What the text is, as an operator would name it (usually a file path).
    pub origin: &'a str,
    /// The text itself.
    pub content: &'a str,
}

/// Why a strategy or its configuration could not be loaded.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LoadError {
    /// The configuration could not be read from where it is stored.
    #[error("Failed to read {origin}: {message}")]
    Read {
        /// Where the configuration is stored.
        origin: String,
        /// What the storage reported.
        message: String,
    },
    /// The configuration is not the JSON the loader expects.
    #[error("Failed to parse {what} ({origin}): {message}")]
    Parse {
        /// What was being parsed.
        what: &'static str,
        /// Where the text came from.
        origin: String,
        /// What the parser reported.
        message: String,
    },
    /// A value could not be turned into JSON.
    #[error("Failed to serialize {what}: {message}")]
    Serialize {
        /// What was being serialized.
        what: &'static str,
        /// What the serializer reported.
        message: String,
    },
    /// No strategy is registered under the requested name.
    #[error("{0}")]
    UnknownStrategy(String),
    /// Two sources of the same setting disagree.
    #[error("{0}")]
    Conflict(String),
    /// The deployment gate refused the artifact.
    #[error(
        "the deployment artifact is not deployable: {0}. Pass --allow-unproven-strategy to run \
         it anyway (e.g. to test the mechanics on testnet)"
    )]
    NotDeployable(String),
    /// The strategy emits a kind of decision no execution path accepts.
    #[error("strategy '{0}' emits target-position decisions, which no execution path accepts yet")]
    EmitsTargets(String),
    /// The strategy refused its configuration.
    #[error("Failed to initialize strategy: {0}")]
    Initialize(String),
}
