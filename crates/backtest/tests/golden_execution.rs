//! Characterisation (golden) tests of the execution layer.
//!
//! They pin, byte for byte, what `dollar_bars_cusum` and `order_flow_imbalance` produce today on
//! a deterministic synthetic tape: closed trades, metrics, rejections, a walk-forward ranking and
//! the rows recorded for run analytics. A refactor of the execution layer must leave every snapshot
//! unchanged (`odd/tasks/swarm-consensus.md`, S10).
//!
//! Snapshots live in `tests/golden/`. To rewrite them after an intended behaviour change, run
//! `UPDATE_GOLDEN=1 cargo test -p backtest --test golden_execution` and review the diff.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use adapters::InMemoryRunRecorder;
use backtest::{
    record_hpo, run_backtest_slice_with_trades, BacktestConfig, BacktestMetrics, ClosedTrade,
    DataWindow, RunContext, RunId, RunSpec, RunType, SamplingConfig, TickSimulationConfig,
    WalkForwardConfig, WalkForwardOptimizer, WalkForwardSplitter,
};
use domain::futures::FundingSchedule;
use domain::{DollarBar, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use serde_json::Value;
use strategies::{
    DollarBarsCusumConfig, DollarBarsCusumSpace, DollarBarsCusumStrategy, MarketType,
    OptimizableStrategy, OrderFlowImbalanceConfig, OrderFlowImbalanceStrategy, Strategy,
};

const SEED: u64 = 7;
const TAPE_TRADES: usize = 60_000;
const DOLLAR_BAR: Decimal = dec!(1_000_000);
/// A prefix of the tape that ends while an order-flow trade is still open, so the bar engine
/// marks it out as `EndOfSimulation`.
const OPEN_AT_END_TRADES: usize = 45_379;
const TICK_LATENCY_MS: i64 = 100;
const FIXED_RUN_ID: &str = "0198b3c0-7a4e-7c21-8f3d-5b2a9c6e1d40";

/// One execution outcome as the snapshots store it.
#[derive(Serialize)]
struct Outcome<'a> {
    metrics: &'a BacktestMetrics,
    trades: &'a [ClosedTrade],
}

/// Account rules of each scenario.
#[derive(Clone, Copy)]
enum Scenario {
    /// Default account and strategy settings.
    Baseline,
    /// Higher risk and a tight monthly limit: the monthly kill switch trips.
    Stress,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Scenario::Baseline => "baseline",
            Scenario::Stress => "stress",
        }
    }

    fn account(self) -> BacktestConfig {
        match self {
            Scenario::Baseline => BacktestConfig::default(),
            Scenario::Stress => BacktestConfig {
                risk_per_trade_pct: dec!(0.05),
                max_monthly_drawdown_pct: dec!(0.03),
                ..BacktestConfig::default()
            },
        }
    }

    fn cusum(self) -> DollarBarsCusumConfig {
        match self {
            Scenario::Baseline => DollarBarsCusumConfig::default(),
            Scenario::Stress => DollarBarsCusumConfig {
                time_barrier_bars: 200,
                z_entry_threshold: dec!(1.5),
                cusum_vol_multiplier: dec!(1.0),
                ..DollarBarsCusumConfig::default()
            },
        }
    }

    fn ofi(self) -> OrderFlowImbalanceConfig {
        OrderFlowImbalanceConfig::default()
    }
}

fn tape() -> Vec<Trade> {
    support::synthetic_trades(SEED, TAPE_TRADES)
}

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

fn bar_run<S: OptimizableStrategy>(
    account: BacktestConfig,
    config: &S::Config,
    bars: &[DollarBar],
) -> (BacktestMetrics, Vec<ClosedTrade>) {
    run_backtest_slice_with_trades::<S>(account, config, bars).expect("bar backtest runs")
}

