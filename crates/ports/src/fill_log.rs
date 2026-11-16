//! Append-only log of the fills a live run observed, one text record per fill.

/// Where observed fills are appended.
pub trait FillLog {
    /// Error reported by the storage behind the log.
    type Error: std::fmt::Display;
    /// Appends one record. The log adds the record separator.
    fn append(&self, record: &str) -> Result<(), Self::Error>;
}
