//! The `portfolio` binary: one strategy on several instruments with one account.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use adapters::TapeWriter;
use domain::Trade;
use rust_decimal_macros::dec;
use support::swarm::{busy_policy, shared, swarm_trades};

fn write_tape(dir: &Path, name: &str, trades: &[Trade]) -> PathBuf {
    let path = dir.join(name);
    let mut writer = TapeWriter::new(&path).expect("create parquet fixture");
    for trade in trades {
        writer.write_trade(trade).expect("write trade");
    }
    writer.finish().expect("finish parquet fixture");
    path
}

fn write_json<T: serde::Serialize>(dir: &Path, name: &str, value: &T) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    path
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The swarm's tape, and a second instrument at a tenth of its price, a second later.
fn tapes(dir: &Path) -> (PathBuf, PathBuf) {
    let btc = swarm_trades(90);
    let eth: Vec<Trade> = btc
        .iter()
        .map(|trade| {
            Trade::new(
                trade.timestamp + 1_000,
                trade.price / dec!(10),
                trade.quantity * dec!(10),
                trade.side,
            )
            .unwrap()
        })
        .collect();
    (
        write_tape(dir, "BTCUSDT.parquet", &btc),
        write_tape(dir, "ETHUSDT.parquet", &eth),
    )
}

fn run(dir: &Path, extra: &[&str]) -> Output {
    let (btc, eth) = tapes(dir);
    let shared_path = write_json(dir, "shared.json", &shared());
    let config_path = write_json(dir, "policy.json", &busy_policy());
    Command::new(env!("CARGO_BIN_EXE_portfolio"))
        .arg("--data")
        .arg(format!("BTCUSDT={}", btc.display()))
        .arg("--data")
        .arg(format!("ETHUSDT={}", eth.display()))
        .args(["--strategy", "swarm"])
        .arg("--shared-config")
        .arg(&shared_path)
        .arg("--config")
        .arg(&config_path)
        .args(extra)
        .output()
        .expect("run portfolio")
}

#[test]
fn the_swarm_trades_two_instruments_on_one_account() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("runs.sqlite");
    let out = run(
        dir.path(),
        &[
            "--max-open-positions",
            "2",
            "--record",
            record.to_str().unwrap(),
        ],
    );
    let output = text(&out);
    assert!(out.status.success(), "{output}");
    assert!(output.contains("one instance per instrument"), "{output}");
    assert!(output.contains(" BTCUSDT "), "{output}");
    assert!(output.contains(" ETHUSDT "), "{output}");

    let db = rusqlite::Connection::open(&record).unwrap();
    let symbols: Vec<String> = db
        .prepare("SELECT DISTINCT symbol FROM trades ORDER BY symbol")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(symbols, ["BTCUSDT", "ETHUSDT"]);
    let run_symbol: String = db
        .query_row("SELECT symbol FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_symbol, "BTCUSDT,ETHUSDT");
}

#[test]
fn one_position_at_a_time_refuses_entries_on_the_other_instrument() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &[]);
    let output = text(&out);
    assert!(out.status.success(), "{output}");
    assert!(output.contains("at most 1 open at once"), "{output}");
    assert!(output.contains("MaxPositionsExceeded"), "{output}");
}

#[test]
fn an_instrument_given_twice_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (btc, _) = tapes(dir.path());
    let out = Command::new(env!("CARGO_BIN_EXE_portfolio"))
        .arg("--data")
        .arg(format!("BTCUSDT={}", btc.display()))
        .arg("--data")
        .arg(format!("btcusdt={}", btc.display()))
        .args(["--strategy", "swarm"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(text(&out).contains("each instrument may be given once"));
}

#[test]
fn the_tick_route_runs_one_session_per_instrument() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        dir.path(),
        &["--fill-model", "tick", "--max-open-positions", "2"],
    );
    let output = text(&out);
    assert!(out.status.success(), "{output}");
    assert!(output.contains("one session per instrument"), "{output}");
    assert!(output.contains(" BTCUSDT "), "{output}");
    assert!(output.contains(" ETHUSDT "), "{output}");
}
