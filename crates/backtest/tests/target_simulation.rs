//! Parity test for target positions between the bar engine and tick simulation (T8.2).
//!
//! A scale-in, a scale-out, a flip, and a stop-out on SimulatedVenue produce the same campaigns
//! as the bar engine on the same synthetic tape within the tick model's documented differences,
//! asserted on side, quantities, and exit reasons.

mod support;

use std::collections::VecDeque;

use backtest::config::BacktestConfig;
use backtest::engine::BacktestEngine;
use backtest::price_event::PriceEvent;
use backtest::tick_simulation::{TickSimulation, TickSimulationConfig};
use domain::futures::FundingSchedule;
use domain::{InputKind, MarketEvent, PositionSide, Side, TargetPosition, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{Decision, MarketType, Strategy};

#[derive(Clone)]
struct ScriptedTargetStrategy {
    targets: VecDeque<(i64, TargetPosition)>,
}

impl Strategy for ScriptedTargetStrategy {
    fn name(&self) -> &str {
        "scripted_target"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn input_kind(&self) -> InputKind {
        InputKind::Trades
    }

    fn emits_targets(&self) -> bool {
        true
    }

    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<Decision> {
        let ts = event.timestamp();
        if let Some((target_ts, _)) = self.targets.front() {
            if ts >= *target_ts {
                let (_, target) = self.targets.pop_front().unwrap();
                return Some(Decision::Target(target));
            }
        }
        None
    }

    fn reset(&mut self) {}
}

fn trade(ts: i64, price: Decimal, qty: Decimal, side: Side) -> Trade {
    Trade {
        timestamp: ts,
        price,
        quantity: qty,
        side,
    }
}

#[tokio::test]
async fn a_scale_in_scale_out_flip_and_stop_out_match_between_bar_and_tick_engine() {
    let mut trades = Vec::new();
    let start_ts = 1_000_000;
    let mut push_trades = |ts: i64, price: Decimal| {
        trades.push(trade(ts, price, dec!(100), Side::Buy));
        trades.push(trade(ts + 1, price, dec!(100), Side::Sell));
    };

    // 10 price points at 100
    for i in 0..10 {
        push_trades(start_ts + i * 100, dec!(100));
    }
    // 10 price points at 100 (scale-in)
    for i in 10..20 {
        push_trades(start_ts + i * 100, dec!(100));
    }
    // 10 price points at 100 (scale-out)
    for i in 20..30 {
        push_trades(start_ts + i * 100, dec!(100));
    }
    // 10 price points at 100 (flip to short)
    for i in 30..40 {
        push_trades(start_ts + i * 100, dec!(100));
    }
    // 10 price points rising to 109 to hit the short stop (100 + 5 = 105)
    for i in 40..50 {
        let price = dec!(100) + Decimal::from(i - 40); // 100..109
        push_trades(start_ts + i * 100, price);
    }

    let script = vec![
        // Open long at level 1: 10_000 * 0.01 / 5 = 20 BTC
        (
            start_ts,
            TargetPosition::new(start_ts, 1, dec!(0.01), dec!(5)).unwrap(),
        ),
        // Scale in to level 2: 10_000 * 0.02 / 5 = 40 BTC
        (
            start_ts + 1000,
            TargetPosition::new(start_ts + 1000, 2, dec!(0.02), dec!(5)).unwrap(),
        ),
        // Scale out to level 1: 10_000 * 0.01 / 5 = 20 BTC
        (
            start_ts + 2000,
            TargetPosition::new(start_ts + 2000, 1, dec!(0.01), dec!(5)).unwrap(),
        ),
        // Flip to short at level -1: -20 BTC
        (
            start_ts + 3000,
            TargetPosition::new(start_ts + 3000, -1, dec!(0.01), dec!(5)).unwrap(),
        ),
    ];

    let config = BacktestConfig {
        initial_capital: dec!(10_000),
        taker_fee_pct: Decimal::ZERO,
        slippage_pct: Decimal::ZERO,
        min_order_quantity: dec!(0.01),
        min_order_notional: dec!(5),
        max_order_notional: Some(dec!(100_000)),
        max_position_notional: Some(dec!(100_000)),
        max_leverage: dec!(10),
        latency_ms: 0,
        ..BacktestConfig::default()
    };

    // 1. Run bar engine
    let mut bar_strat = ScriptedTargetStrategy {
        targets: script.clone().into(),
    };
    let mut bar_engine = BacktestEngine::new(config);
    for t in &trades {
        bar_engine.process_event(&PriceEvent::from(t), &mut || {
            bar_strat.on_event(&MarketEvent::Trade(t))
        });
    }
    let bar_trades = bar_engine.closed_trades().to_vec();

    // 2. Run tick simulation
    let tick_strat = ScriptedTargetStrategy {
        targets: script.into(),
    };
    let mut sim = TickSimulation::new(
        TickSimulationConfig {
            backtest: config,
            symbol: "BTCUSDT".to_string(),
            dollar_bar_threshold: dec!(1_000_000),
            latency_ms: 0,
            funding: FundingSchedule::constant(Decimal::ZERO),
        },
        tick_strat,
    )
    .unwrap();
    for t in &trades {
        sim.on_trade(t).await;
    }
    let tick_trades = sim.closed_trades().to_vec();

    // Assert that both engines produced the exact same 2 campaigns
    assert_eq!(bar_trades.len(), 2, "bar engine trade count");
    assert_eq!(tick_trades.len(), 2, "tick simulation trade count");

    // Campaign 1: Long, peaked at 40 BTC, closed on flip
    assert_eq!(bar_trades[0].side, PositionSide::Long);
    assert_eq!(tick_trades[0].side, PositionSide::Long);
    assert_eq!(bar_trades[0].quantity, dec!(40));
    assert_eq!(tick_trades[0].quantity, dec!(40));
    assert_eq!(bar_trades[0].exit_reason, "TargetFlip");
    assert_eq!(tick_trades[0].exit_reason, "TargetFlip");

    // Campaign 2: Short, 20 BTC, closed on stop-out
    assert_eq!(bar_trades[1].side, PositionSide::Short);
    assert_eq!(tick_trades[1].side, PositionSide::Short);
    assert_eq!(bar_trades[1].quantity, dec!(20));
    assert_eq!(tick_trades[1].quantity, dec!(20));
    assert_eq!(bar_trades[1].exit_reason, "StopLoss");
    assert_eq!(tick_trades[1].exit_reason, "StopLoss");
}
