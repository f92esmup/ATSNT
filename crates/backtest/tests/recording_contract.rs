use backtest::{
    BacktestConfig, BacktestMetrics, ClosedTrade, DataWindow, RunContext, RunSpec, RunStatus,
    RunType, SamplingConfig, TradeExcursion,
};
use domain::PositionSide;
use rust_decimal_macros::dec;
use strategies::{DollarBarsCusumConfig, MarketType};

fn spec() -> RunSpec<DollarBarsCusumConfig> {
    RunSpec {
        strategy_id: "dollar_bars_cusum".to_string(),
        symbol: "BTCUSDT".to_string(),
        market_type: MarketType::UsdMPerpetual,
        sampling: SamplingConfig::DollarBar {
            threshold: dec!(1_000_000),
        },
        strategy_config: DollarBarsCusumConfig::default(),
        execution_context: BacktestConfig::default(),
    }
}

fn closed_trade() -> ClosedTrade {
    ClosedTrade {
        entry_time: 1_700_000_000_000,
        exit_time: 1_700_000_060_000,
        entry_price: dec!(100),
        exit_price: dec!(110),
        quantity: dec!(2),
        side: PositionSide::Long,
        pnl_gross: dec!(20),
        fees_paid: dec!(1),
        slippage_paid: dec!(0.5),
        pnl_net: dec!(18.5),
        return_pct: dec!(0.0925),
        exit_reason: "TakeProfit".to_string(),
        holding_duration_seconds: 60,
        excursion: TradeExcursion {
            entry_equity: dec!(9999),
            trough_equity: dec!(9990),
            trough_time: 1_700_000_020_000,
            peak_equity: dec!(10_020),
            peak_time: 1_700_000_050_000,
            exit_equity: dec!(10_018.5),
            max_drawdown_amount: dec!(10),
            max_drawdown_pct: dec!(0.001),
        },
    }
}

#[test]
fn run_row_records_full_spec_and_lineage() {
    let parent = RunContext::new(RunType::Backtest, &spec(), 1_700_000_000_000);
    let child = parent.child(RunType::MonteCarlo, 1_700_000_100_000);
    let window = DataWindow {
        source: Some("data/historical_2026/BTCUSDT".to_string()),
        start_ms: Some(1_700_000_000_000),
        end_ms: Some(1_700_000_060_000),
    };

    let row = child
        .run_row(
            &spec(),
            RunStatus::Completed,
            Some(1_700_000_200_000),
            &window,
        )
        .unwrap();

    assert_eq!(row.run_id, child.run_id.as_str());
    assert_eq!(row.parent_run_id.as_deref(), Some(parent.run_id.as_str()));
    assert_eq!(row.run_type, "monte_carlo");
    assert_eq!(row.status, "completed");
    assert_eq!(row.market_type, "usd_m_perpetual");
    assert!(row.finished_at.is_some());
    assert_eq!(
        row.data_source.as_deref(),
        Some("data/historical_2026/BTCUSDT")
    );
    let params: serde_json::Value = serde_json::from_str(&row.params).unwrap();
    assert_eq!(params["rolling_window_len"], 20);
    let ctx: serde_json::Value = serde_json::from_str(&row.execution_context).unwrap();
    assert_eq!(ctx["taker_fee_pct"], "0.0005");
    let sampling: serde_json::Value = serde_json::from_str(&row.sampling).unwrap();
    assert_eq!(sampling["type"], "dollar_bar");
}

#[test]
fn metrics_row_maps_every_metric_and_scope() {
    let ctx = RunContext::new(RunType::Backtest, &spec(), 0);
    let metrics =
        BacktestMetrics::calculate(dec!(10_000), &[closed_trade()], dec!(10), dec!(0.001));

    let row = ctx.metrics_row("full", &metrics, Some(serde_json::json!({"k": 1})));

    assert_eq!(row.scope, "full");
    assert_eq!(row.run_type, "backtest");
    assert_eq!(row.total_trades, Some(1));
    assert_eq!(row.net_profit, Some(dec!(18.5)));
    assert_eq!(row.max_drawdown_pct, Some(dec!(0.001)));
    assert_eq!(row.fee_drag_pct, metrics_fee_drag(&metrics));
    let exits: serde_json::Value =
        serde_json::from_str(row.exit_reason_counts.as_deref().unwrap()).unwrap();
    assert_eq!(exits["TakeProfit"], 1);
    assert_eq!(row.extras.as_deref(), Some("{\"k\":1}"));
}

fn metrics_fee_drag(m: &BacktestMetrics) -> Option<rust_decimal::Decimal> {
    Some(m.fee_drag_pct)
}

