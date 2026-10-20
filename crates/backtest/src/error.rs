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

    #[error("{0}")]
    General(String),
}
