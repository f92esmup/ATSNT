//! paper_trading and live_trading refuse an artifact whose HPO scorecard shows no edge.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The v3 HPO winner of the 500 EUR account: eligible, but no edge (fitness 0, DSR p = 1).
const LOSING_ARTIFACT: &str = include_str!("fixtures/losing_artifact.json");

fn losing_artifact(dir: &Path) -> PathBuf {
    let path = dir.join("losing.json");
    std::fs::write(&path, LOSING_ARTIFACT).unwrap();
    path
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn run(binary: &str, config: &Path, extra: &[&str]) -> Output {
    Command::new(binary)
        .arg("--config")
        .arg(config)
        .args(extra)
        // No network in tests: the gate must fire before any connection is opened.
        .env("BINANCE_API_KEY", "test-key")
        .env("BINANCE_SECRET_KEY", "test-secret")
        .output()
        .expect("run binary")
}

fn assert_refused(out: &Output) {
    let stderr = text(&out.stderr);
    assert!(
        !out.status.success(),
        "must refuse; stdout: {}",
        text(&out.stdout)
    );
    assert!(stderr.contains("not deployable"), "stderr: {stderr}");
    assert!(
        stderr.contains("no positive out-of-sample edge"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("--allow-unproven-strategy"),
        "stderr: {stderr}"
    );
}

#[test]
fn paper_trading_refuses_a_losing_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let config = losing_artifact(dir.path());

    assert_refused(&run(env!("CARGO_BIN_EXE_paper_trading"), &config, &[]));
}

#[test]
fn live_trading_refuses_a_losing_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let config = losing_artifact(dir.path());

    assert_refused(&run(
        env!("CARGO_BIN_EXE_live_trading"),
        &config,
        &["--api-key", "test-key", "--secret-key", "test-secret"],
    ));
}

/// With the override the binary gets past the gate: it warns, then stops at the next offline
/// check (an option conflict chosen for it), never at the gate and never on the network.
fn assert_passes_the_gate(out: &Output, next_check: &str) {
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(
        !out.status.success(),
        "the chosen option conflict must stop it; stdout: {stdout}"
    );
    assert!(
        stdout.contains("[WARNING] Running an unproven strategy (--allow-unproven-strategy)"),
        "stdout: {stdout}"
    );
    assert!(!stderr.contains("not deployable"), "stderr: {stderr}");
    assert!(stderr.contains(next_check), "stderr: {stderr}");
}

#[test]
fn paper_trading_runs_a_losing_artifact_past_the_gate_only_when_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let config = losing_artifact(dir.path());

    let out = run(
        env!("CARGO_BIN_EXE_paper_trading"),
        &config,
        &["--allow-unproven-strategy", "--spot"],
    );

    // The futures-only strategy refuses a Spot session: the first offline check after the gate.
    assert_passes_the_gate(&out, "does not support the selected Spot market");
}

#[test]
fn live_trading_runs_a_losing_artifact_past_the_gate_only_when_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let config = losing_artifact(dir.path());

    let out = run(
        env!("CARGO_BIN_EXE_live_trading"),
        &config,
        &[
            "--api-key",
            "test-key",
            "--secret-key",
            "test-secret",
            "--allow-unproven-strategy",
            "--risk-pct",
            "2",
        ],
    );

    assert_passes_the_gate(&out, "--risk-pct must be greater than 0 and at most 1");
}
