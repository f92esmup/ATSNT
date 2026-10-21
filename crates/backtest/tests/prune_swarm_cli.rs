//! CLI integration tests for `prune_swarm`.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output};

use strategies::SwarmConfig;

const DAY_1: i64 = 1_704_067_200_000;

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

fn run_calibrate(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_calibrate_feeds"))
        .args(args)
        .output()
        .unwrap()
}

fn run_prune(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prune_swarm"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn prune_swarm_cli_produces_valid_custom_agents_population() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("aggTrades.csv");
    let base_json = dir.path().join("swarm_base.json");
    let pruned_json = dir.path().join("swarm_pruned.json");
    write_fixture(&data);

    // 1. Run calibrate_feeds with --calibrate-priors
    let cal_res = run_calibrate(&[
        "--data",
        data.to_str().unwrap(),
        "--from",
        "2024-01-01",
        "--to",
        "2024-01-02",
        "-o",
        base_json.to_str().unwrap(),
        "--calibrate-priors",
    ]);
    assert!(
        cal_res.status.success(),
        "calibrate stderr: {}",
        String::from_utf8_lossy(&cal_res.stderr)
    );

    let base_cfg: SwarmConfig =
        serde_json::from_str(&std::fs::read_to_string(&base_json).unwrap()).unwrap();
    assert!(base_cfg.validate().is_ok());

    // 2. Run prune_swarm
    let prune_res = run_prune(&[
        "--data",
        data.to_str().unwrap(),
        "--from",
        "2024-01-01",
        "--to",
        "2024-01-02",
        "--shared-config",
        base_json.to_str().unwrap(),
        "--candidates-per-feed",
        "8",
        "--keep-per-feed",
        "4",
        "-o",
        pruned_json.to_str().unwrap(),
    ]);
    assert!(
        prune_res.status.success(),
        "prune stderr: {}",
        String::from_utf8_lossy(&prune_res.stderr)
    );

    let pruned_cfg: SwarmConfig =
        serde_json::from_str(&std::fs::read_to_string(&pruned_json).unwrap()).unwrap();
    assert_eq!(pruned_cfg.agents_per_feed, 4);
    let agents = pruned_cfg
        .custom_agents
        .as_ref()
        .expect("custom agents present");
    assert_eq!(agents.len(), 4 * 16);
    assert!(pruned_cfg.validate().is_ok());
}
