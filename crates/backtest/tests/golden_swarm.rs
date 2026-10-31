//! Characterisation (golden) tests of the swarm's execution.
//!
//! Two routes run the swarm on the same deterministic tape, each once allowed to short and once
//! long-only: the per-trade route that `backtest` runs for a trade-consuming strategy (the bar
//! fill model over trades, `crates/backtest/src/main.rs`), and the tick-level simulation. The
//! closed trades and metrics of each run are pinned byte for byte.
//!
//! Snapshots live in `tests/golden/swarm_*.json`. To rewrite them after an intended behaviour
//! change, run `UPDATE_GOLDEN=1 cargo test -p backtest --test golden_swarm` and review the diff.
//! This binary is separate from `golden_execution` so that the rewrite touches only swarm files.

mod support;

use std::path::PathBuf;

use backtest::{
    BacktestConfig, BacktestEngine, BacktestMetrics, ClosedTrade, PriceEvent, TickSimulationConfig,
};
use domain::futures::FundingSchedule;
use domain::{PositionSide, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use strategies::{MarketEvent, OptimizableStrategy, Strategy, SwarmConfig, SwarmConsensusStrategy};
use support::swarm::{account, busy_policy, shared, swarm_trades};

/// Ninety days: the swarm makes no trade on a one-day tape; on this one its first trade lands around
/// day 17.
const DAYS: i64 = 90;
const CAPITAL: Decimal = dec!(5000);
const DOLLAR_BAR: Decimal = dec!(1_000_000);
const TICK_LATENCY_MS: i64 = 100;

/// One execution outcome as the snapshots store it.
#[derive(Serialize)]
struct Outcome<'a> {
    metrics: &'a BacktestMetrics,
    trades: &'a [ClosedTrade],
}

/// The shared swarm settings with the given direction rule.
fn swarm_config(allow_short: bool) -> SwarmConfig {
    SwarmConfig {
        allow_short,
        ..shared()
    }
}

/// The swarm's account at the CLI's `--capital 5000`.
fn account_at_capital() -> BacktestConfig {
    BacktestConfig {
        initial_capital: CAPITAL,
        ..account()
    }
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

/// The per-trade route: every trade goes to the engine, which asks the strategy for a decision
/// on each one, as `backtest` does for a trade-consuming strategy.
fn per_trade_run(allow_short: bool) -> (BacktestMetrics, Vec<ClosedTrade>) {
    let trades: Vec<Trade> = swarm_trades(DAYS);
    let mut strategy =
        SwarmConsensusStrategy::from_config(&swarm_config(allow_short), busy_policy())
            .expect("swarm builds");
    let mut engine = BacktestEngine::new(account_at_capital());
    engine.set_bracket_priority(strategy.bracket_priority());
    let mut last_price = Decimal::ZERO;
    for trade in &trades {
        last_price = trade.price;
        engine.process_event(&PriceEvent::from(trade), &mut || {
            strategy.on_event(&MarketEvent::Trade(trade))
        });
        if engine.is_halted() {
            break;
        }
    }
    let mark = (last_price > Decimal::ZERO).then_some(last_price);
    engine.finish_with_trades(mark)
}

/// The tick-level simulation: the same tape through the venue with order latency.
async fn tick_run(allow_short: bool) -> (BacktestMetrics, Vec<ClosedTrade>) {
    let trades: Vec<Trade> = swarm_trades(DAYS);
    let account = account_at_capital();
    let strategy = SwarmConsensusStrategy::from_config(&swarm_config(allow_short), busy_policy())
        .expect("swarm builds");
    let config = TickSimulationConfig {
        backtest: account,
        symbol: "BTCUSDT".to_string(),
        dollar_bar_threshold: DOLLAR_BAR,
        latency_ms: TICK_LATENCY_MS,
        funding: FundingSchedule::constant(account.funding_rate_pct),
    };
    let mut simulation =
        backtest::wiring::tick_simulation(config, strategy).expect("tick simulation builds");
    for trade in &trades {
        simulation.on_trade(trade).await;
    }
    simulation.finish()
}

fn has_short(trades: &[ClosedTrade]) -> bool {
    trades
        .iter()
        .any(|trade| matches!(trade.side, PositionSide::Short))
}

fn pin(name: &str, metrics: &BacktestMetrics, trades: &[ClosedTrade]) {
    assert!(
        !trades.is_empty(),
        "{name}: the swarm closes trades on this tape"
    );
    assert_golden(name, &Outcome { metrics, trades });
}

#[test]
fn per_trade_route_long_only_matches_its_snapshot() {
    let (metrics, trades) = per_trade_run(false);
    assert!(
        !has_short(&trades),
        "a long-only swarm never closes a short"
    );
    pin("swarm_trade_long_only", &metrics, &trades);
}

#[test]
fn per_trade_route_with_shorts_matches_its_snapshot() {
    let (metrics, trades) = per_trade_run(true);
    assert!(
        has_short(&trades),
        "the shorting swarm closes at least one short on this tape"
    );
    pin("swarm_trade_both_sides", &metrics, &trades);
}

#[tokio::test]
async fn tick_route_long_only_matches_its_snapshot() {
    let (metrics, trades) = tick_run(false).await;
    assert!(
        !has_short(&trades),
        "a long-only swarm never closes a short"
    );
    pin("swarm_tick_long_only", &metrics, &trades);
}

#[tokio::test]
async fn tick_route_with_shorts_matches_its_snapshot() {
    let (metrics, trades) = tick_run(true).await;
    assert!(
        has_short(&trades),
        "the shorting swarm closes at least one short on this tape"
    );
    pin("swarm_tick_both_sides", &metrics, &trades);
}
