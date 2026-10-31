use std::collections::BTreeMap;

pub use crate::accounting::TradeExcursion;
use crate::PositionSide;
use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use serde::{Deserialize, Serialize};

/// Record of an individually closed trade for performance attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosedTrade {
    pub entry_time: i64,
    pub exit_time: i64,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub quantity: Decimal,
    pub side: PositionSide,
    pub pnl_gross: Decimal,
    pub fees_paid: Decimal,
    /// Perpetual funding paid while the position was open (negative when received); already
    /// deducted from `pnl_net`.
    #[serde(default)]
    pub funding_paid: Decimal,
    pub slippage_paid: Decimal,
    pub pnl_net: Decimal,
    pub return_pct: Decimal,
    pub exit_reason: String,
    pub holding_duration_seconds: i64,
    /// Account equity path observed while the trade was open.
    #[serde(default)]
    pub excursion: TradeExcursion,
}

/// Comprehensive risk and return metrics adhering to Section 6 of strategy specifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacktestMetrics {
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub win_rate: Decimal,
    pub gross_profit: Decimal,
    pub gross_loss: Decimal,
    pub net_profit: Decimal,
    pub total_fees: Decimal,
    pub total_slippage: Decimal,
    /// Gross profit over gross loss; `None` without losing trades (undefined, not infinite).
    pub profit_factor: Option<Decimal>,
    pub payoff_ratio: Decimal,
    pub mathematical_expectancy: Decimal,
    pub max_drawdown_amount: Decimal,
    pub max_drawdown_pct: Decimal,
    /// Per-trade Sortino ratio: the mean trade return on entry equity over the downside
    /// deviation `sqrt(sum(min(r, 0)^2) / N)` of all N trades (see [`sortino_ratio`]).
    /// `None` without trades or without downside, where the ratio is undefined.
    pub sortino_ratio: Option<Decimal>,
    /// Share of gross profit (sum of positive pre-fee PnL) consumed by fees.
    #[serde(default)]
    pub fee_drag_pct: Decimal,
    /// Number of closed trades per exit reason (e.g. "TakeProfit").
    #[serde(default)]
    pub exit_reason_counts: BTreeMap<String, usize>,
    /// Mean holding time per trade in seconds.
    #[serde(default)]
    pub avg_holding_seconds: Decimal,
    /// Mean maximum adverse excursion, as a fraction of entry equity.
    #[serde(default)]
    pub avg_mae_pct: Decimal,
    /// Mean maximum favorable excursion, as a fraction of entry equity.
    #[serde(default)]
    pub avg_mfe_pct: Decimal,
    /// True when the monthly kill-switch stopped the run before the data ended.
    #[serde(default)]
    pub halted: bool,
    /// Signals the engine did not turn into orders, per reason: a [`domain::RiskError::kind`]
    /// such as "OrderSizeExceeded", or [`DAILY_HALT_REJECTION`] while the daily circuit
    /// breaker is tripped. Target decisions add [`REARM_IGNORED_REJECTION`] and two
    /// adjustment counters, [`POSITION_CAP_CLIP`] and [`STOP_BEYOND_LIQUIDATION_CLIP`], which
    /// count orders that were reduced and still traded, not rejections.
    #[serde(default)]
    pub rejected_orders: BTreeMap<String, usize>,
}

