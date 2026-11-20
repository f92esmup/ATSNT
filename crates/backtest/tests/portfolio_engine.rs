//! The bar route on several instruments.
//!
//! On one instrument the portfolio engine must reproduce the single-instrument engine exactly: it
//! runs the same rules, only keyed by instrument. On two, the account's limits apply across them.

mod support;

use backtest::{
    results_by_instrument, BacktestConfig, BacktestEngine, BacktestMetrics, ClosedTrade,
    PortfolioEngine, PriceEvent,
};
use domain::{
    Decision, InstrumentId, MarketEvent, MarketType, OrderIntent, Side, StrategyView, Trade,
};
use rust_decimal::Decimal;
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

fn single_run(allow_short: bool, trades: &[Trade]) -> (BacktestMetrics, Vec<ClosedTrade>) {
    let mut strategy = swarm(allow_short);
    let mut engine = BacktestEngine::new(account());
    for trade in trades {
        engine.process_event(&PriceEvent::from(trade), &mut |view, out| {
            strategy.on_event(&MarketEvent::trade(&BTC, trade), view, out)
        });
        if engine.is_halted() {
            break;
        }
    }
    engine.finish_with_trades(trades.last().map(|trade| trade.price))
}

fn portfolio_run(allow_short: bool, trades: &[Trade]) -> (BacktestMetrics, Vec<ClosedTrade>) {
    let mut strategy = swarm(allow_short);
    let mut engine = PortfolioEngine::new(account(), Vec::new());
    for trade in trades {
        engine.process_event(&BTC, &PriceEvent::from(trade), &mut |view, out| {
            strategy.on_event(&MarketEvent::trade(&BTC, trade), view, out)
        });
        if engine.is_halted() {
            break;
        }
    }
    engine.finish_with_trades(true)
}

#[test]
fn on_one_instrument_the_portfolio_engine_is_the_single_engine() {
    let trades = swarm_trades(90);
    for allow_short in [true, false] {
        let (single_metrics, single_trades) = single_run(allow_short, &trades);
        let (metrics, mut trades) = portfolio_run(allow_short, &trades);

        assert!(!single_trades.is_empty(), "the tape must trade");
        assert!(trades
            .iter()
            .all(|trade| trade.instrument.as_ref() == Some(&BTC)));
        for trade in &mut trades {
            trade.instrument = None;
        }
        assert_eq!(metrics, single_metrics, "allow_short = {allow_short}");
        assert_eq!(trades, single_trades, "allow_short = {allow_short}");
    }
}

/// Buys every instrument it sees while flat there, with far barriers.
struct BuyEverything;

impl Strategy for BuyEverything {
    fn name(&self) -> &str {
        "buy_everything"
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

/// Interleaved flat prints of both instruments, ten seconds apart.
fn two_instrument_tape() -> Vec<(InstrumentId, Trade)> {
    (0..60)
        .map(|i| {
            let (id, price) = if i % 2 == 0 {
                (BTC, dec!(100))
            } else {
                (ETH, dec!(10))
            };
            let side = if i % 4 < 2 { Side::Buy } else { Side::Sell };
            let trade =
                Trade::new(1_767_225_600_000 + i * 10_000, price, dec!(10), side).expect("valid");
            (id, trade)
        })
        .collect()
}

fn run_two(config: BacktestConfig) -> (BacktestMetrics, Vec<ClosedTrade>, PortfolioEngine) {
    let mut strategy = BuyEverything;
    let mut engine = PortfolioEngine::new(config, Vec::new());
    for (id, trade) in two_instrument_tape() {
        engine.process_event(&id, &PriceEvent::from(&trade), &mut |view, out| {
            strategy.on_event(&MarketEvent::trade(&id, &trade), view, out)
        });
    }
    let snapshot = engine.clone();
    let (metrics, trades) = engine.finish_with_trades(true);
    (metrics, trades, snapshot)
}

#[test]
fn one_position_at_a_time_refuses_the_second_instrument() {
    let (metrics, trades, engine) = run_two(BacktestConfig::default());

    assert!(engine.position(&BTC).is_some());
    assert!(engine.position(&ETH).is_none());
    assert_eq!(trades.len(), 1, "only BTC, closed at the end");
    assert!(metrics.rejected_orders["MaxPositionsExceeded"] > 0);
}

#[test]
fn two_positions_trade_both_instruments_on_one_cash_balance() {
    let config = BacktestConfig {
        max_open_positions: Some(2),
        ..BacktestConfig::default()
    };
    let (metrics, trades, engine) = run_two(config);

    assert!(engine.position(&BTC).is_some());
    assert!(engine.position(&ETH).is_some());
    assert!(!metrics.rejected_orders.contains_key("MaxPositionsExceeded"));
    let by_instrument = results_by_instrument(&trades);
    assert_eq!(by_instrument.len(), 2);
    let net: Decimal = by_instrument.values().map(|(_, pnl)| *pnl).sum();
    assert_eq!(metrics.net_profit, net);
    // The cash balance paid both entry fees before the end.
    assert!(engine.cash_equity() < config.initial_capital);
}

/// Sees both instruments; holding BTC, it closes BTC from an ETH event.
struct CrossClose {
    closed: bool,
}

impl Strategy for CrossClose {
    fn name(&self) -> &str {
        "cross_close"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn scope(&self) -> strategies::StrategyScope {
        strategies::StrategyScope::CrossSectional
    }

    fn on_event(&mut self, event: &MarketEvent<'_>, view: &StrategyView, out: &mut Vec<Decision>) {
        let MarketEvent::Trade { instrument, trade } = event else {
            return;
        };
        if *instrument == &BTC && view.is_flat() && !self.closed {
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
        let holds_btc = view.others.iter().any(|held| held.instrument == BTC);
        if *instrument == &ETH && holds_btc && !self.closed {
            self.closed = true;
            out.push(Decision::exit_of(BTC));
        }
    }

    fn reset(&mut self) {}
}

#[test]
fn a_cross_sectional_strategy_closes_one_instrument_from_another() {
    let mut strategy = CrossClose { closed: false };
    let mut engine = PortfolioEngine::new(BacktestConfig::default(), Vec::new());
    for (id, trade) in two_instrument_tape() {
        engine.process_event(&id, &PriceEvent::from(&trade), &mut |view, out| {
            strategy.on_event(&MarketEvent::trade(&id, &trade), view, out)
        });
    }
    let (_, trades) = engine.finish_with_trades(true);

    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].instrument.as_ref(), Some(&BTC));
    assert_eq!(trades[0].exit_reason, "SignalExit");
}

#[test]
fn the_swarm_declares_itself_per_instrument() {
    assert_eq!(
        swarm(true).scope(),
        strategies::StrategyScope::PerInstrument
    );
    assert!(SwarmConsensusStrategy::warm_up_ms(&shared()) > 0);
}
