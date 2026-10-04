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

    #[error("{0}")]
    General(String),
}