impl BacktestMetrics {
    /// Computes statistical performance metrics from closed trades and equity history.
    pub fn calculate(
        initial_capital: Decimal,
        trades: &[ClosedTrade],
        max_drawdown_amount: Decimal,
        max_drawdown_pct: Decimal,
    ) -> Self {
        if trades.is_empty() {
            return Self::empty();
        }

        let total_trades = trades.len();
        let mut winning_trades = 0;
        let mut losing_trades = 0;
        let mut gross_profit = Decimal::ZERO;
        let mut gross_loss = Decimal::ZERO;
        let mut net_profit = Decimal::ZERO;
        let mut total_fees = Decimal::ZERO;
        let mut total_slippage = Decimal::ZERO;
        let mut positive_gross_pnl = Decimal::ZERO;
        let mut exit_reason_counts = BTreeMap::new();
        let mut holding_seconds_sum = Decimal::ZERO;
        let mut mae_pct_sum = Decimal::ZERO;
        let mut mfe_pct_sum = Decimal::ZERO;

        for t in trades {
            if t.pnl_gross > Decimal::ZERO {
                positive_gross_pnl += t.pnl_gross;
            }
            *exit_reason_counts.entry(t.exit_reason.clone()).or_insert(0) += 1;
            holding_seconds_sum += Decimal::from(t.holding_duration_seconds);
            let entry_equity = t.excursion.entry_equity;
            if entry_equity > Decimal::ZERO {
                mae_pct_sum +=
                    (entry_equity - t.excursion.trough_equity).max(Decimal::ZERO) / entry_equity;
                mfe_pct_sum +=
                    (t.excursion.peak_equity - entry_equity).max(Decimal::ZERO) / entry_equity;
            }
            net_profit += t.pnl_net;
            total_fees += t.fees_paid;
            total_slippage += t.slippage_paid;

            if t.pnl_net > Decimal::ZERO {
                winning_trades += 1;
                gross_profit += t.pnl_net;
            } else if t.pnl_net < Decimal::ZERO {
                losing_trades += 1;
                gross_loss += t.pnl_net.abs();
            }
        }

        let n = Decimal::from(total_trades);
        let win_rate = Decimal::from(winning_trades) / n;

        let profit_factor = (gross_loss > Decimal::ZERO).then(|| gross_profit / gross_loss);

        let avg_win = if winning_trades > 0 {
            gross_profit / Decimal::from(winning_trades)
        } else {
            Decimal::ZERO
        };

        let avg_loss = if losing_trades > 0 {
            gross_loss / Decimal::from(losing_trades)
        } else {
            Decimal::ZERO
        };

        let payoff_ratio = if avg_loss > Decimal::ZERO {
            avg_win / avg_loss
        } else {
            Decimal::ZERO
        };

        // Mathematical Expectancy = (WinRate * AvgWin) - (LossRate * AvgLoss)
        let loss_rate = Decimal::from(losing_trades) / n;
        let mathematical_expectancy = (win_rate * avg_win) - (loss_rate * avg_loss);

        let sortino = sortino_ratio(&trade_returns(initial_capital, trades));

        let fee_drag_pct = if positive_gross_pnl > Decimal::ZERO {
            total_fees / positive_gross_pnl
        } else {
            Decimal::ZERO
        };

        Self {
            total_trades,
            winning_trades,
            losing_trades,
            win_rate,
            gross_profit,
            gross_loss,
            net_profit,
            total_fees,
            total_slippage,
            profit_factor,
            payoff_ratio,
            mathematical_expectancy,
            max_drawdown_amount,
            max_drawdown_pct,
            sortino_ratio: sortino,
            fee_drag_pct,
            exit_reason_counts,
            avg_holding_seconds: holding_seconds_sum / n,
            avg_mae_pct: mae_pct_sum / n,
            avg_mfe_pct: mfe_pct_sum / n,
            halted: false,
            rejected_orders: BTreeMap::new(),
        }
    }

    fn empty() -> Self {
        Self {
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate: Decimal::ZERO,
            gross_profit: Decimal::ZERO,
            gross_loss: Decimal::ZERO,
            net_profit: Decimal::ZERO,
            total_fees: Decimal::ZERO,
            total_slippage: Decimal::ZERO,
            profit_factor: None,
            payoff_ratio: Decimal::ZERO,
            mathematical_expectancy: Decimal::ZERO,
            max_drawdown_amount: Decimal::ZERO,
            max_drawdown_pct: Decimal::ZERO,
            sortino_ratio: None,
            fee_drag_pct: Decimal::ZERO,
            exit_reason_counts: BTreeMap::new(),
            avg_holding_seconds: Decimal::ZERO,
            avg_mae_pct: Decimal::ZERO,
            avg_mfe_pct: Decimal::ZERO,
            halted: false,
            rejected_orders: BTreeMap::new(),
        }
    }
}

