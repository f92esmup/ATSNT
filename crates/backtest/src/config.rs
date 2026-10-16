use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

fn default_max_monthly_drawdown_pct() -> Decimal {
    dec!(0.15)
}

fn default_max_leverage() -> Decimal {
    dec!(20)
}

fn default_min_order_quantity() -> Decimal {
    dec!(0.001)
}

fn default_quantity_step_size() -> Decimal {
    dec!(0.001)
}

fn default_min_order_notional() -> Decimal {
    dec!(5)
}

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
    /// Daily drawdown threshold from the day's marked-equity peak; resets at 00:00:00 UTC.
    pub max_daily_drawdown_pct: Decimal,
    /// Monthly kill-switch threshold from the in-month marked-equity peak (e.g. 0.15 = 15%).
    ///
    /// When breached the open position is closed at market and trading halts for good.
    #[serde(default = "default_max_monthly_drawdown_pct")]
    pub max_monthly_drawdown_pct: Decimal,
    /// Maximum effective leverage; tight stops cannot inflate the position beyond it.
    #[serde(default = "default_max_leverage")]
    pub max_leverage: Decimal,
    /// Minimum order quantity in base units (exchange LOT_SIZE `minQty`).
    #[serde(default = "default_min_order_quantity")]
    pub min_order_quantity: Decimal,
    /// Quantity increment in base units (exchange LOT_SIZE `stepSize`).
    #[serde(default = "default_quantity_step_size")]
    pub quantity_step_size: Decimal,
    /// Minimum order notional in quote currency (exchange MIN_NOTIONAL).
    #[serde(default = "default_min_order_notional")]
    pub min_order_notional: Decimal,
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
            max_monthly_drawdown_pct: default_max_monthly_drawdown_pct(),
            max_leverage: default_max_leverage(),
            min_order_quantity: default_min_order_quantity(),
            quantity_step_size: default_quantity_step_size(),
            min_order_notional: default_min_order_notional(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_artifact_context_without_new_fields_loads_with_defaults() {
        let json = r#"{"initial_capital":"10000","maker_fee_pct":"0.0002","taker_fee_pct":"0.0005","slippage_pct":"0.0005","risk_per_trade_pct":"0.01","max_daily_drawdown_pct":"0.05"}"#;

        let config: BacktestConfig = serde_json::from_str(json).unwrap();

        assert_eq!(config, BacktestConfig::default());
    }
}
