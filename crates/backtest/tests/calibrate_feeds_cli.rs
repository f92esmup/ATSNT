//! `calibrate_feeds` writes a swarm `SharedConfig` calibrated on `[--from, --to)` only, and
//! refuses an empty calibration period.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output};

use strategies::SwarmConfig;

/// 2024-01-01T00:00:00Z and 2024-01-02T00:00:00Z in Unix ms.
const DAY_1: i64 = 1_704_067_200_000;
const DAY_2: i64 = 1_704_153_600_000;

/// Writes a headered Binance aggTrades CSV: one trade per minute from `DAY_1` over two days,
/// plus one trade a millisecond before `DAY_1` that must be left out.
fn write_fixture(path: &Path) {
    let mut csv = String::from(
        "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker\n",
    );
    let mut stamps = vec![DAY_1 - 1];
    stamps.extend((0..2 * 1_440).map(|i| DAY_1 + i * 60_000));
    for (i, ts) in stamps.iter().enumerate() {
        let price = 40_000 + (i % 7) as i64 - (i % 5) as i64;
        let maker = i % 2 == 0;
        writeln!(csv, "{i},{price}.50,0.250,{i},{i},{ts},{maker}").unwrap();
    }
    std::fs::write(path, csv).unwrap();
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_calibrate_feeds"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn the_output_parses_and_ends_at_the_last_trade_before_to() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("aggTrades.csv");
    let output = dir.path().join("swarm.json");
    write_fixture(&data);

    let result = run(&[
        "--data",
        data.to_str().unwrap(),
        "--from",
        "2024-01-01",
        "--to",
        "2024-01-02",
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let config: SwarmConfig =
        serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(config.calibration_end_ms, DAY_2 - 60_000);
    // 1,440 trades over 1,439 minutes: 5m holds ceil(1440 · 5 / 1439) = 6 trades.
    assert_eq!(config.tick_thresholds[0], 6);
    // The structural fields keep their defaults.
    let defaults = SwarmConfig::default();
    assert_eq!(config.genome_seed, defaults.genome_seed);
    assert_eq!(config.allow_short, defaults.allow_short);

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("1440 kept of 2881 read"),
        "stdout: {stdout}"
    );
}

#[test]
fn an_empty_period_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("aggTrades.csv");
    let output = dir.path().join("swarm.json");
    write_fixture(&data);

    let result = run(&[
        "--data",
        data.to_str().unwrap(),
        "--from",
        "2025-01-01",
        "--to",
        "2025-01-02",
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("no trades in the calibration period"),
        "stderr: {stderr}"
    );
    assert!(!output.exists());
}
