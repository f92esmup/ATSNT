//! Run-centric analytics persistence port and its row contract.
//!
//! Every execution mode (backtest, HPO, Monte Carlo, paper, live) records the
//! same four kinds of rows, all keyed by `run_id`:
//! - [`RunRow`]: what ran (spec, lineage, lifecycle status). Append-only; the
//!   latest row per `run_id` is the current state.
//! - [`RunMetricsRow`]: one performance record per run and scope
//!   (`full`, `is`, `oos`, ...), with type-specific values in `extras`.
//! - [`RunTradeRow`]: closed trades with their equity excursion.
//! - [`SeriesPointRow`]: event-based series points (equity at trade events,
//!   Monte Carlo fan chart percentiles). Never time-sampled.
//!
//! Strategy hyperparameters and other open-ended structures travel as JSON
//! strings so new strategies need no schema change.

use std::sync::Mutex;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::AdapterError;

/// Lifecycle record of a run (`runs` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRow {
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub run_type: String,
    /// `running` | `completed` | `failed` | `stopped`.
    pub status: String,
    pub recorded_at: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub strategy_id: String,
    pub symbol: String,
    pub market_type: String,
    /// JSON: sampling configuration.
    pub sampling: String,
    /// JSON: strategy hyperparameters.
    pub params: String,
    /// JSON: fees, slippage, capital, and risk settings actually used.
    pub execution_context: String,
    pub data_source: Option<String>,
    pub data_start: Option<String>,
    pub data_end: Option<String>,
    pub git_sha: Option<String>,
}

/// Performance record of a run for one evaluation scope (`run_metrics` table).
///
/// Trade-derived fields are optional because some run types (Monte Carlo,
/// HPO parents) only carry `extras`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMetricsRow {
    pub run_id: String,
    pub run_type: String,
    /// `full` | `is` | `oos` | `fold_<n>`.
    pub scope: String,
    pub recorded_at: String,
    pub total_trades: Option<i64>,
    pub winning_trades: Option<i64>,
    pub losing_trades: Option<i64>,
    pub win_rate: Option<Decimal>,
    pub gross_profit: Option<Decimal>,
    pub gross_loss: Option<Decimal>,
    pub net_profit: Option<Decimal>,
    pub total_fees: Option<Decimal>,
    pub total_slippage: Option<Decimal>,
    pub profit_factor: Option<Decimal>,
    pub payoff_ratio: Option<Decimal>,
    pub mathematical_expectancy: Option<Decimal>,
    pub max_drawdown_amount: Option<Decimal>,
    pub max_drawdown_pct: Option<Decimal>,
    pub sortino_ratio: Option<Decimal>,
    pub fee_drag_pct: Option<Decimal>,
    pub avg_holding_seconds: Option<Decimal>,
    pub avg_mae_pct: Option<Decimal>,
    pub avg_mfe_pct: Option<Decimal>,
    /// JSON object: exit reason -> trade count.
    pub exit_reason_counts: Option<String>,
    /// JSON object: run-type specific values (DSR, stability, ruin probability, ...).
    pub extras: Option<String>,
}

impl RunMetricsRow {
    /// Row carrying only run-type specific `extras`, with no trade metrics.
    pub fn extras_only(
        run_id: impl Into<String>,
        run_type: impl Into<String>,
        scope: impl Into<String>,
        recorded_at: impl Into<String>,
        extras: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            run_type: run_type.into(),
            scope: scope.into(),
            recorded_at: recorded_at.into(),
            total_trades: None,
            winning_trades: None,
            losing_trades: None,
            win_rate: None,
            gross_profit: None,
            gross_loss: None,
            net_profit: None,
            total_fees: None,
            total_slippage: None,
            profit_factor: None,
            payoff_ratio: None,
            mathematical_expectancy: None,
            max_drawdown_amount: None,
            max_drawdown_pct: None,
            sortino_ratio: None,
            fee_drag_pct: None,
            avg_holding_seconds: None,
            avg_mae_pct: None,
            avg_mfe_pct: None,
            exit_reason_counts: None,
            extras: Some(extras.into()),
        }
    }
}

/// Closed trade with its equity excursion (`trades` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTradeRow {
    pub trade_id: String,
    pub run_id: String,
    pub run_type: String,
    pub strategy_id: String,
    pub symbol: String,
    /// `Long` | `Short`.
    pub side: String,
    pub entry_timestamp: String,
    pub exit_timestamp: String,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub quantity: Decimal,
    pub gross_pnl: Decimal,
    pub fees_paid: Decimal,
    pub slippage_paid: Decimal,
    pub net_pnl: Decimal,
    pub return_pct: Decimal,
    pub exit_reason: String,
    pub holding_duration_seconds: i64,
    pub entry_equity: Decimal,
    pub trough_equity: Decimal,
    pub trough_timestamp: String,
    pub peak_equity: Decimal,
    pub peak_timestamp: String,
    pub exit_equity: Decimal,
    pub max_drawdown_amount: Decimal,
    pub max_drawdown_pct: Decimal,
}

