//! A JSON-lines file: the adapter of the fill log port.

use std::io::Write;
use std::path::PathBuf;

use ports::FillLog;

/// Appends one line per record to a file, creating it on the first record.
#[derive(Debug, Clone)]
pub struct JsonLinesFile {
    path: PathBuf,
}

impl JsonLinesFile {
    /// A log that appends to `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl FillLog for JsonLinesFile {
    type Error = std::io::Error;

    fn append(&self, record: &str) -> Result<(), Self::Error> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(record.as_bytes())?;
        file.write_all(b"\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_appended_one_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fills.jsonl");
        let log = JsonLinesFile::new(&path);
        log.append("{\"a\":1}").unwrap();
        log.append("{\"a\":2}").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"a\":1}\n{\"a\":2}\n"
        );
    }
}
