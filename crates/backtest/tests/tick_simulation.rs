//! The `tick_simulation` module of the application core, run on the simulated venue the wiring builds.

use backtest::session::SessionEvent;
use backtest::tick_simulation::TickSimulationConfig;
use backtest::BacktestConfig;
use domain::futures::FundingSchedule;
use domain::OrderIntent;
use domain::{Side, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use strategies::Strategy;
use strategies::{MarketEvent, MarketType};

/// Emits one scripted decision per trade.
struct Scripted {
    decisions: VecDeque<Option<OrderIntent>>,
}

impl Strategy for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent> {
        let MarketEvent::Trade(_) = event else {
            return None;
        };
        self.decisions.pop_front().flatten()
    }

    fn reset(&mut self) {}
}

fn long_signal() -> Option<OrderIntent> {
    // An hour: the time barrier plays no part in these tests.
    Some(OrderIntent::new(1, Side::Buy, dec!(100), dec!(98), dec!(104), 3_600_000).unwrap())
}

fn simulation(
    decisions: Vec<Option<OrderIntent>>,
) -> backtest::wiring::SimulatedTickSimulation<Scripted> {
    let config = TickSimulationConfig {
        backtest: BacktestConfig {
            initial_capital: dec!(1000),
            taker_fee_pct: dec!(0.001),
            ..BacktestConfig::default()
        },
        symbol: "BTCUSDT".to_string(),
        latency_ms: 0,
        funding: FundingSchedule::constant(dec!(0.0001)),
    };
    backtest::wiring::tick_simulation(
        config,
        Scripted {
            decisions: decisions.into(),
        },
    )
    .unwrap()
}

fn print(timestamp: i64, price: Decimal, quantity: Decimal, side: Side) -> Trade {
    Trade::new(timestamp, price, quantity, side).unwrap()
}

/// Replays the prints through the simulation, one by one.
async fn replay(
    simulation: &mut backtest::wiring::SimulatedTickSimulation<Scripted>,
    ticks: &[Trade],
) {
    for tick in ticks {
        simulation.on_trade(tick).await;
    }
}

fn entry_ticks() -> Vec<Trade> {
    vec![
        print(1_000, dec!(100), dec!(10), Side::Sell),
        print(1_001, dec!(100), dec!(10), Side::Sell),
        print(1_002, dec!(100.2), dec!(10), Side::Buy),
    ]
}

#[tokio::test]
async fn a_signal_becomes_a_position_filled_on_the_tape() {
    let mut simulation = simulation(vec![long_signal()]);

    replay(&mut simulation, &entry_ticks()).await;

    let (side, quantity, entry) = simulation.open_position().unwrap();
    assert_eq!(side, Side::Buy);
    assert_eq!(quantity, dec!(5));
    assert_eq!(entry, dec!(100.2));
    // 5 units at 100.2 with a 0.1% taker fee.
    assert_eq!(simulation.wallet_balance(), dec!(999.499));
}

#[tokio::test]
async fn a_take_profit_closes_the_trade_with_real_fills_and_fees() {
    let mut simulation = simulation(vec![long_signal()]);
    let mut ticks = entry_ticks();
    ticks.extend([
        print(1_003, dec!(103), dec!(10), Side::Buy),
        print(1_004, dec!(104), dec!(1), Side::Buy),
        print(1_005, dec!(103.9), dec!(10), Side::Sell),
    ]);

    replay(&mut simulation, &ticks).await;

    assert!(simulation.open_position().is_none());
    let (metrics, trades) = simulation.finish();
    assert_eq!(trades.len(), 1);
    let trade = &trades[0];
    assert_eq!(trade.exit_reason, "TakeProfit");
    assert_eq!(trade.entry_price, dec!(100.2));
    assert_eq!(trade.exit_price, dec!(103.9));
    assert_eq!(trade.quantity, dec!(5));
    // Commissions on both legs: 5 * 100.2 * 0.001 + 5 * 103.9 * 0.001.
    assert_eq!(trade.fees_paid, dec!(1.0205));
    assert_eq!(metrics.total_trades, 1);
    assert_eq!(metrics.exit_reason_counts.get("TakeProfit"), Some(&1));
}

#[tokio::test]
async fn a_stop_loss_pays_the_slippage_the_tape_charged_after_it_armed() {
    let mut simulation = simulation(vec![long_signal()]);
    let mut ticks = entry_ticks();
    ticks.extend([
        print(1_003, dec!(97.9), dec!(10), Side::Sell),
        print(1_004, dec!(97.4), dec!(10), Side::Sell),
    ]);

    replay(&mut simulation, &ticks).await;

    let (_, trades) = simulation.finish();
    assert_eq!(trades[0].exit_reason, "StopLoss");
    // The stop armed at 97.9 and filled on the next sell print, 0.6 below the trigger.
    assert_eq!(trades[0].exit_price, dec!(97.4));
    assert!(trades[0].pnl_net < Decimal::ZERO);
}

#[tokio::test]
async fn the_time_barrier_closes_at_market_and_is_labelled_as_such() {
    let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(98), dec!(104), 1_004).unwrap();
    assert_eq!(intent.deadline_ms, 1_005);
    let mut simulation = simulation(vec![Some(intent)]);
    let mut ticks = entry_ticks();
    ticks.extend([
        print(1_003, dec!(100.5), dec!(10), Side::Buy),
        print(1_004, dec!(100.5), dec!(10), Side::Buy),
        print(1_005, dec!(100.5), dec!(10), Side::Buy),
        print(1_006, dec!(100.4), dec!(10), Side::Sell),
    ]);

    replay(&mut simulation, &ticks).await;

    assert!(simulation.open_position().is_none());
    let (_, trades) = simulation.finish();
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].exit_reason, "TimeBarrier");
    assert_eq!(trades[0].exit_price, dec!(100.4));
}

