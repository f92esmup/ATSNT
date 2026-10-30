//! Characterisation (golden) tests of the run records.
//!
//! They pin, byte for byte, the rows the swarm's long-only tick-level run records for run
//! analytics, on the same ninety-day tape and account as `golden_swarm.rs`. The trade-level
//! outcomes themselves are pinned there. A refactor of the recording layer must leave the
//! snapshot unchanged.
//!
//! Snapshots live in `tests/golden/`. To rewrite them after an intended behaviour change, run
//! `UPDATE_GOLDEN=1 cargo test -p backtest --test golden_execution` and review the diff.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use backtest::{
    BacktestConfig, FillModel, RunContext, RunId, RunSpec, RunType, SamplingConfig,
    TickSimulationConfig,
};
use domain::futures::FundingSchedule;
use domain::Trade;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use serde_json::Value;
use strategies::{
    MarketType, OptimizableStrategy, PolicyConfig, Strategy, SwarmConfig, SwarmConsensusStrategy,
};

/// Ninety days: past the swarm's 70-day warm-up, so the trades are decided (as in `golden_swarm`).
const DAYS: i64 = 90;
const CAPITAL: Decimal = dec!(5000);
const SEED: u64 = 7;
const DOLLAR_BAR: Decimal = dec!(1_000_000);
const TICK_LATENCY_MS: i64 = 100;
const FIXED_RUN_ID: &str = "0198b3c0-7a4e-7c21-8f3d-5b2a9c6e1d40";

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"))
}

/// Compares `value` with its snapshot, or rewrites the snapshot when `UPDATE_GOLDEN` is set.
fn assert_golden<T: Serialize>(name: &str, value: &T) {
    let actual = serde_json::to_string_pretty(value).expect("snapshot serializes") + "\n";
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("golden dir")).expect("golden dir");
        std::fs::write(&path, &actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {}: {e}; run with UPDATE_GOLDEN=1 once",
            path.display()
        )
    });
    if expected != actual {
        let actual_path = path.with_extension("actual.json");
        let _ = std::fs::write(&actual_path, &actual);
        panic!(
            "golden {name} changed; compare with {} or review with UPDATE_GOLDEN=1 and git diff",
            actual_path.display()
        );
    }
}

async fn tick_run<S: Strategy>(
    account: BacktestConfig,
    strategy: S,
    trades: &[Trade],
) -> (backtest::BacktestMetrics, Vec<backtest::ClosedTrade>) {
    let config = TickSimulationConfig {
        backtest: account,
        symbol: "BTCUSDT".to_string(),
        dollar_bar_threshold: DOLLAR_BAR,
        latency_ms: TICK_LATENCY_MS,
        funding: FundingSchedule::constant(account.funding_rate_pct),
    };
    let mut simulation =
        backtest::wiring::tick_simulation(config, strategy).expect("tick simulation builds");
    for trade in trades {
        simulation.on_trade(trade).await;
    }
    simulation.finish()
}

/// Replaces every run id (a UUID inside any string) by its order of first appearance, and the
/// wall-clock timestamps and the build's git revision by constants, so recorded rows compare
/// across runs and commits.
fn normalize(value: &mut Value, ids: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, field) in map.iter_mut() {
                if matches!(key.as_str(), "recorded_at" | "finished_at") && field.is_string() {
                    *field = Value::String("<wall-clock>".to_string());
                } else if key == "git_sha" {
                    // Absent (null) outside a git checkout, a short revision inside one.
                    *field = Value::String("<build>".to_string());
                } else {
                    normalize(field, ids);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| normalize(item, ids)),
        Value::String(text) => *text = replace_uuids(text, ids),
        _ => {}
    }
}

fn replace_uuids(text: &str, ids: &mut BTreeMap<String, String>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 36 <= bytes.len() && RunId::parse(&text[i..i + 36]).is_ok() {
            let next = format!("<run-{}>", ids.len() + 1);
            let label = ids.entry(text[i..i + 36].to_string()).or_insert(next);
            out.push_str(label);
            i += 36;
        } else {
            let ch = text[i..].chars().next().expect("char boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn run_context(run_type: RunType) -> RunContext {
    let spec = RunSpec {
        strategy_id: "swarm_consensus".to_string(),
        symbol: "BTCUSDT".to_string(),
        market_type: MarketType::UsdMPerpetual,
        sampling: SamplingConfig::Trades {
            price_bucket_ms: None,
        },
        strategy_config: PolicyConfig::default(),
        execution_context: BacktestConfig::default(),
    };
    let mut ctx = RunContext::new(run_type, &spec, support::TAPE_START_MS);
    ctx.run_id = RunId::parse(FIXED_RUN_ID).expect("fixed run id");
    ctx
}

#[test]
fn the_tape_is_deterministic() {
    let a = support::synthetic_trades(SEED, 5_000);
    let b = support::synthetic_trades(SEED, 5_000);
    assert_eq!(a, b);
    assert_ne!(a, support::synthetic_trades(SEED + 1, 5_000));
    assert!(a.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
}

#[tokio::test]
async fn backtest_recording_rows_are_unchanged() {
    let trades = support::swarm::swarm_trades(DAYS);
    let account = BacktestConfig {
        initial_capital: CAPITAL,
        ..support::swarm::account()
    };
    let config = SwarmConfig {
        allow_short: false,
        ..support::swarm::shared()
    };
    let strategy = SwarmConsensusStrategy::from_config(&config, support::swarm::busy_policy())
        .expect("swarm builds");
    let (metrics, closed) = tick_run(account, strategy, &trades).await;

    let ctx = run_context(RunType::Backtest).with_fill_model(FillModel::Tick);
    let mut rows = Value::Array(vec![serde_json::json!({
        "strategy": "swarm_consensus",
        "metrics": ctx.metrics_row("full", &metrics, None, 0),
        "trades": ctx.trade_rows(&closed, 1),
        "equity": ctx.equity_points(&closed, 1),
    })]);
    normalize(&mut rows, &mut BTreeMap::new());
    assert_golden("recording_backtest_rows", &rows);
}
