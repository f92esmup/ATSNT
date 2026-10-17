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

fn default_funding_rate_pct() -> Decimal {
    dec!(0.0001)
}

fn default_maintenance_margin_rate() -> Decimal {
    dec!(0.004)
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
    /// Largest notional, in USDT, of a single order. An absolute guardrail, deliberately not a
    /// multiple of the equity: it is the last line against a sizing or balance-reading bug, so it
    /// does not grow on its own. `None` keeps the policy default (10 000); production runs must
    /// set it explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_order_notional: Option<Decimal>,
    /// Largest notional, in USDT, of the whole position. `None` keeps the policy default
    /// (50 000); production runs must set it explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_position_notional: Option<Decimal>,
    /// Perpetual funding rate charged to an open position at every eight-hour settlement
    /// (e.g. 0.0001 = 0.01%, the exchange's calm-market baseline). Positive: longs pay.
    #[serde(default = "default_funding_rate_pct")]
    pub funding_rate_pct: Decimal,
    /// Maintenance margin as a fraction of notional, which sets the liquidation price of an
    /// isolated position (a flat stand-in for the exchange's notional brackets).
    #[serde(default = "default_maintenance_margin_rate")]
    pub maintenance_margin_rate: Decimal,
}

impl BacktestConfig {
    /// Builds the pre-trade [`domain::RiskPolicy`] shared by the simulator and the live gateway.
    ///
    /// Notional caps keep the policy defaults; the drawdown limit, leverage cap and lot rules
    /// come from this configuration so both environments size and gate orders identically.
    pub fn risk_policy(&self) -> Result<domain::RiskPolicy, domain::RiskError> {
        let (order_cap, position_cap) = self.effective_notional_caps();
        domain::RiskPolicy::new(position_cap, order_cap, self.max_daily_drawdown_pct)?
            .with_execution_rules(
                self.max_leverage,
                self.min_order_quantity,
                self.quantity_step_size,
                self.min_order_notional,
            )
    }

    /// `(order, position)` notional caps in force: the configured ones, else the policy defaults.
    pub fn effective_notional_caps(&self) -> (Decimal, Decimal) {
        let defaults = domain::RiskPolicy::default();
        (
            self.max_order_notional
                .unwrap_or(defaults.max_order_notional),
            self.max_position_notional
                .unwrap_or(defaults.max_position_notional),
        )
    }

    /// Whether both notional caps were chosen explicitly instead of left at the defaults.
    pub fn has_explicit_notional_caps(&self) -> bool {
        self.max_order_notional.is_some() && self.max_position_notional.is_some()
    }