#[tokio::test]
async fn an_entry_the_tape_never_fills_is_cancelled_after_the_timeout() {
    let mut simulation = simulation(vec![long_signal(), long_signal()]);
    let mut ticks = vec![
        print(1_000, dec!(100), dec!(10), Side::Sell),
        print(1_001, dec!(100), dec!(10), Side::Sell),
    ];
    // Only sellers trade for longer than the timeout: a market buy finds no offers.
    ticks.push(print(1_001 + 30_000, dec!(100), dec!(1), Side::Sell));

    let mut events = Vec::new();
    for tick in &ticks {
        events.extend(simulation.on_trade(tick).await.events);
    }

    assert!(events
        .iter()
        .any(|e| matches!(e, SessionEvent::EntryTimedOut { .. })));
    assert!(simulation.open_position().is_none());
}

#[tokio::test]
async fn the_wallet_reported_by_the_venue_matches_the_closed_trades() {
    let mut simulation = simulation(vec![long_signal()]);
    let mut ticks = entry_ticks();
    ticks.extend([
        print(1_003, dec!(97.9), dec!(10), Side::Sell),
        print(1_004, dec!(97.4), dec!(10), Side::Sell),
    ]);

    replay(&mut simulation, &ticks).await;

    let wallet = simulation.wallet_balance();
    let (_, trades) = simulation.finish();
    let net: Decimal = trades.iter().map(|trade| trade.pnl_net).sum();
    assert_eq!(wallet, dec!(1000) + net);
}

#[tokio::test]
async fn warming_up_shows_the_strategy_history_without_trading() {
    // The scripted signal is consumed by the warm-up print, so the replay never trades.
    let mut simulation = simulation(vec![long_signal()]);
    let history = vec![print(0, dec!(100), dec!(20), Side::Sell)];

    let seen = simulation.warm_up_trades(history);
    replay(&mut simulation, &entry_ticks()).await;

    assert_eq!(seen, 1);
    assert_eq!(simulation.ticks_processed(), 3);
    assert!(simulation.open_position().is_none());
}

#[tokio::test]
async fn without_signals_nothing_trades() {
    let mut simulation = simulation(vec![None, None]);

    replay(&mut simulation, &entry_ticks()).await;

    assert_eq!(simulation.ticks_processed(), 3);
    let (metrics, trades) = simulation.finish();
    assert!(trades.is_empty());
    assert_eq!(metrics.total_trades, 0);
}

const SETTLEMENT: i64 = domain::futures::FUNDING_INTERVAL_MS * 10;

#[tokio::test]
async fn funding_paid_while_a_position_is_open_is_part_of_its_trade() {
    let mut simulation = simulation(vec![long_signal()]);
    let base = SETTLEMENT - 10_000;
    let ticks = [
        print(base, dec!(100), dec!(10), Side::Sell),
        print(base + 1, dec!(100), dec!(10), Side::Sell),
        print(base + 2, dec!(100.2), dec!(10), Side::Buy),
        // The settlement passes while the long is open.
        print(SETTLEMENT + 1_000, dec!(100.4), dec!(10), Side::Buy),
        print(SETTLEMENT + 2_000, dec!(104), dec!(1), Side::Buy),
        print(SETTLEMENT + 2_001, dec!(103.9), dec!(10), Side::Sell),
    ];

    replay(&mut simulation, &ticks).await;

    let wallet = simulation.wallet_balance();
    let (_, trades) = simulation.finish();
    assert_eq!(trades.len(), 1);
    // 5 units at the last price before the settlement (100.2) with a 0.01% rate.
    assert_eq!(trades[0].funding_paid, dec!(0.0501));
    assert_eq!(
        trades[0].pnl_net,
        trades[0].pnl_gross - trades[0].fees_paid - dec!(0.0501)
    );
    assert_eq!(wallet, dec!(1000) + trades[0].pnl_net);
}

#[tokio::test]
async fn a_liquidation_closes_the_trade_as_liquidated_and_loses_the_margin() {
    let intent =
        OrderIntent::new(1, Side::Buy, dec!(100), dec!(99.9), dec!(104), 3_600_000).unwrap();
    let mut simulation = simulation(vec![Some(intent)]);
    let ticks = [
        print(1_000, dec!(100), dec!(10), Side::Sell),
        print(1_001, dec!(100), dec!(10), Side::Sell),
        print(1_002, dec!(100), dec!(500), Side::Buy),
        // The mark falls through the liquidation price before any stop could trade.
        print(1_003, dec!(91), dec!(1), Side::Sell),
    ];

    replay(&mut simulation, &ticks).await;

    let wallet = simulation.wallet_balance();
    let (metrics, trades) = simulation.finish();
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].exit_reason, "Liquidation");
    assert_eq!(metrics.exit_reason_counts.get("Liquidation"), Some(&1));
    assert_eq!(wallet, dec!(1000) + trades[0].pnl_net);
    assert!(trades[0].pnl_net < dec!(-900));
}
