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
/// Targets the Zero-Toy-Assumption Principle specified in AGENTS.md. Today it models fee
/// schedules and a fixed per-fill slippage; bid/ask spread, latency and funding are not yet
/// modeled (see `odd/tasks/sim-live-fidelity.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// Starting capital in quote currency (USDT).
    pub initial_capital: Decimal,
    /// Exchange fee for limit maker orders (e.g. 0.0002 = 0.02%). The bar-level engine only
    /// sends market orders, so it does not charge it yet.
    pub maker_fee_pct: Decimal,
    /// Exchange fee for market taker orders (e.g. 0.0005 = 0.05%).
    pub taker_fee_pct: Decimal,
    /// Fixed slippage penalty applied to every market fill, entries and barrier exits (e.g.
    /// 0.0005 = 0.05%); a stand-in for spread and latency until those are modeled explicitly.
    pub slippage_pct: Decimal,
    /// Fixed fractional capital risk allocated per trade (e.g. 0.01 = 1.0%).
    pub risk_per_trade_pct: Decimal,
    /// Daily drawdown threshold from the day's marked-equity peak; resets at 00:00:00 UTC.
    pub max_daily_drawdown_pct: Decimal,
    /// Monthly kill-switch threshold: loss versus the equity at the start of the UTC month
    /// (e.g. 0.15 = 15%).
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

impl BacktestConfig {
    /// Builds the pre-trade [`domain::RiskPolicy`] shared by the simulator and the live gateway.
    ///
    /// Notional caps keep the policy defaults; the drawdown limit, leverage cap and lot rules
    /// come from this configuration so both environments size and gate orders identically.
    pub fn risk_policy(&self) -> Result<domain::RiskPolicy, domain::RiskError> {
        let defaults = domain::RiskPolicy::default();
        domain::RiskPolicy::new(
            defaults.max_position_notional,
            defaults.max_order_notional,
            self.max_daily_drawdown_pct,
        )?
        .with_execution_rules(
            self.max_leverage,
            self.min_order_quantity,
            self.quantity_step_size,
            self.min_order_notional,
        )
    }

    /// Rejects thresholds and execution rules that would trip or distort every run.
    pub fn validate(&self) -> Result<(), crate::error::BacktestError> {
        let fraction = |name: &str, value: Decimal| {
            if value <= Decimal::ZERO || value > Decimal::ONE {
                Err(crate::error::BacktestError::General(format!(
                    "{name} must be greater than 0 and at most 1, got {value}"
                )))
            } else {
                Ok(())
            }
        };
        fraction("max_daily_drawdown_pct", self.max_daily_drawdown_pct)?;
        fraction("max_monthly_drawdown_pct", self.max_monthly_drawdown_pct)?;
        for (name, value) in [
            ("max_leverage", self.max_leverage),
            ("min_order_quantity", self.min_order_quantity),
            ("quantity_step_size", self.quantity_step_size),
            ("min_order_notional", self.min_order_notional),
        ] {
            if value <= Decimal::ZERO {
                return Err(crate::error::BacktestError::General(format!(
                    "{name} must be positive, got {value}"
                )));
            }
        }
        Ok(())
    }
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

    #[test]
    fn risk_policy_carries_the_configured_limits() {
        let config = BacktestConfig {
            max_daily_drawdown_pct: dec!(0.20),
            max_leverage: dec!(5),
            min_order_quantity: dec!(0.01),
            quantity_step_size: dec!(0.01),
            min_order_notional: dec!(10),
            ..BacktestConfig::default()
        };

        let policy = config.risk_policy().unwrap();

        assert_eq!(policy.max_daily_drawdown_pct, dec!(0.20));
        assert_eq!(policy.max_leverage, dec!(5));
        assert_eq!(policy.min_order_quantity, dec!(0.01));
        assert_eq!(policy.quantity_step_size, dec!(0.01));
        assert_eq!(policy.min_order_notional, dec!(10));
    }

    #[test]
    fn risk_policy_keeps_the_default_notional_caps() {
        let policy = BacktestConfig::default().risk_policy().unwrap();
        let defaults = domain::RiskPolicy::default();

        assert_eq!(policy.max_position_notional, defaults.max_position_notional);
        assert_eq!(policy.max_order_notional, defaults.max_order_notional);
    }

    #[test]
    fn validate_accepts_defaults_and_rejects_degenerate_thresholds() {
        assert!(BacktestConfig::default().validate().is_ok());

        for bad in [Decimal::ZERO, dec!(-0.1), dec!(1.5)] {
            let config = BacktestConfig {
                max_monthly_drawdown_pct: bad,
                ..BacktestConfig::default()
            };
            assert!(config.validate().is_err(), "monthly {bad} must be rejected");
        }
        let config = BacktestConfig {
            max_leverage: Decimal::ZERO,
            ..BacktestConfig::default()
        };
        assert!(config.validate().is_err());
    }
}
