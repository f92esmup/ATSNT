//! The runners on a trade-consuming strategy (`odd/tasks/swarm-consensus.md`, T7.2 and T7.3):
//! the HPO tape of price buckets with its warm-up and calibration checks, `run_hpo`, and the
//! `backtest` CLI at trade resolution, all on a deterministic synthetic tape of the swarm.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use adapters::BinanceParquetWriter;
use backtest::{
    check_calibration, embargo_buckets, trade_tape, CandidateEvaluation, WalkForwardConfig,
    WalkForwardOptimizer, WalkForwardSplitter, TRADES_WARM_UP_MS,
};
use domain::Trade;
use rust_decimal_macros::dec;
use strategies::{PolicyConfig, SwarmConfig, SwarmConsensusStrategy, SwarmPolicySpace};

mod support;
use support::swarm::{account, busy_policy, shared, swarm_trades, DAY_MS, START_MS};

const BUCKET_MS: i64 = 1_000;
const EMBARGO_MS: i64 = 5_000;

fn space() -> SwarmPolicySpace {
    SwarmPolicySpace {
        threshold_bases: vec![dec!(0.05), dec!(0.10)],
        threshold_steps: vec![dec!(0.05)],
        hysteresis_values: vec![dec!(0.02)],
        base_risks: vec![dec!(0.01)],
        risk_increments: vec![dec!(0.005)],
        stop_atr_multipliers: vec![dec!(3.0)],
        take_profit_atr_multipliers: vec![None],
        ..SwarmPolicySpace::default()
    }
}

fn ranking(trades: &[Trade]) -> Vec<CandidateEvaluation<PolicyConfig>> {
    let shared = shared();
    let tape = trade_tape::<SwarmConsensusStrategy>(&shared, trades.to_vec(), BUCKET_MS)
        .expect("tape builds")
        .after_warm_up(TRADES_WARM_UP_MS)
        .expect("the tape has its warm-up");
    let folds = WalkForwardSplitter::generate_rolling_folds(
        tape.len(),
        &WalkForwardConfig {
            num_folds: 2,
            train_ratio: 0.7,
            embargo_bars: embargo_buckets(EMBARGO_MS, BUCKET_MS).expect("embargo"),
        },
    )
    .expect("folds");
    WalkForwardOptimizer::<SwarmConsensusStrategy>::new(account(), space())
        .with_shared_config(shared)
        .with_min_trades_per_fold(0)
        .run_optimization_on_tape(&tape, &folds, 10)
        .expect("optimization runs")
}

#[test]
fn a_trade_tape_hpo_ranks_deterministically() {
    let trades = swarm_trades(90);
    let first = ranking(&trades);
    let second = ranking(&trades);

    assert_eq!(first.len(), 2);
    assert!(
        first.iter().any(|e| e.total_oos_trades > 0),
        "the busy policy trades out of sample"
    );
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
}

#[test]
fn trade_folds_respect_the_embargo() {
    let tape = trade_tape::<SwarmConsensusStrategy>(&shared(), swarm_trades(90), BUCKET_MS)
        .unwrap()
        .after_warm_up(TRADES_WARM_UP_MS)
        .unwrap();
    let embargo = embargo_buckets(EMBARGO_MS, BUCKET_MS).unwrap();
    let folds = WalkForwardSplitter::generate_rolling_folds(
        tape.len(),
        &WalkForwardConfig {
            num_folds: 2,
            train_ratio: 0.7,
            embargo_bars: embargo,
        },
    )
    .unwrap();

    assert_eq!(embargo, 5);
    for fold in &folds {
        assert_eq!(fold.embargo_len(), embargo);
        let train_end = tape.prices()[fold.train_range.end - 1].end_ms;
        let test_start = tape.prices()[fold.test_range.start].start_ms;
        assert!(test_start - train_end > EMBARGO_MS, "{fold:?}");
    }
    // The evaluated tape starts after 70 days of data, with the swarm already warm.
    assert!(tape.prices()[0].start_ms >= START_MS + TRADES_WARM_UP_MS);
    assert!(!tape.features().is_empty());
}

#[test]
fn a_tape_without_seventy_days_of_warm_up_is_refused() {
    let message = trade_tape::<SwarmConsensusStrategy>(&shared(), swarm_trades(60), BUCKET_MS)
        .unwrap()
        .after_warm_up(TRADES_WARM_UP_MS)
        .unwrap_err()
        .to_string();
    assert!(
        message.starts_with("the data window gives the swarm less than 70 days of warm-up"),
        "{message}"
    );
}

