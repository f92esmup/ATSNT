use backtest::{BacktestConfig, BacktestEngine};
use domain::{DollarBar, OrderIntent, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{MarketType, Strategy};

/// Emits a long intent on the scripted bar end times.
struct ScriptedLong {
    entries: Vec<i64>,
}

impl Strategy for ScriptedLong {
    fn name(&self) -> &str {
        "scripted_long"
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::UsdMPerpetual]
    }

    fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
        self.entries.contains(&bar.end_time).then(|| OrderIntent {
            timestamp: bar.end_time,
            side: Side::Buy,
            price: bar.close,
            stop_loss: bar.close - dec!(20),
            take_profit: bar.close + dec!(25),
            max_bars_hold: 4,
            risk_fraction: None,
            trailing_atr: None,
            breakeven_atr: None,
            deadline_ms: None,
            order_type: domain::OrderType::Market,
            time_in_force_bars: None,
            post_only: false,
        })
    }

    fn reset(&mut self) {}
}

fn bar(end_time: i64, low: Decimal, high: Decimal, close: Decimal) -> DollarBar {
    DollarBar {
        start_time: end_time - 500,
        end_time,
        open: close,
        high,
        low,
        close,
        volume: dec!(1),
        dollar_volume: close,
        buy_volume: dec!(0.5),
        sell_volume: dec!(0.5),
        buy_dollar_volume: close / dec!(2),
        sell_dollar_volume: close / dec!(2),
        trade_count: 1,
    }
}

fn run_scenario() -> (backtest::BacktestMetrics, Vec<backtest::ClosedTrade>) {
    let mut engine = BacktestEngine::new(BacktestConfig::default());
    let mut strategy = ScriptedLong {
        entries: vec![1_000, 6_000],
    };
    let bars = [
        bar(1_000, dec!(100), dec!(100), dec!(100)), // entry #1
        bar(2_000, dec!(109), dec!(111), dec!(110)), // intra-trade high before the dip
        bar(3_000, dec!(94), dec!(96), dec!(95)),    // trough
        bar(4_000, dec!(119), dec!(121), dec!(120)), // peak (MFE)
        bar(5_000, dec!(104), dec!(106), dec!(105)), // time barrier exit
        bar(6_000, dec!(105), dec!(105), dec!(105)), // entry #2
        bar(7_000, dec!(89), dec!(91), dec!(90)),    // trough
        bar(8_000, dec!(129), dec!(131), dec!(130)), // take profit at 130
    ];
    for b in &bars {
        engine.process_bar(&mut strategy, b);
    }
    engine.finish_with_trades(None)
}

#[test]
fn closed_trades_carry_mae_mfe_equity_points() {
    let (_, trades) = run_scenario();
    assert_eq!(trades.len(), 2);

    let first = &trades[0].excursion;
    assert_eq!(first.trough_time, 3_000);
    assert_eq!(first.peak_time, 4_000);
    assert!(first.trough_equity < first.entry_equity);
    assert!(first.peak_equity > first.entry_equity);
    assert!(first.trough_equity <= first.exit_equity && first.exit_equity <= first.peak_equity);

    let second = &trades[1].excursion;
    assert_eq!(second.trough_time, 7_000);
    assert_eq!(
        second.peak_equity, second.exit_equity,
        "take-profit exit is the trade high"
    );
    assert_eq!(second.peak_time, 8_000);
}

#[test]
fn per_trade_drawdowns_reproduce_engine_max_drawdown_exactly() {
    let (metrics, trades) = run_scenario();
    assert!(metrics.max_drawdown_amount > Decimal::ZERO);

    let max_amount = trades
        .iter()
        .map(|t| t.excursion.max_drawdown_amount)
        .max()
        .unwrap();
    let max_pct = trades
        .iter()
        .map(|t| t.excursion.max_drawdown_pct)
        .max()
        .unwrap();
    assert_eq!(max_amount, metrics.max_drawdown_amount);
    assert_eq!(max_pct, metrics.max_drawdown_pct);
}
