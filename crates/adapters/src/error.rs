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

    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("ZIP archive error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("WebSocket error: {0}")]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("domain error: {0}")]
    DomainError(#[from] domain::DomainError),

    #[error("{0}")]
    General(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for AdapterError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(err))
    }
}
