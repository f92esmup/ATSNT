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
    /// Session drawdown breached the hard circuit breaker limit.
    #[error("Circuit breaker triggered: session drawdown {current_pct} exceeds limit {limit_pct}")]
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
    /// Maximum session drawdown from marked-equity peak; no daily reset (e.g. 0.05 = 5%).
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

    /// Calculates peak-to-trough drawdown from marked session equity.
    ///
    /// The caller owns the session high-water mark and must not reset it daily.
    pub fn calculate_session_drawdown_pct(
        session_peak_equity: Decimal,
        mark_to_market_equity: Decimal,
    ) -> Result<Decimal, RiskError> {
        if session_peak_equity <= Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "session_peak_equity must be positive".to_string(),
            ));
        }

        Ok(
            ((session_peak_equity - mark_to_market_equity) / session_peak_equity)
                .max(Decimal::ZERO),
        )
    }

    /// Sizes an order from stop distance and rejects it when policy limits fail.
    ///
    /// The quantity is never clamped to fit a notional limit: a breach rejects the order.
    /// `execution_price` is the simulated fill price in backtests and submitted price in live.
    pub fn size_order(
        &self,
        intent: &OrderIntent,
        execution_price: Decimal,
        mark_to_market_equity: Decimal,
        risk_per_trade_pct: Decimal,
        current_position_notional: Decimal,
        current_session_drawdown_pct: Decimal,
    ) -> Result<Decimal, RiskError> {
        if execution_price <= Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "execution_price must be positive".to_string(),
            ));
        }
        if mark_to_market_equity <= Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "mark_to_market_equity must be positive".to_string(),
            ));
        }
        if risk_per_trade_pct <= Decimal::ZERO || risk_per_trade_pct > Decimal::ONE {
            return Err(RiskError::InvalidParameters(
                "risk_per_trade_pct must be greater than 0 and at most 1".to_string(),
            ));
        }
        if current_position_notional < Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "current_position_notional cannot be negative".to_string(),
            ));
        }
        if current_session_drawdown_pct < Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "current_session_drawdown_pct cannot be negative".to_string(),
            ));
        }

        let stop_distance = match intent.side {
            crate::trade::Side::Buy => execution_price - intent.stop_loss,
            crate::trade::Side::Sell => intent.stop_loss - execution_price,
        };
        if stop_distance <= Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "stop loss must be on the risk side of the entry price".to_string(),
            ));
        }

        let quantity = (mark_to_market_equity * risk_per_trade_pct) / stop_distance;
        if quantity <= Decimal::ZERO {
            return Err(RiskError::InvalidParameters(
                "calculated order quantity must be positive".to_string(),
            ));
        }

        self.evaluate_order_at_price(
            execution_price,
            quantity,
            current_position_notional,
            current_session_drawdown_pct,
        )?;

        Ok(quantity)
    }

    /// Evaluates if an intended order satisfies all pre-trade risk criteria.
    ///
    /// # Arguments
    /// * `intent` - Desired order intent from active strategy.
    /// * `order_quantity` - Order quantity in base asset units.
    /// * `current_position_notional` - Current gross position exposure in quote currency.
    /// * `current_session_drawdown_pct` - Current marked session peak-to-trough ratio.
    pub fn evaluate_order(
        &self,
        intent: &OrderIntent,
        order_quantity: Decimal,
        current_position_notional: Decimal,
        current_session_drawdown_pct: Decimal,
    ) -> Result<(), RiskError> {
        self.evaluate_order_at_price(
            intent.price,
            order_quantity,
            current_position_notional,
            current_session_drawdown_pct,
        )
    }

    fn evaluate_order_at_price(
        &self,
        order_price: Decimal,
        order_quantity: Decimal,
        current_position_notional: Decimal,
        current_session_drawdown_pct: Decimal,
    ) -> Result<(), RiskError> {
        // 1. Circuit breaker check
        if current_session_drawdown_pct >= self.max_daily_drawdown_pct {
            return Err(RiskError::CircuitBreakerTriggered {
                current_pct: current_session_drawdown_pct.to_string(),
                limit_pct: self.max_daily_drawdown_pct.to_string(),
            });
        }

        // 2. Single order notional check
        let order_notional = order_price * order_quantity;
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
    fn sizing_uses_one_percent_of_marked_equity_over_stop_distance() {
        let quantity = RiskPolicy::default()
            .size_order(
                &sample_intent(dec!(100)),
                dec!(100),
                dec!(10_000),
                dec!(0.01),
                Decimal::ZERO,
                Decimal::ZERO,
            )
            .unwrap();

        assert_eq!(quantity, dec!(50));
    }

    #[test]
    fn oversized_sized_order_is_rejected_instead_of_clamped() {
        let intent = OrderIntent::new(
            1704067200000,
            Side::Buy,
            dec!(100),
            dec!(99.5),
            dec!(101),
            15,
        )
        .unwrap();

        let result = RiskPolicy::default().size_order(
            &intent,
            dec!(100),
            dec!(10_000),
            dec!(0.01),
            Decimal::ZERO,
            Decimal::ZERO,
        );

        assert!(matches!(result, Err(RiskError::OrderSizeExceeded { .. })));
    }

    #[test]
    fn sized_order_that_exceeds_position_cap_is_rejected() {
        let result = RiskPolicy::default().size_order(
            &sample_intent(dec!(100)),
            dec!(100),
            dec!(10_000),
            dec!(0.01),
            dec!(49_100),
            Decimal::ZERO,
        );

        assert!(matches!(
            result,
            Err(RiskError::PositionLimitExceeded { .. })
        ));
    }

    #[test]
    fn sized_order_accepts_exact_order_and_position_limits() {
        let policy = RiskPolicy::default();
        let intent = sample_intent(dec!(100));

        let order_at_limit = policy
            .size_order(
                &intent,
                dec!(100),
                dec!(20_000),
                dec!(0.01),
                Decimal::ZERO,
                Decimal::ZERO,
            )
            .unwrap();
        let position_at_limit = policy
            .size_order(
                &intent,
                dec!(100),
                dec!(20_000),
                dec!(0.01),
                dec!(40_000),
                Decimal::ZERO,
            )
            .unwrap();

        assert_eq!(order_at_limit, dec!(100));
        assert_eq!(position_at_limit, dec!(100));
    }

    #[test]
    fn sizing_rejects_a_stop_on_the_wrong_side_of_entry() {
        let intent = OrderIntent::new(
            1704067200000,
            Side::Buy,
            dec!(100),
            dec!(101),
            dec!(102),
            15,
        )
        .unwrap();

        let result = RiskPolicy::default().size_order(
            &intent,
            dec!(100),
            dec!(1_000),
            dec!(0.01),
            Decimal::ZERO,
            Decimal::ZERO,
        );

        assert!(matches!(result, Err(RiskError::InvalidParameters(_))));
    }

    #[test]
    fn session_drawdown_uses_marked_equity_including_unrealized_pnl() {
        let drawdown =
            RiskPolicy::calculate_session_drawdown_pct(dec!(10_000), dec!(9_400)).unwrap();
        assert_eq!(drawdown, dec!(0.06));

        let result = RiskPolicy::default().size_order(
            &sample_intent(dec!(100)),
            dec!(100),
            dec!(9_400),
            dec!(0.01),
            Decimal::ZERO,
            drawdown,
        );

        assert!(matches!(
            result,
            Err(RiskError::CircuitBreakerTriggered { .. })
        ));
    }

    #[test]
    fn session_drawdown_at_limit_blocks_sizing() {
        let drawdown =
            RiskPolicy::calculate_session_drawdown_pct(dec!(10_000), dec!(9_500)).unwrap();

        let result = RiskPolicy::default().size_order(
            &sample_intent(dec!(100)),
            dec!(100),
            dec!(9_500),
            dec!(0.01),
            Decimal::ZERO,
            drawdown,
        );

        assert_eq!(drawdown, dec!(0.05));
        assert!(matches!(
            result,
            Err(RiskError::CircuitBreakerTriggered { .. })
        ));
    }

    #[test]
    fn sizing_uses_simulated_fill_price_for_stop_distance() {
        let intent =
            OrderIntent::new(1704067200000, Side::Buy, dec!(100), dec!(98), dec!(110), 15).unwrap();

        let quantity = RiskPolicy::default()
            .size_order(
                &intent,
                dec!(102),
                dec!(10_000),
                dec!(0.01),
                Decimal::ZERO,
                Decimal::ZERO,
            )
            .unwrap();

        assert_eq!(quantity, dec!(25));
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
