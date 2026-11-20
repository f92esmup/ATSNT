//! The time barrier is one deadline in milliseconds, and both routes honour it.
//!
//! Before the barrier was unified, the bar route counted price events and the tick route only
//! read an optional deadline, so an entry built with a bar count had no time barrier on the tick
//! route. Here one signal with a three-hour hold must close as `TimeBarrier` on both routes.

use backtest::{BacktestConfig, BacktestEngine, ClosedTrade, PriceEvent, TickSimulationConfig};
use domain::futures::FundingSchedule;
use domain::{Decision, MarketEvent, MarketType, OrderIntent, Side, StrategyView, Trade};
use rust_decimal_macros::dec;
use strategies::Strategy;

/// 2026-01-01T00:00:00Z.
const START_MS: i64 = 1_767_225_600_000;
const HOUR_MS: i64 = 3_600_000;
const HOLD_MS: i64 = 3 * HOUR_MS;

/// Signals one long on the first trade, with a stop and a target the tape never reaches.
#[derive(Clone, Default)]
struct OneLong {
    signalled: bool,
}

impl OneLong {
    fn decide(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent> {
        let MarketEvent::Trade { trade, .. } = event else {
            return None;
        };
        if self.signalled {
            return None;
        }
        self.signalled = true;
        OrderIntent::new(
            trade.timestamp,
            Side::Buy,
            trade.price,
            trade.price * dec!(0.5),
            trade.price * dec!(2),
            HOLD_MS,
        )
        .ok()
    }
}

impl Strategy for OneLong {
    fn name(&self) -> &str {
        "one_long"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn on_event(&mut self, event: &MarketEvent<'_>, _view: &StrategyView, out: &mut Vec<Decision>) {
        out.extend(self.decide(event).map(Decision::Enter));
    }

    fn reset(&mut self) {
        self.signalled = false;
    }
}

/// Spacing of the tape's trades: dense enough for the tick route's market orders to fill before
/// its 30 s entry timeout.
const STEP_MS: i64 = 10_000;

/// Four hours of flat trades, `STEP_MS` apart, alternating the aggressor.
fn tape() -> Vec<Trade> {
    (0..4 * HOUR_MS / STEP_MS)
        .map(|i| {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            Trade::new(START_MS + i * STEP_MS, dec!(100), dec!(100), side).expect("valid")
        })
        .collect()
}

fn bar_trades(tape: &[Trade]) -> Vec<ClosedTrade> {
    let mut engine = BacktestEngine::new(BacktestConfig::default());
    let mut strategy = OneLong::default();
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
    let mut simulation = backtest::wiring::tick_simulation(config, OneLong::default())
        .expect("tick simulation builds");
    for trade in tape {
        simulation.on_trade(trade).await;
    }
    simulation.finish().1
}

#[tokio::test]
async fn both_routes_close_at_the_deadline_as_a_time_barrier() {
    let tape = tape();
    let deadline = START_MS + HOLD_MS;

    let bar = bar_trades(&tape);
    let tick = tick_trades(&tape).await;

    for (route, trades) in [("bar", &bar), ("tick", &tick)] {
        assert_eq!(trades.len(), 1, "{route}: {trades:?}");
        assert_eq!(trades[0].exit_reason, "TimeBarrier", "{route}");
        assert!(
            trades[0].exit_time >= deadline,
            "{route}: closed at {} before the deadline {deadline}",
            trades[0].exit_time
        );
        // Closed at the first trade at or after the deadline, or at the next one that the tick
        // route's market order fills against.
        assert!(
            trades[0].exit_time <= deadline + 2 * STEP_MS,
            "{route}: closed at {}, long after the deadline {deadline}",
            trades[0].exit_time
        );
    }
}
