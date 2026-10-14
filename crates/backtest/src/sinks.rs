//! Failure policy for analytics (BigQuery) and artifact (GCS) sinks, and
//! naming of uploaded deployment artifacts.
//!
//! Locally, sink failures are only warned about so a run never dies because the
//! cloud is unreachable. On ephemeral infrastructure (Cloud Batch) a silently
//! lost result is worse than a failed job, so strict mode makes the process exit
//! non-zero — but only after all local work (artifact file, Monte Carlo) is done.

use std::path::Path;

use adapters::AdapterError;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// Error returned by [`SinkFailures::finish`] in strict mode.
#[derive(Debug, Error)]
#[error("{count} sink operation(s) failed in strict mode: {details}")]
pub struct StrictSinkError {
    /// Number of failed sink operations.
    pub count: usize,
    /// `"<operation>: <error>"` entries joined with `"; "`.
    pub details: String,
}

/// Collects sink failures during a run and decides at the end whether they are fatal.
#[derive(Debug, Default)]
pub struct SinkFailures {
    strict: bool,
    failures: Vec<String>,
}

impl SinkFailures {
    /// Creates a collector; `strict` makes [`SinkFailures::finish`] fail on any recorded error.
    pub fn new(strict: bool) -> Self {
        Self {
            strict,
            failures: Vec::new(),
        }
    }

    /// Records the outcome of one sink operation, printing `[WARN] Failed to <operation>: <error>`
    /// on failure.
    pub fn record(&mut self, operation: &str, result: Result<(), AdapterError>) {
        if let Err(e) = result {
            println!("[WARN] Failed to {operation}: {e}");
            self.failures.push(format!("{operation}: {e}"));
        }
    }

    /// Returns an error listing every failure when strict; otherwise always `Ok`.
    pub fn finish(self) -> Result<(), StrictSinkError> {
        if !self.strict || self.failures.is_empty() {
            return Ok(());
        }
        Err(StrictSinkError {
            count: self.failures.len(),
            details: self.failures.join("; "),
        })
    }
}

/// GCS object name for an uploaded deployment artifact:
/// `artifacts/<config file stem>-<run suffix>.json`, unique per run.
pub fn artifact_object_name(output_config: &Path, run_suffix: &str) -> String {
    let stem = output_config
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("strategy_artifact");
    format!("artifacts/{stem}-{run_suffix}.json")
}

/// Run suffix for artifact names: the explicit value when non-blank (e.g. the
/// Cloud Batch job name), otherwise the UTC timestamp `YYYYMMDDTHHMMSSZ`.
pub fn artifact_run_suffix(explicit: Option<&str>, now: DateTime<Utc>) -> String {
    match explicit.map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => now.format("%Y%m%dT%H%M%SZ").to_string(),
    }
}