async fn tick_run<S: Strategy>(
    account: BacktestConfig,
    strategy: S,
    trades: &[Trade],
) -> (BacktestMetrics, Vec<ClosedTrade>) {
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

fn exit_reasons(trades: &[ClosedTrade]) -> BTreeSet<String> {
    trades.iter().map(|t| t.exit_reason.clone()).collect()
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
        strategy_id: "dollar_bars_cusum".to_string(),
        symbol: "BTCUSDT".to_string(),
        market_type: MarketType::UsdMPerpetual,
        sampling: SamplingConfig::DollarBar {
            threshold: DOLLAR_BAR,
        },
        strategy_config: DollarBarsCusumConfig::default(),
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

#[test]
fn bar_engine_outcomes_are_unchanged() {
    let trades = tape();
    let bars = support::dollar_bars(&trades, DOLLAR_BAR);
    for scenario in [Scenario::Baseline, Scenario::Stress] {
        let account = scenario.account();
        let (m, t) = bar_run::<DollarBarsCusumStrategy>(account, &scenario.cusum(), &bars);
        assert_golden(
            &format!("bar_cusum_{}", scenario.name()),
            &Outcome {
                metrics: &m,
                trades: &t,
            },
        );
        let (m, t) = bar_run::<OrderFlowImbalanceStrategy>(account, &scenario.ofi(), &bars);
        assert_golden(
            &format!("bar_ofi_{}", scenario.name()),
            &Outcome {
                metrics: &m,
                trades: &t,
            },
        );
    }

    let open_at_end = support::dollar_bars(&trades[..OPEN_AT_END_TRADES], DOLLAR_BAR);
    let (m, t) = bar_run::<OrderFlowImbalanceStrategy>(
        BacktestConfig::default(),
        &OrderFlowImbalanceConfig::default(),
        &open_at_end,
    );
    assert_golden(
        "bar_ofi_open_at_end",
        &Outcome {
            metrics: &m,
            trades: &t,
        },
    );
}

#[tokio::test]
async fn tick_simulation_outcomes_are_unchanged() {
    let trades = tape();
    for scenario in [Scenario::Baseline, Scenario::Stress] {
        let account = scenario.account();
        let cusum = DollarBarsCusumStrategy::new(scenario.cusum()).expect("cusum builds");
        let (m, t) = tick_run(account, cusum, &trades).await;
        assert_golden(
            &format!("tick_cusum_{}", scenario.name()),
            &Outcome {
                metrics: &m,
                trades: &t,
            },
        );
        let ofi = OrderFlowImbalanceStrategy::new(scenario.ofi()).expect("ofi builds");
        let (m, t) = tick_run(account, ofi, &trades).await;
        assert_golden(
            &format!("tick_ofi_{}", scenario.name()),
            &Outcome {
                metrics: &m,
                trades: &t,
            },
        );
    }
}

fn hpo_space() -> DollarBarsCusumSpace {
    DollarBarsCusumSpace {
        rolling_window_lens: vec![20, 40],
        cusum_vol_multipliers: vec![dec!(1.0), dec!(2.0)],
        z_entry_thresholds: vec![dec!(1.5), dec!(2.0)],
        z_stop_thresholds: vec![dec!(3.5)],
        time_barrier_bars: vec![15],
        min_take_profit_pcts: vec![dec!(0.0025)],
    }
}

#[tokio::test]
async fn walk_forward_ranking_and_its_recording_are_unchanged() {
    let bars = support::dollar_bars(&tape(), DOLLAR_BAR);
    let folds = WalkForwardSplitter::generate_rolling_folds(
        bars.len(),
        &WalkForwardConfig {
            num_folds: 2,
            train_ratio: 0.7,
            embargo_bars: 10,
        },
    )
    .expect("folds");
    let optimizer = WalkForwardOptimizer::<DollarBarsCusumStrategy>::new(
        BacktestConfig::default(),
        hpo_space(),
    )
    .with_min_trades_per_fold(1);
    let ranking = optimizer
        .run_optimization(&bars, &folds, 6)
        .expect("optimization runs");
    assert_golden("hpo_cusum_ranking", &ranking);

    let recorder = InMemoryRunRecorder::new();
    let parent_spec = RunSpec {
        strategy_id: "dollar_bars_cusum".to_string(),
        symbol: "BTCUSDT".to_string(),
        market_type: MarketType::UsdMPerpetual,
        sampling: SamplingConfig::DollarBar {
            threshold: DOLLAR_BAR,
        },
        strategy_config: hpo_space(),
        execution_context: BacktestConfig::default(),
    };
    let parent = run_context(RunType::Hpo);
    record_hpo(
        &recorder,
        &parent,
        &parent_spec,
        &DataWindow::default(),
        &ranking,
        serde_json::json!({"folds": 2}),
        0,
    )
    .await
    .expect("hpo recorded");
    let recorded = recorder.snapshot();
    let mut rows = serde_json::json!({
        "runs": recorded.runs,
        "metrics": recorded.metrics,
    });
    normalize(&mut rows, &mut BTreeMap::new());
    assert_golden("recording_hpo_rows", &rows);
}

#[test]
fn backtest_recording_rows_are_unchanged() {
    let bars = support::dollar_bars(&tape(), DOLLAR_BAR);
    let ctx = run_context(RunType::Backtest);
    let mut rows = Vec::new();
    for (name, (metrics, trades)) in [
        (
            "cusum",
            bar_run::<DollarBarsCusumStrategy>(
                BacktestConfig::default(),
                &DollarBarsCusumConfig::default(),
                &bars,
            ),
        ),
        (
            "ofi",
            bar_run::<OrderFlowImbalanceStrategy>(
                BacktestConfig::default(),
                &OrderFlowImbalanceConfig::default(),
                &bars,
            ),
        ),
    ] {
        rows.push(serde_json::json!({
            "strategy": name,
            "metrics": ctx.metrics_row("full", &metrics, None, 0),
            "trades": ctx.trade_rows(&trades, 1),
            "equity": ctx.equity_points(&trades, 1),
        }));
    }
    let mut rows = Value::Array(rows);
    normalize(&mut rows, &mut BTreeMap::new());
    assert_golden("recording_backtest_rows", &rows);
}

/// The snapshots only protect the branches they reach: every exit the strategies can take on
/// the bar engine and the tick path must appear in at least one of them.
#[tokio::test]
async fn the_goldens_cover_every_reachable_exit_and_rejection() {
    let trades = tape();
    let bars = support::dollar_bars(&trades, DOLLAR_BAR);
    let mut bar_reasons = BTreeSet::new();
    let mut tick_reasons = BTreeSet::new();
    let mut rejections = BTreeSet::new();
    for scenario in [Scenario::Baseline, Scenario::Stress] {
        let account = scenario.account();
        for (m, t) in [
            bar_run::<DollarBarsCusumStrategy>(account, &scenario.cusum(), &bars),
            bar_run::<OrderFlowImbalanceStrategy>(account, &scenario.ofi(), &bars),
        ] {
            bar_reasons.extend(exit_reasons(&t));
            rejections.extend(m.rejected_orders.keys().cloned());
        }
        let cusum = DollarBarsCusumStrategy::new(scenario.cusum()).expect("cusum builds");
        tick_reasons.extend(exit_reasons(&tick_run(account, cusum, &trades).await.1));
        let ofi = OrderFlowImbalanceStrategy::new(scenario.ofi()).expect("ofi builds");
        tick_reasons.extend(exit_reasons(&tick_run(account, ofi, &trades).await.1));
    }
    let open_at_end = support::dollar_bars(&trades[..OPEN_AT_END_TRADES], DOLLAR_BAR);
    bar_reasons.extend(exit_reasons(
        &bar_run::<OrderFlowImbalanceStrategy>(
            BacktestConfig::default(),
            &OrderFlowImbalanceConfig::default(),
            &open_at_end,
        )
        .1,
    ));

    for reason in [
        "StopLoss",
        "TakeProfit",
        "TimeBarrier",
        "MonthlyCircuitBreaker",
        "EndOfSimulation",
    ] {
        assert!(
            bar_reasons.contains(reason),
            "bar engine never exits by {reason}"
        );
    }
    for reason in [
        "StopLoss",
        "TakeProfit",
        "TimeBarrier",
        "MonthlyCircuitBreaker",
    ] {
        assert!(
            tick_reasons.contains(reason),
            "tick path never exits by {reason}"
        );
    }
    for kind in ["DailyHalt", "OrderSizeExceeded"] {
        assert!(
            rejections.contains(kind),
            "no golden rejects an order by {kind}"
        );
    }
}