#[test]
fn a_calibration_overlapping_the_trades_is_refused() {
    let message = check_calibration(Some(START_MS), START_MS)
        .unwrap_err()
        .to_string();
    assert!(message.contains("calibration"), "{message}");
}

#[tokio::test]
async fn a_backtest_records_the_strategy_audit_only_when_there_is_one() {
    use backtest::{
        record_backtest, record_backtest_with_audit, BacktestMetrics, DataWindow, RunContext,
        RunSpec, RunType, SamplingConfig,
    };
    let spec = RunSpec {
        strategy_id: "swarm_consensus".to_string(),
        symbol: "BTCUSDT".to_string(),
        market_type: strategies::MarketType::UsdMPerpetual,
        sampling: SamplingConfig::Trades {
            price_bucket_ms: None,
        },
        strategy_config: busy_policy(),
        execution_context: account(),
    };
    let ctx = RunContext::new(RunType::Backtest, &spec, 0);
    let metrics = BacktestMetrics::calculate(dec!(10_000), &[], dec!(0), dec!(0));
    let audit = serde_json::json!({"agents": 96});

    let audited = adapters::InMemoryRunRecorder::new();
    record_backtest_with_audit(
        &audited,
        &ctx,
        &spec,
        &DataWindow::default(),
        &metrics,
        &[],
        Some(&audit),
    )
    .await
    .unwrap();
    let plain = adapters::InMemoryRunRecorder::new();
    record_backtest(&plain, &ctx, &spec, &DataWindow::default(), &metrics, &[])
        .await
        .unwrap();

    let extras = audited.snapshot().metrics[0]
        .extras
        .clone()
        .expect("extras");
    let extras: serde_json::Value = serde_json::from_str(&extras).unwrap();
    assert_eq!(
        extras,
        serde_json::json!({"strategy_audit": {"agents": 96}})
    );
    assert_eq!(plain.snapshot().metrics[0].extras, None);
}

// ---- Binaries ----

