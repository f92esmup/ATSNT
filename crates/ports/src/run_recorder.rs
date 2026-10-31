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

use std::future::Future;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Failure of a run recorder to persist rows.
#[derive(Debug, Error)]
pub enum RecorderError {
    /// The storage behind the recorder refused or lost the rows.
    #[error("run recorder failed: {0}")]
    Storage(String),
    /// A row could not be serialized before it was sent to the storage.
    #[error("{0}")]
    Serialization(String),
}

/// Serializers rounding decimals to a 9-digit fractional scale. The storage behind the
/// recorder rejects rows carrying more digits.
mod decimal_scale9 {
    use rust_decimal::Decimal;
    use serde::Serializer;

    const SCALE: u32 = 9;

    pub fn value<S: Serializer>(v: &Decimal, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.round_dp(SCALE).normalize().to_string())
    }

    pub fn option<S: Serializer>(v: &Option<Decimal>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(d) => value(d, s),
            None => s.serialize_none(),
        }
    }
}

/// Lifecycle record of a run (`runs` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRow {
    pub run_id: String,
    pub parent_run_id: Option<String>,
    /// Backtest a paper or live run was started from; omitted (NULL) otherwise so rows of other
    /// runs do not depend on the column existing in the dataset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_run_id: Option<String>,
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
    /// `bar` | `tick` | `exchange`: how the run's orders were filled.
    pub fill_model: String,
    pub data_source: Option<String>,
    pub data_start: Option<String>,
    pub data_end: Option<String>,
    pub git_sha: Option<String>,
}

/// Performance record of a run for one evaluation scope (`run_metrics` table).
///
/// Trade-derived fields are optional because some run types (Monte Carlo,
/// HPO parents) only carry `extras`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMetricsRow {
    pub run_id: String,
    pub run_type: String,
    /// `full` | `is` | `oos` | `fold_<n>`.
    pub scope: String,
    pub recorded_at: String,
    pub total_trades: Option<i64>,
    pub winning_trades: Option<i64>,
    pub losing_trades: Option<i64>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub win_rate: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub gross_profit: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub gross_loss: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub net_profit: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub total_fees: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub total_slippage: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub profit_factor: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub payoff_ratio: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub mathematical_expectancy: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub max_drawdown_amount: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub max_drawdown_pct: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub sortino_ratio: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub fee_drag_pct: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub avg_holding_seconds: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub avg_mae_pct: Option<Decimal>,
    #[serde(serialize_with = "decimal_scale9::option")]
    pub avg_mfe_pct: Option<Decimal>,
    /// JSON object: exit reason -> trade count.
    pub exit_reason_counts: Option<String>,
    /// `true` when the monthly kill-switch halted the run; omitted (NULL) otherwise so rows
    /// of runs that never halted do not depend on the column existing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halted: Option<bool>,
    /// JSON object: rejection reason -> signals the risk policy did not turn into orders.
    /// Omitted (NULL) when nothing was rejected, so such rows do not depend on the column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_orders: Option<String>,
    /// JSON object: run-type specific values (DSR, stability, ruin probability, ...).
    pub extras: Option<String>,
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
    #[serde(serialize_with = "decimal_scale9::value")]
    pub entry_price: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub exit_price: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub quantity: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub gross_pnl: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub fees_paid: Decimal,
    /// Perpetual funding paid while the trade was open (negative when received); already part
    /// of `net_pnl`.
    #[serde(serialize_with = "decimal_scale9::value")]
    pub funding_paid: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub slippage_paid: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub net_pnl: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub return_pct: Decimal,
    pub exit_reason: String,
    pub holding_duration_seconds: i64,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub entry_equity: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub trough_equity: Decimal,
    pub trough_timestamp: String,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub peak_equity: Decimal,
    pub peak_timestamp: String,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub exit_equity: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
    pub max_drawdown_amount: Decimal,
    #[serde(serialize_with = "decimal_scale9::value")]
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
    #[serde(serialize_with = "decimal_scale9::value")]
    pub value: Decimal,
    pub trade_id: Option<String>,
}

/// Outbound port persisting run analytics.
///
/// Every method returns a `Send` future, so a recorder can be used from a task spawned on the
/// runtime. The trait has no object form: a composition that picks one recorder among several
/// wraps them in an enum.
pub trait RunRecorder: Send + Sync {
    /// Appends lifecycle rows (one per run and status change).
    fn record_runs(
        &self,
        rows: &[RunRow],
    ) -> impl Future<Output = Result<(), RecorderError>> + Send;

    /// Appends performance rows.
    fn record_metrics(
        &self,
        rows: &[RunMetricsRow],
    ) -> impl Future<Output = Result<(), RecorderError>> + Send;

    /// Appends closed trade rows.
    fn record_trades(
        &self,
        rows: &[RunTradeRow],
    ) -> impl Future<Output = Result<(), RecorderError>> + Send;

    /// Appends series points.
    fn record_series(
        &self,
        rows: &[SeriesPointRow],
    ) -> impl Future<Output = Result<(), RecorderError>> + Send;
}
