use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Friction parameters and account risk settings for 1:1 realistic simulation.
///
/// Implements the Zero-Toy-Assumption Principle specified in AGENTS.md,
/// enforcing fee schedules, spreads, and latency slippage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// Starting capital in quote currency (USDT).
    pub initial_capital: Decimal,
    /// Exchange fee for limit maker orders (e.g. 0.0002 = 0.02%).
    pub maker_fee_pct: Decimal,
    /// Exchange fee for market taker orders (e.g. 0.0005 = 0.05%).
    pub taker_fee_pct: Decimal,
    /// Simulated latency and spread penalty on entry/exit (e.g. 0.0005 = 0.05%).
    pub slippage_pct: Decimal,
    /// Fixed fractional capital risk allocated per trade (e.g. 0.01 = 1.0%).
    pub risk_per_trade_pct: Decimal,
    /// Circuit breaker threshold: halt trading if intraday drawdown reaches this fraction (e.g. 0.05 = 5.0%).
    pub max_daily_drawdown_pct: Decimal,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            initial_capital: dec!(10_000),
            maker_fee_pct: dec!(0.0002),
            taker_fee_pct: dec!(0.0005),
            slippage_pct: dec!(0.0005),
            risk_per_trade_pct: dec!(0.01),
            max_daily_drawdown_pct: dec!(0.05),
        }
    }
}