    /// One line telling the operator what the caps mean for this equity, so a cap that is too
    /// tight (or too loose) for the account shows at start-up.
    pub fn describe_notional_caps(&self, equity: Decimal) -> String {
        let (order, position) = self.effective_notional_caps();
        let times = |cap: Decimal| {
            if equity > Decimal::ZERO {
                format!("{:.2}x", cap / equity)
            } else {
                "n/a".to_string()
            }
        };
        format!(
            "Notional caps: order {} USDT = {} of equity | position {} USDT = {} of equity | leverage cap {}x",
            order.normalize(),
            times(order),
            position.normalize(),
            times(position),
            self.max_leverage.normalize()
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
        for (name, cap) in [
            ("max_order_notional", self.max_order_notional),
            ("max_position_notional", self.max_position_notional),
        ] {
            if cap.is_some_and(|value| value <= Decimal::ZERO) {
                return Err(crate::error::BacktestError::General(format!(
                    "{name} must be positive, got {}",
                    cap.unwrap_or_default()
                )));
            }
        }
        let (order_cap, position_cap) = self.effective_notional_caps();
        if order_cap > position_cap {
            return Err(crate::error::BacktestError::General(format!(
                "max_order_notional ({order_cap}) cannot exceed max_position_notional ({position_cap})"
            )));
        }
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
        if self.maintenance_margin_rate < Decimal::ZERO
            || self.maintenance_margin_rate >= Decimal::ONE
        {
            return Err(crate::error::BacktestError::General(format!(
                "maintenance_margin_rate must be at least 0 and below 1, got {}",
                self.maintenance_margin_rate
            )));
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
            max_order_notional: None,
            max_position_notional: None,
            funding_rate_pct: default_funding_rate_pct(),
            maintenance_margin_rate: default_maintenance_margin_rate(),
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

    #[test]
    fn notional_caps_default_to_the_policy_defaults_and_old_artifacts_still_load() {
        let defaults = domain::RiskPolicy::default();

        let policy = BacktestConfig::default().risk_policy().unwrap();

        assert_eq!(policy.max_order_notional, defaults.max_order_notional);
        assert_eq!(policy.max_position_notional, defaults.max_position_notional);
        let legacy = r#"{"initial_capital":"10000","maker_fee_pct":"0.0002","taker_fee_pct":"0.0005","slippage_pct":"0.0005","risk_per_trade_pct":"0.01","max_daily_drawdown_pct":"0.05"}"#;
        let config: BacktestConfig = serde_json::from_str(legacy).unwrap();
        assert_eq!(config.max_order_notional, None);
        assert_eq!(config.max_position_notional, None);
    }

    #[test]
    fn explicit_notional_caps_reach_the_risk_policy_and_the_artifact_json() {
        let config = BacktestConfig {
            max_order_notional: Some(dec!(15_000)),
            max_position_notional: Some(dec!(25_000)),
            ..BacktestConfig::default()
        };

        let policy = config.risk_policy().unwrap();
        let json = serde_json::to_string(&config).unwrap();

        assert_eq!(policy.max_order_notional, dec!(15_000));
        assert_eq!(policy.max_position_notional, dec!(25_000));
        assert!(json.contains("max_order_notional") && json.contains("max_position_notional"));
        // Unset caps are not written, so existing artifacts keep their exact form.
        let unset = serde_json::to_string(&BacktestConfig::default()).unwrap();
        assert!(!unset.contains("max_order_notional") && !unset.contains("max_position_notional"));
    }

    #[test]
    fn validate_rejects_non_positive_caps_and_an_order_cap_above_the_position_cap() {
        let with = |order: Option<Decimal>, position: Option<Decimal>| BacktestConfig {
            max_order_notional: order,
            max_position_notional: position,
            ..BacktestConfig::default()
        };

        assert!(with(Some(Decimal::ZERO), None).validate().is_err());
        assert!(with(None, Some(dec!(-1))).validate().is_err());
        assert!(
            with(Some(dec!(60_000)), None).validate().is_err(),
            "above the default position cap"
        );
        assert!(with(Some(dec!(30_000)), Some(dec!(20_000)))
            .validate()
            .is_err());
        assert!(with(Some(dec!(15_000)), Some(dec!(25_000)))
            .validate()
            .is_ok());
    }

    #[test]
    fn the_caps_are_described_in_multiples_of_the_equity() {
        let config = BacktestConfig {
            max_order_notional: Some(dec!(15_000)),
            max_position_notional: Some(dec!(25_000)),
            ..BacktestConfig::default()
        };

        let text = config.describe_notional_caps(dec!(5_000));

        assert!(text.contains("order 15000 USDT = 3.00x"), "{text}");
        assert!(text.contains("position 25000 USDT = 5.00x"), "{text}");
        assert!(text.contains("leverage cap 20x"), "{text}");
    }

    #[test]
    fn notional_caps_are_explicit_only_when_both_are_set() {
        assert!(!BacktestConfig::default().has_explicit_notional_caps());
        assert!(!BacktestConfig {
            max_order_notional: Some(dec!(15_000)),
            ..BacktestConfig::default()
        }
        .has_explicit_notional_caps());
        assert!(BacktestConfig {
            max_order_notional: Some(dec!(15_000)),
            max_position_notional: Some(dec!(25_000)),
            ..BacktestConfig::default()
        }
        .has_explicit_notional_caps());
    }
}
