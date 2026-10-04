use thiserror::Error;

/// Adapter and I/O integration errors.
#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse decimal '{raw}' on line {line}")]
    DecimalParseError { line: usize, raw: String },

    #[error("failed to parse timestamp '{raw}' on line {line}")]
    TimestampParseError { line: usize, raw: String },

    #[error("malformed CSV record on line {line}: expected at least 7 columns, got {found}")]
    MalformedLine { line: usize, found: usize },

    #[error("domain invariant violation on line {line}: {source}")]
    Domain {
        line: usize,
        #[source]
        source: domain::DomainError,
    },
}
