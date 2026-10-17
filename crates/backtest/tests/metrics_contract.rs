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