/// Rejection reason counted while the daily circuit breaker is tripped. It sits in the same
/// namespace as [`domain::RiskError::kind`], so it must not match any of its names.
pub const DAILY_HALT_REJECTION: &str = "DailyHalt";

/// Rejection reason counted when opening a short position is attempted in Spot mode.
/// Same namespace as [`DAILY_HALT_REJECTION`].
pub const SPOT_SHORTING_DISALLOWED: &str = "SpotShortingDisallowed";

/// Rejection reason counted when a runner receives a decision kind it cannot execute yet
/// (`Decision::Target` before the target-position execution exists). Same namespace as
/// [`DAILY_HALT_REJECTION`].
pub const UNSUPPORTED_DECISION_REJECTION: &str = "UnsupportedDecision";

/// Target ignored by the re-arm rule (strategy spec §11.7): after a stop, a liquidation or a
/// kill-switch flatten on one side, a target on that side is ignored until a flat or opposite
/// target arrives. Same namespace as [`DAILY_HALT_REJECTION`].
pub const REARM_IGNORED_REJECTION: &str = "RearmIgnored";

/// Target order reduced because its liquidation price lay between the entry and the stop
/// (strategy spec §11.6). An **adjustment counter**, not a rejection: the reduced order still
/// trades (unless nothing is left). Same namespace as [`DAILY_HALT_REJECTION`].
pub const STOP_BEYOND_LIQUIDATION_CLIP: &str = "StopBeyondLiquidation";

/// Target clipped at `max_position_notional` when it was sized (strategy spec §11.3). An
/// **adjustment counter**, not a rejection: the clipped target still trades. Same namespace as
/// [`DAILY_HALT_REJECTION`].
pub const POSITION_CAP_CLIP: &str = "PositionCapClip";

/// Two-decimal rendering of a ratio for reports, `n/a` where it is undefined.
pub fn format_ratio(ratio: Option<Decimal>) -> String {
    ratio.map_or_else(|| "n/a".to_string(), |r| format!("{r:.2}"))
}

/// Report rendering of rejected signals, e.g. `OrderSizeExceeded=3, DailyHalt=1`, or `none`.
pub fn format_rejections(rejected: &BTreeMap<String, usize>) -> String {
    if rejected.is_empty() {
        return "none".to_string();
    }
    rejected
        .iter()
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Net return of each trade on the account equity it was opened with.
///
/// Sizing risks a fraction of equity, so this is the return the account experiences. A trade
/// without a recorded equity path is measured on `initial_capital`. A trade with no positive
/// base at all (both are zero or negative, which a valid run never produces) is left out, so
/// the series can be shorter than the trade list.
pub fn trade_returns(initial_capital: Decimal, trades: &[ClosedTrade]) -> Vec<Decimal> {
    trades
        .iter()
        .filter_map(|t| {
            let base = if t.excursion.entry_equity > Decimal::ZERO {
                t.excursion.entry_equity
            } else {
                initial_capital
            };
            (base > Decimal::ZERO).then(|| t.pnl_net / base)
        })
        .collect()
}

/// Sortino ratio of a return series: `mean(r) / sqrt(sum(min(r, 0)^2) / N)`.
///
/// The downside deviation divides by all N returns (target return 0), so a series with few,
/// small losses is not inflated. `None` for an empty series or one without downside: the
/// ratio is undefined there, and a fixed cap would read as an excellent measurement.
pub fn sortino_ratio(returns: &[Decimal]) -> Option<Decimal> {
    if returns.is_empty() {
        return None;
    }
    let n = Decimal::from(returns.len());
    let mean = returns.iter().copied().sum::<Decimal>() / n;
    let downside_sq_sum = returns
        .iter()
        .filter(|r| **r < Decimal::ZERO)
        .map(|r| r * r)
        .sum::<Decimal>();
    let downside_dev = (downside_sq_sum / n).sqrt()?;
    (downside_dev > Decimal::ZERO).then(|| mean / downside_dev)
}
