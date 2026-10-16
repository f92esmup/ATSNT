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
    /// Sized quantity falls below the exchange minimum lot (LOT_SIZE `minQty`).
    #[error("Order quantity {quantity} is below the minimum lot size {min_quantity}")]
    OrderBelowMinLotSize {
        quantity: String,
        min_quantity: String,
    },
    /// Sized order value falls below the exchange minimum notional (MIN_NOTIONAL).
    #[error("Order notional {notional} is below the minimum notional {min_notional}")]
    OrderBelowMinNotional {
        notional: String,
        min_notional: String,
    },
}

fn default_max_leverage() -> Decimal {
    Decimal::new(20, 0)
}

fn default_min_order_quantity() -> Decimal {
    Decimal::new(1, 3)
}

fn default_quantity_step_size() -> Decimal {
    Decimal::new(1, 3)
}

fn default_min_order_notional() -> Decimal {
    Decimal::new(5, 0)
}

/// Pre-trade risk policy configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskPolicy {
    /// Maximum allowed position notional value in quote currency (e.g. 50,000 USDT).
    pub max_position_notional: Decimal,
    /// Maximum order notional size allowed in a single order (e.g. 10,000 USDT).
    pub max_order_notional: Decimal,
    /// Maximum drawdown from the peak supplied by the caller (e.g. 0.05 = 5%).
    ///
    /// The policy applies no reset itself: the simulator and the live loop both pass the
    /// marked-equity peak of the current UTC day.
    pub max_daily_drawdown_pct: Decimal,
    /// Maximum effective leverage; sized quantity is clamped to `equity * max_leverage / price`.
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

