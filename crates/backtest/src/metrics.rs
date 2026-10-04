use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use serde::{Deserialize, Serialize};

/// Record of an individually closed trade for performance attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosedTrade {
    pub exit_time: i64,
    pub pnl_gross: Decimal,
    pub fees_paid: Decimal,
    pub slippage_paid: Decimal,
    pub pnl_net: Decimal,
    pub return_pct: Decimal,
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
    pub profit_factor: Decimal,
    pub payoff_ratio: Decimal,
    pub mathematical_expectancy: Decimal,
    pub max_drawdown_amount: Decimal,
    pub max_drawdown_pct: Decimal,
    pub sortino_ratio: Decimal,
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
        let mut negative_returns_sq_sum = Decimal::ZERO;
        let mut downside_count = 0;

        for t in trades {
            net_profit += t.pnl_net;
            total_fees += t.fees_paid;
            total_slippage += t.slippage_paid;

            if t.pnl_net > Decimal::ZERO {
                winning_trades += 1;
                gross_profit += t.pnl_net;
            } else if t.pnl_net < Decimal::ZERO {
                losing_trades += 1;
                gross_loss += t.pnl_net.abs();
                negative_returns_sq_sum += t.return_pct * t.return_pct;
                downside_count += 1;
            }
        }

        let n = Decimal::from(total_trades);
        let win_rate = Decimal::from(winning_trades) / n;

        let profit_factor = if gross_loss == Decimal::ZERO {
            if gross_profit > Decimal::ZERO {
                Decimal::from(100) // Arbitrary ceiling for zero losses
            } else {
                Decimal::ZERO
            }
        } else {
            gross_profit / gross_loss
        };

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

        // Sortino Ratio = Mean Return / Downside Deviation
        let mean_return = if initial_capital > Decimal::ZERO {
            net_profit / initial_capital
        } else {
            Decimal::ZERO
        };

        let sortino_ratio = if downside_count > 0 {
            let downside_variance = negative_returns_sq_sum / Decimal::from(downside_count);
            let downside_dev = downside_variance.sqrt().unwrap_or(Decimal::ZERO);
            if downside_dev > Decimal::ZERO {
                mean_return / downside_dev
            } else {
                Decimal::ZERO
            }
        } else if mean_return > Decimal::ZERO {
            Decimal::from(100) // Positive returns with zero downside
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
            sortino_ratio,
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
            profit_factor: Decimal::ZERO,
            payoff_ratio: Decimal::ZERO,
            mathematical_expectancy: Decimal::ZERO,
            max_drawdown_amount: Decimal::ZERO,
            max_drawdown_pct: Decimal::ZERO,
            sortino_ratio: Decimal::ZERO,
        }
    }
}
