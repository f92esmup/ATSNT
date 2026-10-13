//! Translation of domain results into the run analytics row contract
//! ([`adapters::analytics`]).

use std::time::{SystemTime, UNIX_EPOCH};

use adapters::{
    format_unix_ms_rfc3339, AdapterError, RunMetricsRow, RunRecorder, RunRow, RunTradeRow,
    SeriesPointRow,
};
use domain::PositionSide;
use serde::Serialize;

use crate::metrics::{BacktestMetrics, ClosedTrade};
use crate::monte_carlo::{MonteCarloReport, ResampleMethod};
use crate::optimization::CandidateEvaluation;
use crate::run::{RunId, RunSpec, RunType};

/// Git commit the binary was built from, when available at build time.
pub const GIT_SHA: Option<&str> = option_env!("ATSNT_GIT_SHA");

/// Lifecycle state persisted on each appended run row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Run started and is still executing.
    Running,
    /// Run finished normally.
    Completed,
    /// Run aborted on an error.
    Failed,
    /// Run was stopped by the operator (paper/live shutdown).
    Stopped,
}

impl RunStatus {
    /// Stable label used in persisted rows.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

/// Market data a run consumed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataWindow {
    /// Data file, directory, or stream identifier.
    pub source: Option<String>,
    /// First event time consumed (Unix ms).
    pub start_ms: Option<i64>,
    /// Last event time consumed (Unix ms).
    pub end_ms: Option<i64>,
}

/// Identity shared by every row a run emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunContext {
    pub run_id: RunId,
    pub parent_run_id: Option<RunId>,
    pub run_type: RunType,
    pub strategy_id: String,
    pub symbol: String,
    pub started_at_ms: i64,
}