/// One event-based series point (`run_series` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesPointRow {
    pub run_id: String,
    pub run_type: String,
    /// Series name, e.g. `equity` or `mc_max_drawdown`.
    pub series: String,
    /// Point label, e.g. `entry`, `trough`, `peak`, `exit`, `p95`.
    pub event: String,
    /// Event time for time-indexed series.
    pub ts: Option<String>,
    /// Ordinal index for non-temporal series (e.g. trade number in a fan chart).
    pub x: Option<i64>,
    pub value: Decimal,
    pub trade_id: Option<String>,
}

/// Outbound port persisting run analytics.
pub trait RunRecorder: Send + Sync {
    /// Appends lifecycle rows (one per run and status change).
    #[allow(async_fn_in_trait)]
    async fn record_runs(&self, rows: &[RunRow]) -> Result<(), AdapterError>;

    /// Appends performance rows.
    #[allow(async_fn_in_trait)]
    async fn record_metrics(&self, rows: &[RunMetricsRow]) -> Result<(), AdapterError>;

    /// Appends closed trade rows.
    #[allow(async_fn_in_trait)]
    async fn record_trades(&self, rows: &[RunTradeRow]) -> Result<(), AdapterError>;

    /// Appends series points.
    #[allow(async_fn_in_trait)]
    async fn record_series(&self, rows: &[SeriesPointRow]) -> Result<(), AdapterError>;
}

/// Everything an [`InMemoryRunRecorder`] has received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordedRuns {
    pub runs: Vec<RunRow>,
    pub metrics: Vec<RunMetricsRow>,
    pub trades: Vec<RunTradeRow>,
    pub series: Vec<SeriesPointRow>,
}

/// Recorder keeping rows in memory, for tests and offline runs.
#[derive(Debug, Default)]
pub struct InMemoryRunRecorder {
    inner: Mutex<RecordedRuns>,
}

impl InMemoryRunRecorder {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a copy of every row recorded so far.
    pub fn snapshot(&self) -> RecordedRuns {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn with<F: FnOnce(&mut RecordedRuns)>(&self, f: F) -> Result<(), AdapterError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| AdapterError::General("in-memory recorder poisoned".to_string()))?;
        f(&mut guard);
        Ok(())
    }
}

impl RunRecorder for InMemoryRunRecorder {
    async fn record_runs(&self, rows: &[RunRow]) -> Result<(), AdapterError> {
        self.with(|r| r.runs.extend_from_slice(rows))
    }

    async fn record_metrics(&self, rows: &[RunMetricsRow]) -> Result<(), AdapterError> {
        self.with(|r| r.metrics.extend_from_slice(rows))
    }

    async fn record_trades(&self, rows: &[RunTradeRow]) -> Result<(), AdapterError> {
        self.with(|r| r.trades.extend_from_slice(rows))
    }

    async fn record_series(&self, rows: &[SeriesPointRow]) -> Result<(), AdapterError> {
        self.with(|r| r.series.extend_from_slice(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_recorder_keeps_every_row_kind() {
        let recorder = InMemoryRunRecorder::new();
        let metrics = RunMetricsRow::extras_only("r1", "monte_carlo", "full", "t", "{}");
        recorder
            .record_metrics(std::slice::from_ref(&metrics))
            .await
            .unwrap();
        let point = SeriesPointRow {
            run_id: "r1".into(),
            run_type: "monte_carlo".into(),
            series: "mc_max_drawdown".into(),
            event: "p95".into(),
            ts: None,
            x: Some(3),
            value: Decimal::ONE,
            trade_id: None,
        };
        recorder
            .record_series(std::slice::from_ref(&point))
            .await
            .unwrap();

        let recorded = recorder.snapshot();
        assert_eq!(recorded.metrics, vec![metrics]);
        assert_eq!(recorded.series, vec![point]);
        assert!(recorded.runs.is_empty() && recorded.trades.is_empty());
    }

    #[test]
    fn extras_only_metrics_leave_trade_fields_null() {
        let row = RunMetricsRow::extras_only("r1", "monte_carlo", "full", "t", "{\"a\":1}");
        let json = serde_json::to_value(&row).unwrap();
        assert!(json["total_trades"].is_null());
        assert_eq!(json["extras"], "{\"a\":1}");
    }
}
