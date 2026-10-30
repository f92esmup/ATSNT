//! paper_trading and live_trading refuse a strategy that emits target-position decisions
//! (the product gate of spec §17.1), offline and before any network or account access.

use std::process::{Command, Output};

const REFUSAL: &str = "strategy 'swarm_consensus' emits target-position decisions, which \
paper_trading and live_trading do not accept yet";

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn run(binary: &str, extra: &[&str]) -> Output {
    Command::new(binary)
        .args(["--strategy", "swarm_consensus"])
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
    assert!(stderr.contains(REFUSAL), "stderr: {stderr}");
}

#[test]
fn paper_trading_refuses_a_strategy_that_emits_targets() {
    assert_refused(&run(env!("CARGO_BIN_EXE_paper_trading"), &[]));
}

#[test]
fn live_trading_refuses_a_strategy_that_emits_targets() {
    assert_refused(&run(
        env!("CARGO_BIN_EXE_live_trading"),
        &["--api-key", "test-key", "--secret-key", "test-secret"],
    ));
}

/// The gate is a capability gap, not an evidence gate: the override does not lift it.
#[test]
fn the_unproven_strategy_override_does_not_lift_the_gate() {
    assert_refused(&run(
        env!("CARGO_BIN_EXE_paper_trading"),
        &["--allow-unproven-strategy"],
    ));
    assert_refused(&run(
        env!("CARGO_BIN_EXE_live_trading"),
        &[
            "--api-key",
            "test-key",
            "--secret-key",
            "test-secret",
            "--allow-unproven-strategy",
        ],
    ));
}
