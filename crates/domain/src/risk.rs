//! Pure pre-trade risk policy rules and circuit breakers.
//!
//! Enforces institutional safety bounds on order sizes, maximum position exposure,
//! and accounts drawdown circuit breakers with zero floating-point math.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::order::OrderIntent;

/// Errors emitted when pre-trade risk constraints are violated.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RiskError {
    /// Account daily drawdown breached the hard circuit breaker limit.
    #[error("Circuit breaker triggered: daily drawdown {current_pct} exceeds limit {limit_pct}")]
    CircuitBreakerTriggered {
        current_pct: String,
        limit_pct: String,
    },
    /// Single order notional value exceeds authorized threshold.
    #[error("Order notional {order_notional} exceeds maximum single order limit {limit}")]
    OrderSizeExceeded {
        order_notional: String,
        limit: String,
    },
    /// Resulting position exposure exceeds aggregate portfolio limits.
    #[error("Resulting position notional {new_notional} exceeds maximum position limit {limit}")]
    PositionLimitExceeded { new_notional: String, limit: String },
    /// Risk policy was constructed with invalid parameters.
    #[error("Invalid risk policy parameters: {0}")]
    InvalidParameters(String),
}

/// Pre-trade risk policy configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskPolicy {
    /// Maximum allowed position notional value in quote currency (e.g. 50,000 USDT).
    pub max_position_notional: Decimal,
    /// Maximum order notional size allowed in a single order (e.g. 10,000 USDT).
    pub max_order_notional: Decimal,
    /// Maximum daily drawdown percentage before circuit breaker halts all trading (e.g. 0.05 = 5%).
    pub max_daily_drawdown_pct: Decimal,
}

impl Default for RiskPolicy {
    fn default() -> Self {
        Self {
            max_position_notional: Decimal::new(50_000, 0),
            max_order_notional: Decimal::new(10_000, 0),
            max_daily_drawdown_pct: Decimal::new(5, 2), // 0.05
        }
    }
}

impl RiskPolicy {
    /// Creates and validates a new [`RiskPolicy`].
    pub fn new(
        max_position_notional: Decimal,
        max_order_notional: Decimal,
        max_daily_drawdown_pct: Decimal,
    ) -> Result<Self, RiskError> {
        if max_position_notional <= Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "max_position_notional must be positive".to_string(),
            ));
        }
        if max_order_notional <= Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "max_order_notional must be positive".to_string(),
            ));
        }
        if max_daily_drawdown_pct <= Decimal::ZERO || max_daily_drawdown_pct > Decimal::ONE {
            return Err(RiskError::InvalidParameters(
                "max_daily_drawdown_pct must be between 0.0 and 1.0".to_string(),
            ));
        }

        Ok(Self {
            max_position_notional,
            max_order_notional,
            max_daily_drawdown_pct,
        })
    }

    /// Evaluates if an intended order satisfies all pre-trade risk criteria.
    ///
    /// # Arguments
    /// * `intent` - Desired order intent from active strategy.
    /// * `order_quantity` - Order quantity in base asset units.
    /// * `current_position_notional` - Current gross position exposure in quote currency.
    /// * `current_daily_drawdown_pct` - Current daily peak-to-trough drawdown ratio.
    pub fn evaluate_order(
        &self,
        intent: &OrderIntent,
        order_quantity: Decimal,
        current_position_notional: Decimal,
        current_daily_drawdown_pct: Decimal,
    ) -> Result<(), RiskError> {
        // 1. Circuit breaker check
        if current_daily_drawdown_pct >= self.max_daily_drawdown_pct {
            return Err(RiskError::CircuitBreakerTriggered {
                current_pct: current_daily_drawdown_pct.to_string(),
                limit_pct: self.max_daily_drawdown_pct.to_string(),
            });
        }

        // 2. Single order notional check
        let order_notional = intent.price * order_quantity;
        if order_notional > self.max_order_notional {
            return Err(RiskError::OrderSizeExceeded {
                order_notional: order_notional.to_string(),
                limit: self.max_order_notional.to_string(),
            });
        }

        // 3. Resulting position notional check
        let resulting_notional = current_position_notional + order_notional;
        if resulting_notional > self.max_position_notional {
            return Err(RiskError::PositionLimitExceeded {
                new_notional: resulting_notional.to_string(),
                limit: self.max_position_notional.to_string(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::Side;
    use rust_decimal_macros::dec;

    fn sample_intent(price: Decimal) -> OrderIntent {
        OrderIntent::new(
            1704067200000,
            Side::Buy,
            price,
            price * dec!(0.98),
            price * dec!(1.02),
            15,
        )
        .unwrap()
    }

    #[test]
    fn test_valid_order_passes_risk_checks() {
        let policy = RiskPolicy::default();
        let intent = sample_intent(dec!(65000.00));
        let qty = dec!(0.1); // notional = 6500 <= 10000
        let res = policy.evaluate_order(&intent, qty, dec!(0.0), dec!(0.01));
        assert!(res.is_ok());
    }

    #[test]
    fn test_circuit_breaker_blocks_trading() {
        let policy = RiskPolicy::default(); // max dd = 0.05
        let intent = sample_intent(dec!(65000.00));
        let qty = dec!(0.05);
        let res = policy.evaluate_order(&intent, qty, dec!(0.0), dec!(0.055));
        assert!(matches!(
            res,
            Err(RiskError::CircuitBreakerTriggered { .. })
        ));
    }

    #[test]
    fn test_order_size_exceeded() {
        let policy = RiskPolicy::default(); // max order = 10,000
        let intent = sample_intent(dec!(65000.00));
        let qty = dec!(0.2); // notional = 13,000 > 10,000
        let res = policy.evaluate_order(&intent, qty, dec!(0.0), dec!(0.01));
        assert!(matches!(res, Err(RiskError::OrderSizeExceeded { .. })));
    }

    #[test]
    fn test_position_limit_exceeded() {
        let policy = RiskPolicy::default(); // max pos = 50,000, max order = 10,000
        let intent = sample_intent(dec!(65000.00));
        let qty = dec!(0.1); // notional = 6,500
        let current_pos = dec!(46000.00); // 46,000 + 6,500 = 52,500 > 50,000
        let res = policy.evaluate_order(&intent, qty, current_pos, dec!(0.01));
        assert!(matches!(res, Err(RiskError::PositionLimitExceeded { .. })));
    }
}