#[test]
fn trade_rows_carry_run_identity_and_excursion() {
    let ctx = RunContext::new(RunType::Paper, &spec(), 0);
    let rows = ctx.trade_rows(&[closed_trade(), closed_trade()], 7);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].trade_id, format!("{}_t00007", ctx.run_id));
    assert_eq!(rows[1].trade_id, format!("{}_t00008", ctx.run_id));
    assert_eq!(rows[0].run_type, "paper");
    assert_eq!(rows[0].side, "Long");
    assert_eq!(rows[0].trough_equity, dec!(9990));
    assert_eq!(rows[0].peak_equity, dec!(10_020));
    assert_eq!(rows[0].max_drawdown_pct, dec!(0.001));
}

#[test]
fn equity_points_are_four_events_per_trade_in_time_order() {
    let ctx = RunContext::new(RunType::Backtest, &spec(), 0);
    let points = ctx.equity_points(&[closed_trade()], 1);

    let events: Vec<(&str, rust_decimal::Decimal)> =
        points.iter().map(|p| (p.event.as_str(), p.value)).collect();
    assert_eq!(
        events,
        [
            ("entry", dec!(9999)),
            ("trough", dec!(9990)),
            ("peak", dec!(10_020)),
            ("exit", dec!(10_018.5)),
        ]
    );
    assert!(points.iter().all(|p| p.series == "equity"));
    assert!(points
        .iter()
        .all(|p| p.trade_id.as_deref() == Some(format!("{}_t00001", ctx.run_id).as_str())));
    let times: Vec<_> = points.iter().map(|p| p.ts.clone().unwrap()).collect();
    let mut sorted = times.clone();
    sorted.sort();
    assert_eq!(times, sorted);
}

mod persistence {
    use super::*;
    use adapters::InMemoryRunRecorder;
    use backtest::{
        record_backtest, record_monte_carlo, MonteCarloConfig, MonteCarloSimulator, ResampleMethod,
    };

    fn trades(n: usize) -> Vec<ClosedTrade> {
        (0..n)
            .map(|i| {
                let mut t = closed_trade();
                if i % 3 == 0 {
                    t.pnl_net = dec!(-5);
                    t.return_pct = dec!(-0.025);
                }
                t
            })
            .collect()
    }

    #[tokio::test]
    async fn backtest_records_run_metrics_trades_and_equity_events() {
        let recorder = InMemoryRunRecorder::new();
        let ctx = RunContext::new(RunType::Backtest, &spec(), 0);
        let closed = trades(3);
        let metrics = BacktestMetrics::calculate(dec!(10_000), &closed, dec!(10), dec!(0.001));

        record_backtest(
            &recorder,
            &ctx,
            &spec(),
            &DataWindow::default(),
            &metrics,
            &closed,
        )
        .await
        .unwrap();

        let rec = recorder.snapshot();
        assert_eq!(rec.runs.len(), 1);
        assert_eq!(rec.runs[0].status, "completed");
        assert_eq!(rec.runs[0].run_type, "backtest");
        assert_eq!(rec.metrics.len(), 1);
        assert_eq!(rec.metrics[0].scope, "full");
        assert_eq!(rec.trades.len(), 3);
        assert_eq!(rec.series.len(), 12);
    }

    #[tokio::test]
    async fn monte_carlo_is_a_child_run_with_extras_and_bounded_fan_chart() {
        let recorder = InMemoryRunRecorder::new();
        let parent = RunContext::new(RunType::Backtest, &spec(), 0);
        let mc_ctx = parent.child(RunType::MonteCarlo, 0);
        let closed = trades(12);
        let config = MonteCarloConfig {
            iterations: 200,
            method: ResampleMethod::IidWithReplacement,
            ruin_threshold_pct: dec!(0.30),
            seed: Some(7),
        };
        let report = MonteCarloSimulator::new(config)
            .run(dec!(10_000), &closed)
            .unwrap();

        record_monte_carlo(&recorder, &mc_ctx, &spec(), &DataWindow::default(), &report)
            .await
            .unwrap();

        let rec = recorder.snapshot();
        assert_eq!(rec.runs[0].run_type, "monte_carlo");
        assert_eq!(
            rec.runs[0].parent_run_id.as_deref(),
            Some(parent.run_id.as_str())
        );
        assert!(rec.trades.is_empty(), "Monte Carlo stores no trades");

        let extras: serde_json::Value =
            serde_json::from_str(rec.metrics[0].extras.as_deref().unwrap()).unwrap();
        assert_eq!(extras["iterations"], 200);
        assert_eq!(extras["resample_method"], "IidWithReplacement");
        assert!(extras.get("probability_of_ruin_pct").is_some());
        assert!(extras.get("p95_max_drawdown_pct").is_some());
        assert!(rec.metrics[0].total_trades.is_none());

        let mut curves: Vec<&str> = rec.series.iter().map(|p| p.event.as_str()).collect();
        curves.dedup();
        assert!(curves.contains(&"Realized"));
        assert!(curves.len() <= 8, "only representative curves are stored");
        assert!(rec
            .series
            .iter()
            .all(|p| p.series == "mc_equity" && p.x.is_some()));
        assert_eq!(rec.series.len(), curves.len() * (closed.len() + 1));
    }
}
