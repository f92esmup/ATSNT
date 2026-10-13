use backtest::{BacktestConfig, WalkForwardConfig, WalkForwardOptimizer, WalkForwardSplitter};
use domain::DollarBar;
use rust_decimal_macros::dec;
use strategies::DollarBarsCusumStrategy;

fn synthetic_bars(count: usize) -> Vec<DollarBar> {
    let mut price = dec!(40000);
    (0..count)
        .map(|i| {
            let delta = if i % 7 < 3 { dec!(90) } else { dec!(-60) };
            price += delta;
            let end_time = 1_700_000_000_000 + (i as i64) * 60_000;
            DollarBar {
                start_time: end_time - 60_000,
                end_time,
                open: price - delta,
                high: price + dec!(40),
                low: price - dec!(40),
                close: price,
                volume: dec!(10),
                dollar_volume: dec!(400000),
                trade_count: 50,
            }
        })
        .collect()
}

#[test]
fn each_candidate_keeps_full_in_and_out_of_sample_metrics_per_fold() {
    let bars = synthetic_bars(600);
    let folds = WalkForwardSplitter::generate_rolling_folds(
        bars.len(),
        &WalkForwardConfig {
            num_folds: 3,
            train_ratio: 0.7,
            embargo_bars: 5,
        },
    )
    .unwrap();
    let optimizer = WalkForwardOptimizer::<DollarBarsCusumStrategy>::with_default_space(
        BacktestConfig::default(),
    );

    let evaluations = optimizer.run_optimization(&bars, &folds, 4).unwrap();

    assert!(!evaluations.is_empty());
    for eval in &evaluations {
        assert_eq!(eval.fold_metrics.len(), folds.len());
        let fold_ids: Vec<usize> = eval.fold_metrics.iter().map(|f| f.fold_idx).collect();
        let expected: Vec<usize> = folds.iter().map(|f| f.fold_idx).collect();
        assert_eq!(fold_ids, expected);
        let oos_trades: usize = eval
            .fold_metrics
            .iter()
            .map(|f| f.out_of_sample.total_trades)
            .sum();
        assert_eq!(oos_trades, eval.total_oos_trades);
        let mean_is = eval
            .fold_metrics
            .iter()
            .map(|f| f.in_sample.sortino_ratio)
            .sum::<rust_decimal::Decimal>()
            / rust_decimal::Decimal::from(folds.len());
        assert_eq!(mean_is, eval.mean_is_sortino);
    }
}
