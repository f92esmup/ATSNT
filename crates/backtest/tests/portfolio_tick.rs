//! The tick route on several instruments.
//!
//! On one instrument the portfolio simulation must reproduce the single-instrument simulation:
//! the session is the same, only told the state of an account it alone trades. On two, the
//! account's limits apply across the sessions.

mod support;

use std::collections::BTreeMap;

use backtest::{BacktestConfig, BacktestMetrics, ClosedTrade, TickSimulationConfig};
use domain::futures::FundingSchedule;
use domain::{
    Decision, InstrumentId, MarketEvent, MarketType, OrderIntent, Side, StrategyView, Trade,
};
use rust_decimal_macros::dec;
use strategies::{OptimizableStrategy, Strategy, SwarmConfig, SwarmConsensusStrategy};
use support::swarm::{account, busy_policy, shared, swarm_trades};

const BTC: InstrumentId = InstrumentId::from_static("BTCUSDT");
const ETH: InstrumentId = InstrumentId::from_static("ETHUSDT");

fn swarm(allow_short: bool) -> SwarmConsensusStrategy {
    SwarmConsensusStrategy::from_config(
        &SwarmConfig {
            allow_short,
            ..shared()
        },
        busy_policy(),
    )
    .expect("swarm builds")
}

fn config(backtest: BacktestConfig) -> TickSimulationConfig {
    TickSimulationConfig {
        backtest,
        symbol: "BTCUSDT".to_string(),
        latency_ms: 100,
        funding: FundingSchedule::constant(backtest.funding_rate_pct),
    }
}

async fn single_run(allow_short: bool, trades: &[Trade]) -> (BacktestMetrics, Vec<ClosedTrade>) {
    let mut simulation = backtest::wiring::tick_simulation(config(account()), swarm(allow_short))
        .expect("tick simulation builds");
    for trade in trades {
        simulation.on_trade(trade).await;
    }
    simulation.finish()
}

async fn portfolio_run(allow_short: bool, trades: &[Trade]) -> (BacktestMetrics, Vec<ClosedTrade>) {
    let mut simulation = backtest::wiring::portfolio_tick_simulation(
        config(account()),
        BTreeMap::from([(BTC, swarm(allow_short))]),
    )
    .expect("portfolio simulation builds");
    for trade in trades {
        simulation.on_trade(&BTC, trade).await;
    }
    simulation.finish()
}

#[tokio::test]
async fn on_one_instrument_the_portfolio_simulation_is_the_single_simulation() {
    let trades = swarm_trades(90);
    for allow_short in [true, false] {
        let (single_metrics, single_trades) = single_run(allow_short, &trades).await;
        let (metrics, mut trades) = portfolio_run(allow_short, &trades).await;

        assert!(!single_trades.is_empty(), "the tape must trade");
        for trade in &mut trades {
            assert_eq!(trade.instrument.take(), Some(BTC));
        }
        assert_eq!(metrics, single_metrics, "allow_short = {allow_short}");
        assert_eq!(trades, single_trades, "allow_short = {allow_short}");
    }
}

/// Buys its instrument whenever it is flat there, with far barriers.
struct BuyWhenFlat;

impl Strategy for BuyWhenFlat {
    fn name(&self) -> &str {
        "buy_when_flat"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn on_event(&mut self, event: &MarketEvent<'_>, view: &StrategyView, out: &mut Vec<Decision>) {
        let MarketEvent::Trade { trade, .. } = event else {
            return;
        };
        if view.is_flat() {
            out.extend(
                OrderIntent::new(
                    trade.timestamp,
                    Side::Buy,
                    trade.price,
                    trade.price * dec!(0.9),
                    trade.price * dec!(1.1),
                    86_400_000,
                )
                .ok()
                .map(Decision::Enter),
            );
        }
    }

    fn reset(&mut self) {}
}

/// Ten minutes of both instruments, a print of each every second, both aggressor sides.
fn two_instrument_tape() -> Vec<(InstrumentId, Trade)> {
    (0..600)
        .flat_map(|i| {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            let at = 1_767_225_600_000 + i * 1_000;
            [
                (BTC, Trade::new(at, dec!(100), dec!(10), side).unwrap()),
                (ETH, Trade::new(at + 1, dec!(10), dec!(100), side).unwrap()),
            ]
        })
        .collect()
}

async fn run_two(
    max_open_positions: Option<usize>,
) -> backtest::wiring::SimulatedPortfolioTickSimulation<BuyWhenFlat> {
    let backtest = BacktestConfig {
        max_open_positions,
        ..BacktestConfig::default()
    };
    let mut simulation = backtest::wiring::portfolio_tick_simulation(
        config(backtest),
        BTreeMap::from([(BTC, BuyWhenFlat), (ETH, BuyWhenFlat)]),
    )
    .expect("portfolio simulation builds");
    for (id, trade) in two_instrument_tape() {
        simulation.on_trade(&id, &trade).await;
    }
    simulation
}

#[tokio::test]
async fn one_position_at_a_time_holds_only_the_first_instrument() {
    let simulation = run_two(None).await;

    assert!(simulation.open_position(&BTC).is_some());
    assert!(simulation.open_position(&ETH).is_none());
    let (metrics, _) = simulation.finish();
    assert!(metrics.rejected_orders["MaxPositionsExceeded"] > 0);
}

#[tokio::test]
async fn two_positions_hold_both_instruments_on_one_wallet() {
    let simulation = run_two(Some(2)).await;

    assert!(simulation.open_position(&BTC).is_some());
    assert!(simulation.open_position(&ETH).is_some());
    assert!(simulation.wallet_balance() < BacktestConfig::default().initial_capital);
    let (metrics, _) = simulation.finish();
    assert!(!metrics.rejected_orders.contains_key("MaxPositionsExceeded"));
}

#[test]
fn the_swarm_needs_its_warm_up_on_every_route() {
    assert!(SwarmConsensusStrategy::warm_up_ms(&shared()) > 0);
}
