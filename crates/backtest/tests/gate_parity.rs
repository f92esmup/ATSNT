//! The bar engine and the tick simulation refuse the same signals for the same reasons.
//!
//! Both routes ask the shared `domain::EntryGate`, with the same `DailyBreaker`, so a signal the
//! account rules forbid is counted under the same name on either route. The routes still fill
//! differently (the tick route walks the tape with latency), so only refusals are compared here.

use std::collections::BTreeMap;

use backtest::{BacktestConfig, BacktestEngine, PriceEvent, TickSimulationConfig};
use domain::futures::FundingSchedule;
use domain::{Decision, MarketEvent, MarketType, OrderIntent, Side, StrategyView, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::Strategy;

/// 2026-01-01T00:00:00Z.
const START_MS: i64 = 1_767_225_600_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// Signals `side` on every trade, with a stop `stop_fraction` of the price away.
#[derive(Clone)]
struct EveryTrade {
    side: Side,
    stop_fraction: Decimal,
}

impl EveryTrade {
    fn decide(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent> {
        let MarketEvent::Trade(trade) = event else {
            return None;
        };
        let stop = trade.price * self.stop_fraction;
        let (stop_loss, take_profit) = match self.side {
            Side::Buy => (trade.price - stop, trade.price + stop * dec!(2)),
            Side::Sell => (trade.price + stop, trade.price - stop * dec!(2)),
        };
        OrderIntent::new(
            trade.timestamp,
            self.side,
            trade.price,
            stop_loss,
            take_profit,
            // A year: the time barrier plays no part in these tests.
            365 * 86_400_000,
        )
        .ok()
    }
}

impl Strategy for EveryTrade {
    fn name(&self) -> &str {
        "every_trade"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::Spot, MarketType::UsdMPerpetual]
    }

    fn on_event(&mut self, event: &MarketEvent<'_>, _view: &StrategyView, out: &mut Vec<Decision>) {
        out.extend(self.decide(event).map(Decision::Enter));
    }

    fn reset(&mut self) {}
}

fn trade(at_ms: i64, price: Decimal, side: Side) -> Trade {
    Trade::new(at_ms, price, dec!(100), side).expect("valid trade")
}

/// `count` hourly trades at `price`, alternating the aggressor so both sides find liquidity.
fn flat_tape(count: i64, price: Decimal) -> Vec<Trade> {
    (0..count)
        .map(|i| {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            trade(START_MS + i * HOUR_MS, price, side)
        })
        .collect()
}

/// Refusals of the per-trade bar route.
fn bar_refusals(
    config: BacktestConfig,
    mut strategy: EveryTrade,
    tape: &[Trade],
) -> (BTreeMap<String, usize>, usize) {
    let mut engine = BacktestEngine::new(config);
    for trade in tape {
        engine.process_event(&PriceEvent::from(trade), &mut |view, out| {
            strategy.on_event(&MarketEvent::Trade(trade), view, out)
        });
    }
    let (metrics, trades) = engine.finish_with_trades(tape.last().map(|trade| trade.price));
    (metrics.rejected_orders, trades.len())
}

/// Refusals of the tick route on the simulated venue.
async fn tick_refusals(
    config: BacktestConfig,
    strategy: EveryTrade,
    tape: &[Trade],
) -> (BTreeMap<String, usize>, usize) {
    let config = TickSimulationConfig {
        backtest: config,
        symbol: "BTCUSDT".to_string(),
        latency_ms: 0,
        funding: FundingSchedule::constant(config.funding_rate_pct),
    };
    let mut simulation =
        backtest::wiring::tick_simulation(config, strategy).expect("tick simulation builds");
    for trade in tape {
        simulation.on_trade(trade).await;
    }
    let (metrics, trades) = simulation.finish();
    (metrics.rejected_orders, trades.len())
}

fn only(reason: &str, count: usize) -> BTreeMap<String, usize> {
    BTreeMap::from([(reason.to_string(), count)])
}

#[tokio::test]
async fn an_order_above_the_notional_cap_is_refused_alike_on_both_routes() {
    let tape = flat_tape(48, dec!(100));
    // A stop 0.001 % away sizes a notional far above the default order cap.
    let strategy = EveryTrade {
        side: Side::Buy,
        stop_fraction: dec!(0.00001),
    };
    let config = BacktestConfig::default();

    let bar = bar_refusals(config, strategy.clone(), &tape);
    let tick = tick_refusals(config, strategy, &tape).await;

    assert_eq!(bar, (only("OrderSizeExceeded", 48), 0));
    assert_eq!(tick, bar);
}

#[tokio::test]
async fn a_short_on_spot_is_refused_alike_on_both_routes() {
    let tape = flat_tape(48, dec!(100));
    let strategy = EveryTrade {
        side: Side::Sell,
        stop_fraction: dec!(0.02),
    };
    let config = BacktestConfig::spot();

    let bar = bar_refusals(config, strategy.clone(), &tape);
    let tick = tick_refusals(config, strategy, &tape).await;

    assert_eq!(bar, (only("SpotShortingDisallowed", 48), 0));
    assert_eq!(tick, bar);
}

#[tokio::test]
async fn an_entry_the_balance_cannot_fund_is_sized_down_alike_on_both_routes() {
    // 1 % risked over a 1 % stop asks for the whole balance, which leaves nothing for the fee:
    // the spot defaults used to drop such an entry in silence on both routes.
    let mut tape = flat_tape(3, dec!(100));
    tape.extend((3..12).map(|i| {
        trade(
            START_MS + i * HOUR_MS,
            dec!(103),
            if i % 2 == 0 { Side::Buy } else { Side::Sell },
        )
    }));
    let strategy = EveryTrade {
        side: Side::Buy,
        stop_fraction: dec!(0.01),
    };
    let config = BacktestConfig {
        initial_capital: dec!(1_000),
        ..BacktestConfig::spot()
    };

    let (bar, bar_trades) = bar_refusals(config, strategy.clone(), &tape);
    let (tick, tick_trades) = tick_refusals(config, strategy, &tape).await;

    assert!(bar.is_empty() && tick.is_empty(), "{bar:?} / {tick:?}");
    assert!(bar_trades >= 1, "the bar route trades");
    assert!(tick_trades >= 1, "the tick route trades");
}

#[tokio::test]
async fn an_entry_below_the_smallest_order_the_balance_funds_is_refused_alike_on_both_routes() {
    // 5.02 funds 0.049 units once the fee is left aside: 4.9 of notional, under the 5 minimum.
    let tape = flat_tape(48, dec!(100));
    let strategy = EveryTrade {
        side: Side::Buy,
        stop_fraction: dec!(0.01),
    };
    let config = BacktestConfig {
        initial_capital: dec!(5.02),
        ..BacktestConfig::spot()
    };

    let bar = bar_refusals(config, strategy.clone(), &tape);
    let tick = tick_refusals(config, strategy, &tape).await;

    assert_eq!(bar, (only("InsufficientMargin", 48), 0));
    assert_eq!(tick, bar);
}

/// A day that opens flat at 100, falls to 80 and stays there, then a second day at 80.
fn crash_tape() -> Vec<Trade> {
    let mut tape = Vec::new();
    for hour in 0..24 {
        let price = if hour < 4 { dec!(100) } else { dec!(80) };
        let side = if hour % 2 == 0 { Side::Buy } else { Side::Sell };
        tape.push(trade(START_MS + hour * HOUR_MS, price, side));
    }
    for hour in 0..6 {
        let side = if hour % 2 == 0 { Side::Buy } else { Side::Sell };
        tape.push(trade(START_MS + DAY_MS + hour * HOUR_MS, dec!(80), side));
    }
    tape
}

#[tokio::test]
async fn the_daily_breaker_halts_both_routes_for_the_rest_of_the_day_and_re_arms_the_next() {
    let tape = crash_tape();
    // A 5 % stop risks 1 % of equity on a fifth of it; the fall to 80 gaps through the stop and
    // costs about 4 % of equity, so the 3 % daily limit trips.
    let strategy = EveryTrade {
        side: Side::Buy,
        stop_fraction: dec!(0.05),
    };
    let config = BacktestConfig {
        max_daily_drawdown_pct: dec!(0.03),
        slippage_pct: Decimal::ZERO,
        ..BacktestConfig::default()
    };

    let (bar, bar_trades) = bar_refusals(config, strategy.clone(), &tape);
    let (tick, tick_trades) = tick_refusals(config, strategy, &tape).await;

    for (route, refusals) in [("bar", &bar), ("tick", &tick)] {
        let halted = refusals.get("DailyHalt").copied().unwrap_or(0);
        // The stop-out happens in hour 4; the signals of the rest of that day are refused.
        assert!(
            halted >= 15,
            "{route}: the breaker refuses the day's remaining signals, got {refusals:?}"
        );
        assert_eq!(
            refusals.len(),
            1,
            "{route}: nothing else is refused: {refusals:?}"
        );
    }
    // Both routes stop out on the first day. The counts differ by one because the routes fill at
    // different instants: the tick route fills an entry on the print after the signal, so it has
    // one signal fewer left in the day, and it leaves the second day's position open at the end
    // of the tape where the bar route marks it out.
    assert_eq!(bar, only("DailyHalt", 20));
    assert_eq!(tick, only("DailyHalt", 19));
    assert_eq!((bar_trades, tick_trades), (2, 1));
}
