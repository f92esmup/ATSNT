//! `--spot` still selects the Spot market. The swarm is perpetual-only, so the run refuses it
//! offline, before any data is read.

use std::process::Command;

#[test]
fn the_spot_flag_selects_the_spot_market_and_the_swarm_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("empty.csv");
    std::fs::write(&data, "").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_backtest"))
        .args(["--strategy", "swarm", "--spot", "--data"])
        .arg(&data)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "must refuse; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(stderr.contains("does not support Spot"), "stderr: {stderr}");
}