/// Current wall-clock time in Unix milliseconds.
pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl RunContext {
    /// Starts a new top-level run for `spec`.
    pub fn new<C>(run_type: RunType, spec: &RunSpec<C>, started_at_ms: i64) -> Self {
        Self {
            run_id: RunId::generate(),
            parent_run_id: None,
            run_type,
            strategy_id: spec.strategy_id.clone(),
            symbol: spec.symbol.clone(),
            started_at_ms,
        }
    }

    /// Starts a run nested under this one (HPO trial, Monte Carlo of a backtest).
    pub fn child(&self, run_type: RunType, started_at_ms: i64) -> Self {
        Self {
            run_id: RunId::generate(),
            parent_run_id: Some(self.run_id.clone()),
            run_type,
            strategy_id: self.strategy_id.clone(),
            symbol: self.symbol.clone(),
            started_at_ms,
        }
    }

    /// Lifecycle row capturing the complete spec the run executed.
    pub fn run_row<C: Serialize>(
        &self,
        spec: &RunSpec<C>,
        status: RunStatus,
        finished_at_ms: Option<i64>,
        data: &DataWindow,
    ) -> Result<RunRow, serde_json::Error> {
        let market_type = match serde_json::to_value(spec.market_type)? {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        Ok(RunRow {
            run_id: self.run_id.to_string(),
            parent_run_id: self.parent_run_id.as_ref().map(ToString::to_string),
            run_type: self.run_type.as_str().to_string(),
            status: status.as_str().to_string(),
            recorded_at: format_unix_ms_rfc3339(now_unix_ms()),
            started_at: format_unix_ms_rfc3339(self.started_at_ms),
            finished_at: finished_at_ms.map(format_unix_ms_rfc3339),
            strategy_id: spec.strategy_id.clone(),
            symbol: spec.symbol.clone(),
            market_type,
            sampling: serde_json::to_string(&spec.sampling)?,
            params: serde_json::to_string(&spec.strategy_config)?,
            execution_context: serde_json::to_string(&spec.execution_context)?,
            data_source: data.source.clone(),
            data_start: data.start_ms.map(format_unix_ms_rfc3339),
            data_end: data.end_ms.map(format_unix_ms_rfc3339),
            git_sha: GIT_SHA.map(str::to_string),
        })
    }

    /// Performance row for `scope` built from computed metrics.
    pub fn metrics_row(
        &self,
        scope: &str,
        m: &BacktestMetrics,
        extras: Option<serde_json::Value>,
    ) -> RunMetricsRow {
        RunMetricsRow {
            run_id: self.run_id.to_string(),
            run_type: self.run_type.as_str().to_string(),
            scope: scope.to_string(),
            recorded_at: format_unix_ms_rfc3339(now_unix_ms()),
            total_trades: Some(m.total_trades as i64),
            winning_trades: Some(m.winning_trades as i64),
            losing_trades: Some(m.losing_trades as i64),
            win_rate: Some(m.win_rate),
            gross_profit: Some(m.gross_profit),
            gross_loss: Some(m.gross_loss),
            net_profit: Some(m.net_profit),
            total_fees: Some(m.total_fees),
            total_slippage: Some(m.total_slippage),
            profit_factor: Some(m.profit_factor),
            payoff_ratio: Some(m.payoff_ratio),
            mathematical_expectancy: Some(m.mathematical_expectancy),
            max_drawdown_amount: Some(m.max_drawdown_amount),
            max_drawdown_pct: Some(m.max_drawdown_pct),
            sortino_ratio: Some(m.sortino_ratio),
            fee_drag_pct: Some(m.fee_drag_pct),
            avg_holding_seconds: Some(m.avg_holding_seconds),
            avg_mae_pct: Some(m.avg_mae_pct),
            avg_mfe_pct: Some(m.avg_mfe_pct),
            exit_reason_counts: serde_json::to_string(&m.exit_reason_counts).ok(),
            extras: extras.map(|v| v.to_string()),
        }
    }

    /// Performance row carrying only run-type specific values.
    pub fn extras_row(&self, scope: &str, extras: serde_json::Value) -> RunMetricsRow {
        RunMetricsRow::extras_only(
            self.run_id.to_string(),
            self.run_type.as_str(),
            scope,
            format_unix_ms_rfc3339(now_unix_ms()),
            extras.to_string(),
        )
    }

    /// Stable trade identifier for the 1-based trade number within this run.
    pub fn trade_id(&self, number: usize) -> String {
        format!("{}_t{:05}", self.run_id, number)
    }

    /// Trade rows numbered from `first_number` (1-based).
    pub fn trade_rows(&self, trades: &[ClosedTrade], first_number: usize) -> Vec<RunTradeRow> {
        trades
            .iter()
            .enumerate()
            .map(|(i, t)| RunTradeRow {
                trade_id: self.trade_id(first_number + i),
                run_id: self.run_id.to_string(),
                run_type: self.run_type.as_str().to_string(),
                strategy_id: self.strategy_id.clone(),
                symbol: self.symbol.clone(),
                side: match t.side {
                    PositionSide::Long => "Long",
                    PositionSide::Short => "Short",
                }
                .to_string(),
                entry_timestamp: format_unix_ms_rfc3339(t.entry_time),
                exit_timestamp: format_unix_ms_rfc3339(t.exit_time),
                entry_price: t.entry_price,
                exit_price: t.exit_price,
                quantity: t.quantity,
                gross_pnl: t.pnl_gross,
                fees_paid: t.fees_paid,
                slippage_paid: t.slippage_paid,
                net_pnl: t.pnl_net,
                return_pct: t.return_pct,
                exit_reason: t.exit_reason.clone(),
                holding_duration_seconds: t.holding_duration_seconds,
                entry_equity: t.excursion.entry_equity,
                trough_equity: t.excursion.trough_equity,
                trough_timestamp: format_unix_ms_rfc3339(t.excursion.trough_time),
                peak_equity: t.excursion.peak_equity,
                peak_timestamp: format_unix_ms_rfc3339(t.excursion.peak_time),
                exit_equity: t.excursion.exit_equity,
                max_drawdown_amount: t.excursion.max_drawdown_amount,
                max_drawdown_pct: t.excursion.max_drawdown_pct,
            })
            .collect()
    }

    /// Event-based equity curve: entry, trough, peak, and exit per trade,
    /// in chronological order within each trade.
    pub fn equity_points(
        &self,
        trades: &[ClosedTrade],
        first_number: usize,
    ) -> Vec<SeriesPointRow> {
        let mut points = Vec::with_capacity(trades.len() * 4);
        for (i, t) in trades.iter().enumerate() {
            let trade_id = self.trade_id(first_number + i);
            let ex = &t.excursion;
            let mut events = [
                ("entry", t.entry_time, ex.entry_equity, 0),
                ("trough", ex.trough_time, ex.trough_equity, 1),
                ("peak", ex.peak_time, ex.peak_equity, 1),
                ("exit", t.exit_time, ex.exit_equity, 2),
            ];
            events.sort_by_key(|(_, time, _, rank)| (*rank, *time));
            for (event, time, value, _) in events {
                points.push(SeriesPointRow {
                    run_id: self.run_id.to_string(),
                    run_type: self.run_type.as_str().to_string(),
                    series: "equity".to_string(),
                    event: event.to_string(),
                    ts: Some(format_unix_ms_rfc3339(time)),
                    x: None,
                    value,
                    trade_id: Some(trade_id.clone()),
                });
            }
        }
        points
    }
}

/// Fan-chart curves persisted per Monte Carlo run: the realized path, the
/// extreme paths, and quantile paths (ranked by final equity, best first).
const MC_PERSISTED_CURVES: [&str; 8] = [
    "Realized",
    "Worst_Stress_1",
    "Best_Ceiling_1",
    "Quantile_0.05",
    "Quantile_0.25",
    "Quantile_0.50",
    "Quantile_0.75",
    "Quantile_0.95",
];

fn serialization_error(e: serde_json::Error) -> AdapterError {
    AdapterError::General(format!("run row serialization failed: {e}"))
}

/// Persists a completed backtest: run, `full` metrics, trades, and equity events.
pub async fn record_backtest<R: RunRecorder, C: Serialize>(
    recorder: &R,
    ctx: &RunContext,
    spec: &RunSpec<C>,
    data: &DataWindow,
    metrics: &BacktestMetrics,
    trades: &[ClosedTrade],
) -> Result<(), AdapterError> {
    let run = ctx
        .run_row(spec, RunStatus::Completed, Some(now_unix_ms()), data)
        .map_err(serialization_error)?;
    recorder.record_runs(&[run]).await?;
    recorder
        .record_metrics(&[ctx.metrics_row("full", metrics, None)])
        .await?;
    if !trades.is_empty() {
        recorder.record_trades(&ctx.trade_rows(trades, 1)).await?;
        recorder
            .record_series(&ctx.equity_points(trades, 1))
            .await?;
    }
    Ok(())
}

/// Persists a Monte Carlo stress test as its own run: percentiles and
/// simulation settings in metrics `extras`, representative curves in
/// `run_series` (series `mc_equity`, `x` = trade index). No trades.
pub async fn record_monte_carlo<R: RunRecorder, C: Serialize>(
    recorder: &R,
    ctx: &RunContext,
    spec: &RunSpec<C>,
    data: &DataWindow,
    report: &MonteCarloReport,
) -> Result<(), AdapterError> {
    let run = ctx
        .run_row(spec, RunStatus::Completed, Some(now_unix_ms()), data)
        .map_err(serialization_error)?;
    recorder.record_runs(&[run]).await?;

    let (method, block_size) = match report.config.method {
        ResampleMethod::IidWithReplacement => ("IidWithReplacement", None),
        ResampleMethod::CircularBlockBootstrap { block_size } => {
            ("CircularBlockBootstrap", Some(block_size))
        }
    };
    let mut extras = serde_json::to_value(&report.metrics).map_err(serialization_error)?;
    if let Some(obj) = extras.as_object_mut() {
        obj.insert("iterations".into(), report.config.iterations.into());
        obj.insert("resample_method".into(), method.into());
        obj.insert("block_size".into(), block_size.into());
        obj.insert(
            "ruin_threshold_pct".into(),
            serde_json::to_value(report.config.ruin_threshold_pct).map_err(serialization_error)?,
        );
        obj.insert("seed".into(), report.config.seed.into());
    }
    recorder
        .record_metrics(&[ctx.extras_row("full", extras)])
        .await?;

    let points: Vec<SeriesPointRow> = report
        .fan_chart
        .iter()
        .filter(|curve| MC_PERSISTED_CURVES.contains(&curve.label.as_str()))
        .flat_map(|curve| {
            curve
                .equity_curve
                .iter()
                .enumerate()
                .map(move |(i, value)| SeriesPointRow {
                    run_id: ctx.run_id.to_string(),
                    run_type: ctx.run_type.as_str().to_string(),
                    series: "mc_equity".to_string(),
                    event: curve.label.clone(),
                    ts: None,
                    x: Some(i as i64),
                    value: *value,
                    trade_id: None,
                })
        })
        .collect();
    if !points.is_empty() {
        recorder.record_series(&points).await?;
    }
    Ok(())
}

/// Persists a Walk-Forward HPO sweep: one `hpo` parent run (its `params` are
/// the search space) and one `hpo_trial` child run per evaluated candidate.
///
/// Each trial gets a `full` extras row with its ranking scores and full
/// metrics for every fold (`is_fold_<n>`, `oos_fold_<n>`). No trades or
/// series are stored. `settings` (folds, ratios, timings) is merged into the
/// parent's extras. Returns the winning (rank 1) trial's run id.
pub async fn record_hpo<R: RunRecorder, S: Serialize, C: Serialize + Clone>(
    recorder: &R,
    parent: &RunContext,
    parent_spec: &RunSpec<S>,
    data: &DataWindow,
    evaluations: &[CandidateEvaluation<C>],
    settings: serde_json::Value,
) -> Result<Option<RunId>, AdapterError> {
    let finished = now_unix_ms();
    let mut runs = vec![parent
        .run_row(parent_spec, RunStatus::Completed, Some(finished), data)
        .map_err(serialization_error)?];
    let mut metrics = Vec::new();
    let mut winner = None;

    for (rank, eval) in evaluations.iter().enumerate() {
        let trial = parent.child(RunType::HpoTrial, parent.started_at_ms);
        let trial_spec = parent_spec.with_config(eval.config.clone());
        runs.push(
            trial
                .run_row(&trial_spec, RunStatus::Completed, Some(finished), data)
                .map_err(serialization_error)?,
        );
        let summary = serde_json::json!({
            "rank": rank + 1,
            "mean_is_sortino": eval.mean_is_sortino,
            "mean_oos_sortino": eval.mean_oos_sortino,
            "mean_oos_profit_factor": eval.mean_oos_profit_factor,
            "total_oos_trades": eval.total_oos_trades,
            "stability_score": eval.stability_score,
            "fitness": eval.fitness,
            "dsr_p_value": eval.dsr_p_value,
            "is_statistically_significant": eval.is_statistically_significant,
        });
        metrics.push(trial.extras_row("full", summary));
        for fold in &eval.fold_metrics {
            metrics.push(trial.metrics_row(
                &format!("is_fold_{}", fold.fold_idx),
                &fold.in_sample,
                None,
            ));
            metrics.push(trial.metrics_row(
                &format!("oos_fold_{}", fold.fold_idx),
                &fold.out_of_sample,
                None,
            ));
        }
        if rank == 0 {
            winner = Some(trial.run_id.clone());
        }
    }

    let mut parent_extras = settings;
    if let Some(obj) = parent_extras.as_object_mut() {
        obj.insert("trials_evaluated".into(), evaluations.len().into());
        obj.insert(
            "winner_run_id".into(),
            winner.as_ref().map(ToString::to_string).into(),
        );
    }
    metrics.insert(0, parent.extras_row("full", parent_extras));

    recorder.record_runs(&runs).await?;
    recorder.record_metrics(&metrics).await?;
    Ok(winner)
}

/// Appends a lifecycle row for a long-running session (paper/live).
/// `finished_at` is set for every status other than `Running`.
pub async fn record_run_status<R: RunRecorder, C: Serialize>(
    recorder: &R,
    ctx: &RunContext,
    spec: &RunSpec<C>,
    status: RunStatus,
    data: &DataWindow,
) -> Result<(), AdapterError> {
    let finished_at = (status != RunStatus::Running).then(now_unix_ms);
    let row = ctx
        .run_row(spec, status, finished_at, data)
        .map_err(serialization_error)?;
    recorder.record_runs(&[row]).await
}

/// Records one closed trade (1-based `number` within the run) and its four
/// equity events.
pub async fn record_closed_trade<R: RunRecorder>(
    recorder: &R,
    ctx: &RunContext,
    trade: &ClosedTrade,
    number: usize,
) -> Result<(), AdapterError> {
    let trades = std::slice::from_ref(trade);
    recorder
        .record_trades(&ctx.trade_rows(trades, number))
        .await?;
    recorder
        .record_series(&ctx.equity_points(trades, number))
        .await
}