impl Default for RiskPolicy {
    fn default() -> Self {
        Self {
            max_position_notional: Decimal::new(50_000, 0),
            max_order_notional: Decimal::new(10_000, 0),
            max_daily_drawdown_pct: Decimal::new(5, 2), // 0.05
            max_leverage: default_max_leverage(),
            min_order_quantity: default_min_order_quantity(),
            quantity_step_size: default_quantity_step_size(),
            min_order_notional: default_min_order_notional(),
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
            max_leverage: default_max_leverage(),
            min_order_quantity: default_min_order_quantity(),
            quantity_step_size: default_quantity_step_size(),
            min_order_notional: default_min_order_notional(),
        })
    }

    /// Returns a copy with exchange execution rules (leverage cap, lot and notional filters).
    pub fn with_execution_rules(
        mut self,
        max_leverage: Decimal,
        min_order_quantity: Decimal,
        quantity_step_size: Decimal,
        min_order_notional: Decimal,
    ) -> Result<Self, RiskError> {
        for (name, value) in [
            ("max_leverage", max_leverage),
            ("min_order_quantity", min_order_quantity),
            ("quantity_step_size", quantity_step_size),
            ("min_order_notional", min_order_notional),
        ] {
            if value <= Decimal::ZERO {
                return Err(RiskError::InvalidParameters(format!(
                    "{name} must be positive"
                )));
            }
        }
        self.max_leverage = max_leverage;
        self.min_order_quantity = min_order_quantity;
        self.quantity_step_size = quantity_step_size;
        self.min_order_notional = min_order_notional;
        Ok(self)
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
    /// The quantity is clamped to `max_leverage` and floored to the lot step, but never
    /// clamped to fit a notional limit: a breach of those limits rejects the order.
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

        // A tripped session breaker must win over any sizing error on a degraded account.
        if current_session_drawdown_pct >= self.max_daily_drawdown_pct {
            return Err(RiskError::CircuitBreakerTriggered {
                current_pct: current_session_drawdown_pct.to_string(),
                limit_pct: self.max_daily_drawdown_pct.to_string(),
            });
        }

        let risk_quantity = (mark_to_market_equity * risk_per_trade_pct) / stop_distance;
        // Leverage cap: tight stops must not inflate the position beyond max_leverage.
        let max_quantity = (mark_to_market_equity * self.max_leverage) / execution_price;
        let capped = risk_quantity.min(max_quantity);
        // Exchange LOT_SIZE: floor to a whole number of steps.
        let quantity = (capped / self.quantity_step_size).floor() * self.quantity_step_size;

        if quantity < self.min_order_quantity {
            return Err(RiskError::OrderBelowMinLotSize {
                quantity: quantity.to_string(),
                min_quantity: self.min_order_quantity.to_string(),
            });
        }
        let notional = quantity * execution_price;
        if notional < self.min_order_notional {
            return Err(RiskError::OrderBelowMinNotional {
                notional: notional.to_string(),
                min_notional: self.min_order_notional.to_string(),
            });
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

    #[test]
    fn degraded_account_is_rejected_below_min_lot_instead_of_dust_position() {
        let result = RiskPolicy::default().size_order(
            &sample_intent(dec!(100)),
            dec!(100),
            dec!(0.1),
            dec!(0.01),
            Decimal::ZERO,
            Decimal::ZERO,
        );

        assert!(matches!(
            result,
            Err(RiskError::OrderBelowMinLotSize { .. })
        ));
    }

    #[test]
    fn quantity_below_min_notional_is_rejected() {
        let policy = RiskPolicy::default()
            .with_execution_rules(dec!(20), dec!(0.001), dec!(0.001), dec!(100))
            .unwrap();
        // risk qty = 100 * 0.01 / 2 = 0.5 -> notional 50 < 100
        let result = policy.size_order(
            &sample_intent(dec!(100)),
            dec!(100),
            dec!(100),
            dec!(0.01),
            Decimal::ZERO,
            Decimal::ZERO,
        );

        assert!(matches!(
            result,
            Err(RiskError::OrderBelowMinNotional { .. })
        ));
    }

    #[test]
    fn tight_stop_is_capped_at_max_leverage() {
        let policy = RiskPolicy::new(dec!(10_000_000), dec!(10_000_000), dec!(0.05)).unwrap();
        let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(99.9), dec!(101), 15).unwrap();
        // risk qty = 10_000 * 0.01 / 0.1 = 1000 (10x of 100 price = 100_000 notional = 10x? no: 100k/10k = 10x)
        // use 5% risk so uncapped leverage would be 50x.
        let quantity = policy
            .size_order(
                &intent,
                dec!(100),
                dec!(10_000),
                dec!(0.05),
                Decimal::ZERO,
                Decimal::ZERO,
            )
            .unwrap();

        assert_eq!(quantity * dec!(100), dec!(200_000));
    }

    #[test]
    fn sized_quantity_is_exact_multiple_of_step() {
        let quantity = RiskPolicy::default()
            .size_order(
                &sample_intent(dec!(30_000)),
                dec!(30_000),
                dec!(10_000),
                dec!(0.01),
                Decimal::ZERO,
                Decimal::ZERO,
            )
            .unwrap();

        assert_eq!(quantity % dec!(0.001), Decimal::ZERO);
        assert_eq!(quantity, dec!(0.166));
    }

    #[test]
    fn tripped_breaker_wins_over_min_lot_error() {
        let result = RiskPolicy::default().size_order(
            &sample_intent(dec!(100)),
            dec!(100),
            dec!(0.5),
            dec!(0.01),
            Decimal::ZERO,
            dec!(0.06),
        );

        assert!(matches!(
            result,
            Err(RiskError::CircuitBreakerTriggered { .. })
        ));
    }

    #[test]
    fn legacy_serialized_policy_without_execution_rules_still_loads() {
        let json = r#"{"max_position_notional":"50000","max_order_notional":"10000","max_daily_drawdown_pct":"0.05"}"#;
        let policy: RiskPolicy = serde_json::from_str(json).unwrap();

        assert_eq!(policy, RiskPolicy::default());
    }
}
