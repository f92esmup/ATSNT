use backtest::{BacktestMetrics, ClosedTrade, TradeExcursion};
use domain::PositionSide;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn trade(
    pnl_gross: Decimal,
    fees: Decimal,
    reason: &str,
    holding: i64,
    excursion: TradeExcursion,
) -> ClosedTrade {
    ClosedTrade {
        entry_time: 0,
        exit_time: holding * 1000,
        entry_price: dec!(100),
        exit_price: dec!(100),
        quantity: dec!(1),
        side: PositionSide::Long,
        pnl_gross,
        fees_paid: fees,
        funding_paid: Decimal::ZERO,
        slippage_paid: Decimal::ZERO,
        pnl_net: pnl_gross - fees,
        return_pct: (pnl_gross - fees) / dec!(100),
        exit_reason: reason.to_string(),
        holding_duration_seconds: holding,
        excursion,
    }
}

fn excursion(entry: Decimal, trough: Decimal, peak: Decimal) -> TradeExcursion {
    TradeExcursion {
        entry_equity: entry,
        trough_equity: trough,
        peak_equity: peak,
        ..TradeExcursion::default()
    }
}

fn sample_metrics() -> BacktestMetrics {
    let trades = [
        trade(
            dec!(40),
            dec!(4),
            "TakeProfit",
            60,
            excursion(dec!(1000), dec!(990), dec!(1040)),
        ),
        trade(
            dec!(60),
            dec!(6),
            "TakeProfit",
            120,
            excursion(dec!(1036), dec!(1026), dec!(1100)),
        ),
        trade(
            dec!(-20),
            dec!(2),
            "StopLoss",
            30,
            excursion(dec!(1090), dec!(1068), dec!(1090)),
        ),
    ];
    BacktestMetrics::calculate(dec!(1000), &trades, dec!(22), dec!(0.02))
}

#[test]
fn fee_drag_is_share_of_gross_profit_consumed_by_fees() {
    // fees 12 / gross profit (40 + 60) = 0.12
    assert_eq!(sample_metrics().fee_drag_pct, dec!(0.12));
}

#[test]
fn exits_are_counted_by_reason() {
    let m = sample_metrics();
    assert_eq!(m.exit_reason_counts.get("TakeProfit"), Some(&2));
    assert_eq!(m.exit_reason_counts.get("StopLoss"), Some(&1));
    assert_eq!(m.exit_reason_counts.get("TimeBarrier"), None);
}

#[test]
fn average_holding_time_in_seconds() {
    assert_eq!(sample_metrics().avg_holding_seconds, dec!(70));
}

#[test]
fn average_adverse_and_favorable_excursion_relative_to_entry_equity() {
    let m = sample_metrics();
    // MAE: 10/1000, 10/1036, 22/1090 ; MFE: 40/1000, 64/1036, 0/1090
    let expected_mae =
        (dec!(10) / dec!(1000) + dec!(10) / dec!(1036) + dec!(22) / dec!(1090)) / dec!(3);
    let expected_mfe = (dec!(40) / dec!(1000) + dec!(64) / dec!(1036)) / dec!(3);
    assert_eq!(m.avg_mae_pct, expected_mae);
    assert_eq!(m.avg_mfe_pct, expected_mfe);
}

#[test]
fn existing_metrics_are_unchanged() {
    let m = sample_metrics();
    assert_eq!(m.total_trades, 3);
    assert_eq!(m.total_fees, dec!(12));
    assert_eq!(m.net_profit, dec!(68));
    assert_eq!(m.max_drawdown_pct, dec!(0.02));
}

#[test]
fn metrics_without_new_fields_still_deserialize() {
    let legacy = serde_json::json!({
        "total_trades": 0, "winning_trades": 0, "losing_trades": 0,
        "win_rate": "0", "gross_profit": "0", "gross_loss": "0", "net_profit": "0",
        "total_fees": "0", "total_slippage": "0", "profit_factor": "0", "payoff_ratio": "0",
        "mathematical_expectancy": "0", "max_drawdown_amount": "0", "max_drawdown_pct": "0",
        "sortino_ratio": "0"
    });
    let m: BacktestMetrics = serde_json::from_value(legacy).unwrap();
    assert_eq!(m.fee_drag_pct, Decimal::ZERO);
    assert!(m.exit_reason_counts.is_empty());
}

