//! Run recorders: the adapters that implement the run recorder port.
//!
//! The port and its row contract live in `ports::run_recorder`. The in-memory recorder keeps
//! every row for tests and offline runs; the null recorder keeps none and is what long-running
//! binaries use when their analytics are not persisted, so memory does not grow with the run.

use std::sync::Mutex;

use ports::{RecorderError, RunMetricsRow, RunRecorder, RunRow, RunTradeRow, SeriesPointRow};

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

    fn with<F: FnOnce(&mut RecordedRuns)>(&self, f: F) -> Result<(), RecorderError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| RecorderError::Storage("in-memory recorder poisoned".to_string()))?;
        f(&mut guard);
        Ok(())
    }
}

impl RunRecorder for InMemoryRunRecorder {
    async fn record_runs(&self, rows: &[RunRow]) -> Result<(), RecorderError> {
        self.with(|r| r.runs.extend_from_slice(rows))
    }

    async fn record_metrics(&self, rows: &[RunMetricsRow]) -> Result<(), RecorderError> {
        self.with(|r| r.metrics.extend_from_slice(rows))
    }

    async fn record_trades(&self, rows: &[RunTradeRow]) -> Result<(), RecorderError> {
        self.with(|r| r.trades.extend_from_slice(rows))
    }

    async fn record_series(&self, rows: &[SeriesPointRow]) -> Result<(), RecorderError> {
        self.with(|r| r.series.extend_from_slice(rows))
    }
}

/// Recorder that keeps nothing: for runs whose analytics are not persisted.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullRunRecorder;

impl RunRecorder for NullRunRecorder {
    async fn record_runs(&self, _rows: &[RunRow]) -> Result<(), RecorderError> {
        Ok(())
    }

    async fn record_metrics(&self, _rows: &[RunMetricsRow]) -> Result<(), RecorderError> {
        Ok(())
    }

    async fn record_trades(&self, _rows: &[RunTradeRow]) -> Result<(), RecorderError> {
        Ok(())
    }

    async fn record_series(&self, _rows: &[SeriesPointRow]) -> Result<(), RecorderError> {
        Ok(())
    }
}

/// Row carrying only run-type specific `extras`, with no trade metrics. A fixture for the
/// recorders' tests.
#[cfg(test)]
pub(crate) fn extras_only_metrics(
    run_id: impl Into<String>,
    run_type: impl Into<String>,
    scope: impl Into<String>,
    recorded_at: impl Into<String>,
    extras: impl Into<String>,
) -> RunMetricsRow {
    RunMetricsRow {
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
        halted: None,
        rejected_orders: None,
        extras: Some(extras.into()),
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    #[tokio::test]
    async fn in_memory_recorder_keeps_every_row_kind() {
        let recorder = InMemoryRunRecorder::new();
        let metrics = extras_only_metrics("r1", "monte_carlo", "full", "t", "{}");
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

    #[tokio::test]
    async fn null_recorder_accepts_every_row_and_keeps_none() {
        let recorder = NullRunRecorder;
        let metrics = extras_only_metrics("r1", "backtest", "full", "t", "{}");
        recorder
            .record_metrics(std::slice::from_ref(&metrics))
            .await
            .unwrap();
        recorder.record_runs(&[]).await.unwrap();
        recorder.record_trades(&[]).await.unwrap();
        recorder.record_series(&[]).await.unwrap();
    }

    #[test]
    fn numeric_fields_serialize_within_nine_fractional_digits() {
        let mut row = extras_only_metrics("r1", "backtest", "full", "t", "{}");
        row.fee_drag_pct = Some(Decimal::from_str_exact("0.1147980030689286271025803653").unwrap());
        let point = SeriesPointRow {
            run_id: "r1".into(),
            run_type: "backtest".into(),
            series: "equity".into(),
            event: "entry".into(),
            ts: None,
            x: None,
            value: Decimal::from_str_exact("9989.130841157723777327276313").unwrap(),
            trade_id: None,
        };

        let metrics = serde_json::to_value(&row).unwrap();
        let series = serde_json::to_value(&point).unwrap();

        assert_eq!(metrics["fee_drag_pct"], "0.114798003");
        assert_eq!(series["value"], "9989.130841158");
        assert!(metrics["win_rate"].is_null());
    }

    #[test]
    fn extras_only_metrics_leave_trade_fields_null() {
        let row = extras_only_metrics("r1", "monte_carlo", "full", "t", "{\"a\":1}");
        let json = serde_json::to_value(&row).unwrap();
        assert!(json["total_trades"].is_null());
        assert_eq!(json["extras"], "{\"a\":1}");
    }
}
