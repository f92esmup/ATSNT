//! A strategy can close its own position, and both routes honour the decision alike.
//!
//! The strategy below reads its position from the view it is given: flat, it buys; holding, it
//! closes half an hour after it first saw the position. Both routes must close every trade with
//! the `SignalExit` label, long before the far barriers would.

use backtest::{BacktestConfig, BacktestEngine, ClosedTrade, PriceEvent, TickSimulationConfig};
use domain::futures::FundingSchedule;
use domain::{Decision, MarketEvent, MarketType, OrderIntent, Side, StrategyView, Trade};
use rust_decimal_macros::dec;
use strategies::Strategy;

/// 2026-01-01T00:00:00Z.
const START_MS: i64 = 1_767_225_600_000;
const MINUTE_MS: i64 = 60_000;
const HOLD_MS: i64 = 30 * MINUTE_MS;
/// Round trips the strategy makes before it stops.
const ROUND_TRIPS: usize = 2;

/// Buys when flat and closes half an hour after it first sees the position.
#[derive(Clone, Default)]
struct InAndOut {
    entries: usize,
    seen_open_at: Option<i64>,
}

impl Strategy for InAndOut {
    fn name(&self) -> &str {
        "in_and_out"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn on_event(&mut self, event: &MarketEvent<'_>, view: &StrategyView, out: &mut Vec<Decision>) {
        let MarketEvent::Trade { trade, .. } = event else {
            return;
        };
        if view.position.is_some() {
            let since = *self.seen_open_at.get_or_insert(trade.timestamp);
            if trade.timestamp - since >= HOLD_MS {
                out.push(Decision::exit());
            }
            return;
        }
        self.seen_open_at = None;
        if view.is_flat() && self.entries < ROUND_TRIPS {
            let intent = OrderIntent::new(
                trade.timestamp,
                Side::Buy,
                trade.price,
                trade.price * dec!(0.5),
                trade.price * dec!(2),
                // A day: the barriers never close these trades.
                86_400_000,
            );
            if let Ok(intent) = intent {
                self.entries += 1;
                out.push(Decision::Enter(intent));
            }
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Four hours of flat trades ten seconds apart, alternating the aggressor.
fn tape() -> Vec<Trade> {
    (0..4 * 60 * 6)
        .map(|i| {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            Trade::new(START_MS + i * 10_000, dec!(100), dec!(100), side).expect("valid")
        })
        .collect()
}

fn bar_trades(tape: &[Trade]) -> Vec<ClosedTrade> {
    let mut engine = BacktestEngine::new(BacktestConfig::default());
    let mut strategy = InAndOut::default();
    for trade in tape {
        engine.process_event(&PriceEvent::from(trade), &mut |view, out| {
            strategy.on_event(
                &MarketEvent::trade(&domain::UNNAMED_INSTRUMENT, trade),
                view,
                out,
            )
        });
    }
    engine.finish_with_trades(None).1
}

async fn tick_trades(tape: &[Trade]) -> Vec<ClosedTrade> {
    let backtest = BacktestConfig::default();
    let config = TickSimulationConfig {
        backtest,
        symbol: "BTCUSDT".to_string(),
        latency_ms: 0,
        funding: FundingSchedule::constant(backtest.funding_rate_pct),
    };
    let mut simulation = backtest::wiring::tick_simulation(config, InAndOut::default())
        .expect("tick simulation builds");
    for trade in tape {
        simulation.on_trade(trade).await;
    }
    simulation.finish().1
}

#[tokio::test]
async fn both_routes_close_on_the_strategy_signal() {
    let tape = tape();

    let bar = bar_trades(&tape);
    let tick = tick_trades(&tape).await;

    for (route, trades) in [("bar", &bar), ("tick", &tick)] {
        assert_eq!(trades.len(), ROUND_TRIPS, "{route}: {trades:?}");
        for trade in trades.iter() {
            assert_eq!(trade.exit_reason, "SignalExit", "{route}");
            let held = trade.exit_time - trade.entry_time;
            assert!(
                (HOLD_MS..HOLD_MS + MINUTE_MS).contains(&held),
                "{route}: held {held} ms, expected about {HOLD_MS}"
            );
        }
    }
}

#[test]
fn an_exit_while_flat_changes_nothing() {
    let mut engine = BacktestEngine::new(BacktestConfig::default());
    for trade in tape().iter().take(100) {
        engine.process_event(&PriceEvent::from(trade), &mut |view, out| {
            assert!(view.is_flat());
            out.push(Decision::exit());
        });
    }
    let (metrics, trades) = engine.finish_with_trades(None);
    assert!(trades.is_empty());
    assert!(metrics.rejected_orders.is_empty());
}
