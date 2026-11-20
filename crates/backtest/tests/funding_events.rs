//! Strategies are told of funding settlements on every route, before the trade that crosses
//! them, and what they decide on one is ignored.

use std::collections::BTreeMap;

use backtest::{BacktestConfig, TickSimulationConfig};
use domain::futures::FundingSchedule;
use domain::{Decision, InstrumentId, MarketEvent, MarketType, Side, StrategyView, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::Strategy;

/// 2026-01-01T00:00:00Z, a settlement instant.
const START_MS: i64 = 1_767_225_600_000;
const HOUR_MS: i64 = 3_600_000;

/// Records the funding settlements it is told of, and asks to close on each (to be ignored).
#[derive(Default)]
struct FundingRecorder {
    settlements: Vec<(i64, Decimal)>,
}

impl Strategy for FundingRecorder {
    fn name(&self) -> &str {
        "funding_recorder"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::Spot, MarketType::UsdMPerpetual]
    }

    fn on_event(&mut self, event: &MarketEvent<'_>, _view: &StrategyView, out: &mut Vec<Decision>) {
        if let MarketEvent::Funding { time_ms, rate, .. } = event {
            self.settlements.push((*time_ms, *rate));
            out.push(Decision::exit());
        }
    }

    fn reset(&mut self) {}

    fn audit(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!(self.settlements.len()))
    }
}

/// A day of trades ten minutes apart, starting just after a settlement.
fn day_of_trades() -> Vec<Trade> {
    (1..=144)
        .map(|i| {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            Trade::new(START_MS + i * 600_000, dec!(100), dec!(1), side).unwrap()
        })
        .collect()
}

fn config(market_type: MarketType) -> TickSimulationConfig {
    let backtest = BacktestConfig {
        market_type,
        funding_rate_pct: dec!(0.0001),
        ..BacktestConfig::default()
    };
    TickSimulationConfig {
        backtest,
        symbol: "BTCUSDT".to_string(),
        latency_ms: 0,
        funding: FundingSchedule::constant(backtest.funding_rate_pct),
    }
}

#[tokio::test]
async fn the_tick_route_tells_the_strategy_of_each_settlement() {
    let mut simulation = backtest::wiring::tick_simulation(
        config(MarketType::UsdMPerpetual),
        FundingRecorder::default(),
    )
    .unwrap();
    for trade in day_of_trades() {
        simulation.on_trade(&trade).await;
    }

    assert_eq!(
        simulation.strategy().settlements,
        [
            (START_MS + 8 * HOUR_MS, dec!(0.0001)),
            (START_MS + 16 * HOUR_MS, dec!(0.0001)),
            (START_MS + 24 * HOUR_MS, dec!(0.0001)),
        ]
    );
    let (metrics, trades) = simulation.finish();
    assert!(trades.is_empty(), "a decision on a settlement is ignored");
    assert!(metrics.rejected_orders.is_empty());
}

#[tokio::test]
async fn spot_has_no_funding_to_tell_of() {
    let mut simulation =
        backtest::wiring::tick_simulation(config(MarketType::Spot), FundingRecorder::default())
            .unwrap();
    for trade in day_of_trades() {
        simulation.on_trade(&trade).await;
    }
    assert!(simulation.strategy().settlements.is_empty());
}

#[tokio::test]
async fn the_portfolio_tick_route_tells_each_instrument_of_its_settlements() {
    let btc = InstrumentId::from_static("BTCUSDT");
    let eth = InstrumentId::from_static("ETHUSDT");
    let mut simulation = backtest::wiring::portfolio_tick_simulation(
        config(MarketType::UsdMPerpetual),
        BTreeMap::from([
            (btc.clone(), FundingRecorder::default()),
            (eth.clone(), FundingRecorder::default()),
        ]),
    )
    .unwrap();
    for trade in day_of_trades() {
        simulation.on_trade(&btc, &trade).await;
        simulation.on_trade(&eth, &trade).await;
    }
    let audits = simulation.strategy_audits();
    assert_eq!(audits[&btc], serde_json::json!(3));
    assert_eq!(audits[&eth], serde_json::json!(3));
}

#[test]
fn the_bar_route_of_a_portfolio_tells_the_strategy_through_its_runner() {
    let btc = InstrumentId::from_static("BTCUSDT");
    let engine = backtest::PortfolioEngine::new(BacktestConfig::default(), Vec::new());
    let mut strategies = backtest::PortfolioStrategies::new(std::slice::from_ref(&btc), || {
        Ok::<_, ()>(FundingRecorder::default())
    })
    .unwrap();
    strategies.on_funding(&engine, &btc, START_MS, dec!(0.0002));
    let backtest::PortfolioStrategies::PerInstrument(instances) = strategies else {
        panic!("a recorder runs per instrument");
    };
    assert_eq!(instances[&btc].settlements, [(START_MS, dec!(0.0002))]);
}
