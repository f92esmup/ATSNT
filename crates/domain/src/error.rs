use thiserror::Error;

/// Domain errors representing financial or state invariant violations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("threshold must be strictly positive, got: {0}")]
    InvalidThreshold(String),
    #[error("price must be strictly positive, got: {0}")]
    InvalidPrice(String),
    #[error("quantity must be strictly positive, got: {0}")]
    InvalidQuantity(String),
}
