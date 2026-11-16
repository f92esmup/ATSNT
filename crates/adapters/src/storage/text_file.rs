//! Whole text files: configurations and artifacts the composition hands to the library as text.

use std::path::Path;

use crate::error::AdapterError;

/// Reads the whole file at `path` as UTF-8 text.
pub fn read_text(path: &Path) -> Result<String, AdapterError> {
    Ok(std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_read_back_and_a_missing_one_is_an_error() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "{\"a\": 1}").unwrap();
        assert_eq!(read_text(file.path()).unwrap(), "{\"a\": 1}");
        assert!(read_text(&file.path().with_extension("missing")).is_err());
    }
}
