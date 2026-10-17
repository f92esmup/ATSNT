use backtest::{
    BacktestConfig, BacktestMetrics, ClosedTrade, DataWindow, FillModel, RunContext, RunId,
    RunSpec, RunStatus, RunType, SamplingConfig, TradeExcursion,
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
        funding_paid: dec!(0),
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

#[test]
fn halted_runs_flag_the_metrics_row_and_others_omit_the_column() {
    let normal = BacktestMetrics::calculate(dec!(10_000), &[closed_trade()], dec!(10), dec!(0.001));
    let mut halted = normal.clone();
    halted.halted = true;

    let ctx = RunContext::new(RunType::Backtest, &spec(), 0);
    let normal_row = ctx.metrics_row("full", &normal, None);
    let halted_row = ctx.metrics_row("full", &halted, None);

    assert_eq!(normal_row.halted, None);
    assert_eq!(halted_row.halted, Some(true));
    assert!(!serde_json::to_string(&normal_row)
        .unwrap()
        .contains("halted"));
    assert!(serde_json::to_string(&halted_row)
        .unwrap()
        .contains("\"halted\":true"));
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

    #[tokio::test]
    async fn hpo_records_parent_and_one_trial_run_per_candidate_with_fold_metrics() {
        use backtest::{record_hpo, CandidateEvaluation, FoldMetrics};
        use strategies::DollarBarsCusumSpace;

        let recorder = InMemoryRunRecorder::new();
        let parent_spec = RunSpec {
            strategy_config: DollarBarsCusumSpace::default(),
            ..spec_without_config()
        };
        let parent = RunContext::new(RunType::Hpo, &parent_spec, 0);
        let fold = |idx| FoldMetrics {
            fold_idx: idx,
            in_sample: BacktestMetrics::calculate(dec!(10_000), &trades(4), dec!(5), dec!(0.0005)),
            out_of_sample: BacktestMetrics::calculate(
                dec!(10_000),
                &trades(2),
                dec!(3),
                dec!(0.0003),
            ),
        };
        let candidate = |window| CandidateEvaluation {
            config: DollarBarsCusumConfig {
                rolling_window_len: window,
                ..DollarBarsCusumConfig::default()
            },
            mean_is_sortino: dec!(1.5),
            mean_oos_sortino: dec!(0.8),
            mean_oos_profit_factor: Some(dec!(1.3)),
            total_oos_trades: 4,
            min_fold_trades: 2,
            eligible: true,
            oos_sharpe: Some(dec!(0.12)),
            stability_score: dec!(0.9),
            fitness: dec!(1.35),
            dsr_p_value: dec!(0.04),
            is_statistically_significant: true,
            fold_metrics: vec![fold(0), fold(1)],
        };
        let evaluations = vec![candidate(25), candidate(30)];

        let winner = record_hpo(
            &recorder,
            &parent,
            &parent_spec,
            &DataWindow::default(),
            &evaluations,
            serde_json::json!({"folds": 2}),
        )
        .await
        .unwrap();

        let rec = recorder.snapshot();
        assert_eq!(rec.runs.len(), 3);
        assert_eq!(rec.runs[0].run_type, "hpo");
        let trials: Vec<_> = rec
            .runs
            .iter()
            .filter(|r| r.run_type == "hpo_trial")
            .collect();
        assert_eq!(trials.len(), 2);
        assert!(trials
            .iter()
            .all(|t| t.parent_run_id.as_deref() == Some(parent.run_id.as_str())));
        let params: serde_json::Value = serde_json::from_str(&trials[1].params).unwrap();
        assert_eq!(params["rolling_window_len"], 30);
        assert_eq!(
            winner.as_ref().map(|w| w.as_str()),
            Some(trials[0].run_id.as_str())
        );

        let trial_scopes: Vec<&str> = rec
            .metrics
            .iter()
            .filter(|m| m.run_id == trials[0].run_id)
            .map(|m| m.scope.as_str())
            .collect();
        assert_eq!(
            trial_scopes,
            ["full", "is_fold_0", "oos_fold_0", "is_fold_1", "oos_fold_1"]
        );
        let oos = rec
            .metrics
            .iter()
            .find(|m| m.run_id == trials[0].run_id && m.scope == "oos_fold_1")
            .unwrap();
        assert_eq!(oos.total_trades, Some(2));
        assert_eq!(oos.max_drawdown_pct, Some(dec!(0.0003)));
        let summary: serde_json::Value = serde_json::from_str(
            rec.metrics
                .iter()
                .find(|m| m.run_id == trials[0].run_id && m.scope == "full")
                .and_then(|m| m.extras.as_deref())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(summary["rank"], 1);
        assert_eq!(summary["mean_oos_sortino"], "0.8");
        let parent_extras: serde_json::Value = serde_json::from_str(
            rec.metrics
                .iter()
                .find(|m| m.run_id == parent.run_id.as_str())
                .and_then(|m| m.extras.as_deref())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(parent_extras["folds"], 2);
        assert_eq!(parent_extras["trials_evaluated"], 2);
        assert_eq!(parent_extras["winner_run_id"], winner.unwrap().as_str());
        assert!(rec.trades.is_empty() && rec.series.is_empty());
    }

    fn spec_without_config() -> RunSpec<DollarBarsCusumSpace> {
        let base = spec();
        RunSpec {
            strategy_id: base.strategy_id,
            symbol: base.symbol,
            market_type: base.market_type,
            sampling: base.sampling,
            strategy_config: DollarBarsCusumSpace::default(),
            execution_context: base.execution_context,
        }
    }
    use strategies::DollarBarsCusumSpace;
}

fn fill_model_of(context: &RunContext) -> String {
    context
        .run_row(&spec(), RunStatus::Running, None, &DataWindow::default())
        .unwrap()
        .fill_model
}

#[test]
fn every_run_type_records_how_its_orders_were_filled() {
    let at = 1_700_000_000_000;

    // The optimizer and plain backtests fill at bar level, paper trades the tick-level venue and
    // live fills come from the exchange.
    assert_eq!(
        fill_model_of(&RunContext::new(RunType::Backtest, &spec(), at)),
        "bar"
    );
    assert_eq!(
        fill_model_of(&RunContext::new(RunType::Hpo, &spec(), at)),
        "bar"
    );
    assert_eq!(
        fill_model_of(&RunContext::new(RunType::Paper, &spec(), at)),
        "tick"
    );
    assert_eq!(
        fill_model_of(&RunContext::new(RunType::Live, &spec(), at)),
        "exchange"
    );
}

#[test]
fn a_tick_backtest_and_its_children_are_tagged_as_tick() {
    let at = 1_700_000_000_000;
    let tick = RunContext::new(RunType::Backtest, &spec(), at).with_fill_model(FillModel::Tick);

    let monte_carlo = tick.child(RunType::MonteCarlo, at + 1);

    assert_eq!(fill_model_of(&tick), "tick");
    assert_eq!(fill_model_of(&monte_carlo), "tick");
}

#[test]
fn trade_rows_carry_the_funding_paid_while_the_trade_was_open() {
    let context = RunContext::new(RunType::Backtest, &spec(), 1_700_000_000_000);
    let mut trade = closed_trade();
    trade.funding_paid = dec!(0.3);

    let rows = context.trade_rows(&[trade], 1);

    assert_eq!(rows[0].funding_paid, dec!(0.3));
}

/// Columns of `table` in `scripts/gcp/bigquery_schema.sql` as `(name, is_not_null)`.
fn schema_columns(table: &str) -> Vec<(String, bool)> {
    let sql = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/gcp/bigquery_schema.sql"),
    )
    .expect("the BigQuery schema file");
    let header = format!("CREATE TABLE IF NOT EXISTS `atsnt_bi.{table}` (");
    let body = sql
        .split(&header)
        .nth(1)
        .unwrap_or_else(|| panic!("table {table} is not in the schema"))
        .split("\n)")
        .next()
        .unwrap();
    body.lines()
        .map(|line| line.split("--").next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .map(|line| {
            let name = line.split_whitespace().next().unwrap().to_string();
            (name, line.contains("NOT NULL"))
        })
        .collect()
}

/// Every field a row sends must be a column, and every required column must be sent: BigQuery
/// rejects the whole batch otherwise, which would lose a run's records silently in a cloud job.
fn assert_row_matches_schema(table: &str, row: &serde_json::Value) {
    let columns = schema_columns(table);
    let sent: Vec<&String> = row.as_object().expect("a JSON object").keys().collect();
    for key in &sent {
        assert!(
            columns.iter().any(|(name, _)| name == *key),
            "{table}: the row sends `{key}` but the schema has no such column"
        );
    }
    for (name, required) in &columns {
        if *required {
            assert!(
                sent.contains(&name),
                "{table}: the schema requires `{name}` but the row does not send it"
            );
        }
    }
}

#[test]
fn run_rows_match_the_bigquery_runs_table() {
    let ctx = RunContext::new(RunType::Backtest, &spec(), 1_700_000_000_000);
    let row = ctx
        .run_row(&spec(), RunStatus::Running, None, &DataWindow::default())
        .unwrap();

    assert_row_matches_schema("runs", &serde_json::to_value(row).unwrap());
}

#[test]
fn metrics_rows_match_the_bigquery_run_metrics_table() {
    let ctx = RunContext::new(RunType::Backtest, &spec(), 0);
    let mut metrics =
        BacktestMetrics::calculate(dec!(10_000), &[closed_trade()], dec!(10), dec!(0.001));
    metrics.halted = true;

    assert_row_matches_schema(
        "run_metrics",
        &serde_json::to_value(ctx.metrics_row("full", &metrics, None)).unwrap(),
    );
}

#[test]
fn trade_rows_match_the_bigquery_trades_table() {
    let ctx = RunContext::new(RunType::Paper, &spec(), 0);
    let rows = ctx.trade_rows(&[closed_trade()], 1);

    assert_row_matches_schema("trades", &serde_json::to_value(&rows[0]).unwrap());
}

#[test]
fn series_rows_match_the_bigquery_run_series_table() {
    let ctx = RunContext::new(RunType::Paper, &spec(), 0);
    let points = ctx.equity_points(&[closed_trade()], 1);

    assert_row_matches_schema("run_series", &serde_json::to_value(&points[0]).unwrap());
}

#[test]
fn paper_run_records_the_baseline_it_was_started_from() {
    let backtest = RunContext::new(RunType::Backtest, &spec(), 1_700_000_000_000);
    let paper = RunContext::new(RunType::Paper, &spec(), 1_700_001_000_000)
        .with_baseline(Some(backtest.run_id.clone()));
    let window = DataWindow {
        source: None,
        start_ms: None,
        end_ms: None,
    };

    let row = paper
        .run_row(&spec(), RunStatus::Running, None, &window)
        .unwrap();

    assert_eq!(
        row.baseline_run_id.as_deref(),
        Some(backtest.run_id.as_str())
    );
    // The baseline is not a lineage link.
    assert_eq!(row.parent_run_id, None);
    let json = serde_json::to_value(&row).unwrap();
    assert_eq!(json["baseline_run_id"], backtest.run_id.as_str());
}

#[test]
fn runs_without_a_baseline_omit_the_column() {
    let window = DataWindow {
        source: None,
        start_ms: None,
        end_ms: None,
    };
    let backtest = RunContext::new(RunType::Backtest, &spec(), 1_700_000_000_000);
    let child = backtest.child(RunType::MonteCarlo, 1_700_000_100_000);

    for ctx in [&backtest, &child] {
        let row = ctx
            .run_row(&spec(), RunStatus::Running, None, &window)
            .unwrap();
        assert_eq!(row.baseline_run_id, None);
        let json = serde_json::to_value(&row).unwrap();
        assert!(json.get("baseline_run_id").is_none());
    }
}

#[test]
fn run_id_parse_accepts_generated_ids_and_rejects_typos() {
    let id = RunId::generate();
    assert_eq!(RunId::parse(id.as_str()).unwrap(), id);
    for bad in [
        "",
        "not-a-run-id",
        "0198B3C0-7A4E-7C21-8F3D-5B2A9C6E1D40",
        "0198b3c0-7a4e-4c21-8f3d-5b2a9c6e1d40",
        "0198b3c0-7a4e-7c21-0f3d-5b2a9c6e1d40",
        "0198b3c0-7a4e-7c21-8f3d-5b2a9c6e1d4",
        "0198b3c0-7a4e-7c21-8f3d-5b2a9c6e1d40 ",
    ] {
        assert!(RunId::parse(bad).is_err(), "{bad:?} must be rejected");
    }
}
