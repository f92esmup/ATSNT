//! Contract for strict analytics/artifact sink handling and artifact object naming.

use std::path::Path;

use adapters::AdapterError;
use backtest::{artifact_object_name, artifact_run_suffix, SinkFailures};
use chrono::{TimeZone, Utc};

fn failure(msg: &str) -> Result<(), AdapterError> {
    Err(AdapterError::General(msg.to_string()))
}

#[test]
fn tolerant_mode_finishes_ok_despite_failures() {
    let mut sinks = SinkFailures::new(false);
    sinks.record("record HPO run", failure("HTTP 500"));
    assert!(sinks.finish().is_ok());
}

#[test]
fn strict_mode_reports_every_failure_at_finish() {
    let mut sinks = SinkFailures::new(true);
    sinks.record("record HPO run", failure("HTTP 500"));
    sinks.record("upload artifact", Ok(()));
    sinks.record("upload artifact", failure("HTTP 403"));
    let err = sinks.finish().expect_err("strict mode must fail");
    let msg = err.to_string();
    assert!(msg.contains("2 sink operation(s) failed"), "{msg}");
    assert!(msg.contains("record HPO run: HTTP 500"), "{msg}");
    assert!(msg.contains("upload artifact: HTTP 403"), "{msg}");
}

#[test]
fn strict_mode_without_failures_finishes_ok() {
    let mut sinks = SinkFailures::new(true);
    sinks.record("record backtest run", Ok(()));
    assert!(sinks.finish().is_ok());
}

#[test]
fn artifact_object_name_appends_suffix_to_config_stem() {
    let name = artifact_object_name(
        Path::new("configs/hpo_btcusdt_2026_01.json"),
        "hpo-1760000000",
    );
    assert_eq!(name, "artifacts/hpo_btcusdt_2026_01-hpo-1760000000.json");
}

#[test]
fn artifact_run_suffix_prefers_explicit_value() {
    let now = Utc.with_ymd_and_hms(2026, 10, 14, 18, 5, 9).unwrap();
    assert_eq!(artifact_run_suffix(Some("hpo-123"), now), "hpo-123");
}

#[test]
fn artifact_run_suffix_falls_back_to_utc_timestamp() {
    let now = Utc.with_ymd_and_hms(2026, 10, 14, 18, 5, 9).unwrap();
    assert_eq!(artifact_run_suffix(None, now), "20261014T180509Z");
    assert_eq!(artifact_run_suffix(Some("  "), now), "20261014T180509Z");
}