fn write_trades(dir: &Path, trades: &[Trade]) -> PathBuf {
    let path = dir.join("BTCUSDT-aggTrades-swarm.parquet");
    let mut writer = BinanceParquetWriter::new(&path).expect("create parquet fixture");
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

fn run_hpo(data: &Path, output: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_run_hpo"))
        .arg("--data")
        .arg(data)
        .arg("--output-config")
        .arg(output)
        .args([
            "--strategy",
            "swarm_consensus",
            "--folds",
            "2",
            "--candidates",
            "2",
            "--embargo-ms",
            "5000",
            "--min-trades-per-fold",
            "0",
        ])
        .args(extra)
        .output()
        .expect("run run_hpo")
}

#[test]
fn run_hpo_help_lists_the_trade_flags() {
    let out = Command::new(env!("CARGO_BIN_EXE_run_hpo"))
        .arg("--help")
        .output()
        .unwrap();
    let help = text(&out);
    for flag in ["--shared-config", "--price-bucket-ms", "--embargo-ms"] {
        assert!(help.contains(flag), "{flag} missing from:\n{help}");
    }
}

#[test]
fn run_hpo_optimises_the_swarm_on_price_buckets_and_stores_the_shared_config() {
    let dir = tempfile::tempdir().unwrap();
    let data = write_trades(dir.path(), &swarm_trades(90));
    let shared_path = write_json(dir.path(), "shared.json", &shared());
    let space_path = write_json(dir.path(), "space.json", &space());
    let output = dir.path().join("swarm_hpo.json");

    let out = run_hpo(
        &data,
        &output,
        &[
            "--shared-config",
            shared_path.to_str().unwrap(),
            "--param-space",
            space_path.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));

    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(artifact["strategy_id"], "swarm_consensus");
    assert_eq!(
        artifact["sampling"],
        serde_json::json!({"type": "trades", "price_bucket_ms": 1000})
    );
    assert_eq!(
        artifact["shared_config"],
        serde_json::to_value(shared()).unwrap()
    );
    assert!(artifact["hpo_scorecard"].is_object());
}

#[test]
fn run_hpo_refuses_the_swarm_without_a_shared_config() {
    let dir = tempfile::tempdir().unwrap();
    let data = write_trades(dir.path(), &swarm_trades(3));
    let output = dir.path().join("never.json");

    let out = run_hpo(&data, &output, &[]);
    assert!(!out.status.success());
    assert!(text(&out).contains("--shared-config"), "{}", text(&out));
    assert!(!output.exists());
}

#[test]
fn run_hpo_refuses_a_calibration_that_overlaps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let data = write_trades(dir.path(), &swarm_trades(3));
    let overlapping = SwarmConfig {
        calibration_end_ms: START_MS + DAY_MS,
        ..shared()
    };
    let shared_path = write_json(dir.path(), "shared.json", &overlapping);
    let output = dir.path().join("never.json");

    let out = run_hpo(
        &data,
        &output,
        &["--shared-config", shared_path.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(
        text(&out).contains("calibration overlaps"),
        "{}",
        text(&out)
    );
    assert!(!output.exists());
}

#[test]
fn run_hpo_refuses_data_without_the_warm_up() {
    let dir = tempfile::tempdir().unwrap();
    let data = write_trades(dir.path(), &swarm_trades(60));
    let shared_path = write_json(dir.path(), "shared.json", &shared());
    let output = dir.path().join("never.json");

    let out = run_hpo(
        &data,
        &output,
        &["--shared-config", shared_path.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(
        text(&out).contains("the data window gives the swarm less than 70 days of warm-up"),
        "{}",
        text(&out)
    );
}

#[test]
fn run_hpo_keeps_the_trade_flags_away_from_dollar_bar_strategies() {
    let dir = tempfile::tempdir().unwrap();
    let data = write_trades(dir.path(), &swarm_trades(3));
    let output = dir.path().join("never.json");

    let out = Command::new(env!("CARGO_BIN_EXE_run_hpo"))
        .arg("--data")
        .arg(&data)
        .arg("--output-config")
        .arg(&output)
        .args(["--strategy", "cusum", "--embargo-ms", "5000"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(text(&out).contains("--embargo-ms"), "{}", text(&out));
}

fn run_backtest(data: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_backtest"))
        .arg("--data")
        .arg(data)
        .args(["--strategy", "swarm_consensus"])
        .args(extra)
        .output()
        .expect("run backtest")
}

#[test]
fn the_backtest_runs_the_swarm_at_trade_resolution_and_reports_its_audit() {
    let dir = tempfile::tempdir().unwrap();
    let data = write_trades(dir.path(), &swarm_trades(90));
    let shared_path = write_json(dir.path(), "shared.json", &shared());
    let policy_path = write_json(dir.path(), "policy.json", &busy_policy());

    let out = run_backtest(
        &data,
        &[
            "--strategy-config",
            policy_path.to_str().unwrap(),
            "--shared-config",
            shared_path.to_str().unwrap(),
            "--fill-model",
            "bar",
        ],
    );
    let stdout = text(&out);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains(&format!(" Raw Ticks Processed:       {}", 90 * 24)),
        "{stdout}"
    );
    let trades_line = stdout
        .lines()
        .find(|l| l.starts_with(" Total Trades Executed:"))
        .expect("trade count line");
    let campaigns: usize = trades_line
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a count");
    assert!(campaigns > 0, "{stdout}");
    assert!(stdout.contains("STRATEGY AUDIT"), "{stdout}");
    assert!(stdout.contains("\"rule_firings\""), "{stdout}");
}

#[test]
fn the_backtest_refuses_a_calibration_that_overlaps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let data = write_trades(dir.path(), &swarm_trades(3));
    let overlapping = SwarmConfig {
        calibration_end_ms: START_MS + DAY_MS,
        ..shared()
    };
    let shared_path = write_json(dir.path(), "shared.json", &overlapping);

    let out = run_backtest(&data, &["--shared-config", shared_path.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("calibration overlaps"),
        "{}",
        text(&out)
    );
}

#[test]
fn the_backtest_runs_the_tick_fill_model_for_a_trade_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let data = write_trades(dir.path(), &swarm_trades(3));
    let shared_path = write_json(dir.path(), "shared.json", &shared());

    let out = run_backtest(
        &data,
        &[
            "--shared-config",
            shared_path.to_str().unwrap(),
            "--fill-model",
            "tick",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let t = text(&out);
    assert!(t.contains("QUANTITATIVE PERFORMANCE REPORT"));
    assert!(t.contains("STRATEGY AUDIT"));
}