fn equity_trade(pnl_net: Decimal) -> ClosedTrade {
    trade(
        pnl_net,
        Decimal::ZERO,
        "TimeBarrier",
        60,
        excursion(dec!(1000), dec!(1000), dec!(1000)),
    )
}

fn assert_close(actual: Decimal, expected: Decimal) {
    assert!(
        (actual - expected).abs() < dec!(0.000001),
        "{actual} is not {expected}"
    );
}

#[test]
fn sortino_is_the_mean_trade_return_over_the_downside_deviation_of_all_trades() {
    // Returns on entry equity: +2%, -1%, +0.5%. Mean 0.5%; downside deviation
    // sqrt(0.01^2 / 3) over all three trades, so the ratio is sqrt(3) / 2.
    let trades = [
        equity_trade(dec!(20)),
        equity_trade(dec!(-10)),
        equity_trade(dec!(5)),
    ];

    let m = BacktestMetrics::calculate(dec!(1000), &trades, Decimal::ZERO, Decimal::ZERO);

    assert_close(m.sortino_ratio.expect("defined"), dec!(0.8660254));
}

#[test]
fn ratios_are_undefined_instead_of_capped_without_losing_trades() {
    let trades = [equity_trade(dec!(20)), equity_trade(dec!(5))];

    let m = BacktestMetrics::calculate(dec!(1000), &trades, Decimal::ZERO, Decimal::ZERO);

    assert_eq!(m.sortino_ratio, None);
    assert_eq!(m.profit_factor, None);
}

#[test]
fn ratios_are_undefined_without_trades() {
    let m = BacktestMetrics::calculate(dec!(1000), &[], Decimal::ZERO, Decimal::ZERO);

    assert_eq!(m.sortino_ratio, None);
    assert_eq!(m.profit_factor, None);
    assert!(m.rejected_orders.is_empty());
}

#[test]
fn a_trade_without_an_equity_path_is_measured_on_the_initial_capital() {
    let flat = TradeExcursion::default();
    let trades = [
        trade(dec!(20), Decimal::ZERO, "TakeProfit", 60, flat),
        trade(dec!(-10), Decimal::ZERO, "StopLoss", 60, flat),
        trade(dec!(5), Decimal::ZERO, "TakeProfit", 60, flat),
    ];

    let m = BacktestMetrics::calculate(dec!(1000), &trades, Decimal::ZERO, Decimal::ZERO);

    assert_close(m.sortino_ratio.expect("defined"), dec!(0.8660254));
}

#[test]
fn profit_factor_is_gross_profit_over_gross_loss() {
    let trades = [
        equity_trade(dec!(20)),
        equity_trade(dec!(-10)),
        equity_trade(dec!(5)),
    ];

    let m = BacktestMetrics::calculate(dec!(1000), &trades, Decimal::ZERO, Decimal::ZERO);

    assert_eq!(m.profit_factor, Some(dec!(2.5)));
}

#[test]
fn legacy_metrics_keep_their_recorded_ratios() {
    let legacy = serde_json::json!({
        "total_trades": 5, "winning_trades": 5, "losing_trades": 0,
        "win_rate": "1", "gross_profit": "10", "gross_loss": "0", "net_profit": "10",
        "total_fees": "0", "total_slippage": "0", "profit_factor": "100", "payoff_ratio": "0",
        "mathematical_expectancy": "2", "max_drawdown_amount": "0", "max_drawdown_pct": "0",
        "sortino_ratio": "100"
    });
    let m: BacktestMetrics = serde_json::from_value(legacy).unwrap();
    assert_eq!(m.sortino_ratio, Some(dec!(100)));
    assert_eq!(m.profit_factor, Some(dec!(100)));
    assert!(m.rejected_orders.is_empty());
}
