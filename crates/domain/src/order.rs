use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::trade::Side;

/// Order execution type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum OrderType {
    #[default]
    Market,
    Limit,
}

/// Time in force execution instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimeInForce {
    GoodTilCanceled,
    ImmediateOrCancel,
}

/// Strict lifecycle state machine for orders, satisfying AGENTS.md requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    PendingNew,
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

/// Pure trading intent emitted by a strategy without broker/exchange specific details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderIntent {
    /// Timestamp when signal was generated (Unix ms).
    pub timestamp: i64,
    /// Direction of desired position.
    pub side: Side,
    /// Desired limit or reference price.
    pub price: Decimal,
    /// Stop Loss barrier price (dynamic Z-Score exit).
    pub stop_loss: Decimal,
    /// Take Profit barrier price (Z = 0 exit).
    pub take_profit: Decimal,
    /// Horizontal time barrier (maximum bars to hold before forced exit).
    pub max_bars_hold: usize,
    /// Optional position risk fraction calculated at entry (e.g. from consensus conviction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_fraction: Option<Decimal>,
    /// Optional ATR distance for adaptive breakeven and trailing stop ratchet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing_atr: Option<Decimal>,
    /// Optional ATR or price distance for breakeven trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakeven_atr: Option<Decimal>,
    /// Optional timestamp deadline in Unix ms for real-time / intraday time barriers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<i64>,
    /// Order execution type: Market (aggressive Taker) or Limit (passive Maker).
    #[serde(default)]
    pub order_type: OrderType,
    /// Optional Time-In-Force timeout in number of bars for Limit orders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_in_force_bars: Option<usize>,
    /// Whether the order must execute strictly as a Maker order (Post-Only).
    #[serde(default)]
    pub post_only: bool,
}

impl OrderIntent {
    /// Constructs a validated OrderIntent with default portfolio risk.
    pub fn new(
        timestamp: i64,
        side: Side,
        price: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
        max_bars_hold: usize,
    ) -> Result<Self, DomainError> {
        Self::with_risk_and_exits(
            timestamp,
            side,
            price,
            stop_loss,
            take_profit,
            max_bars_hold,
            None,
            None,
            None,
            None,
        )
    }

    /// Constructs a validated OrderIntent with an optional conviction risk fraction.
    pub fn with_risk(
        timestamp: i64,
        side: Side,
        price: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
        max_bars_hold: usize,
        risk_fraction: Option<Decimal>,
    ) -> Result<Self, DomainError> {
        Self::with_risk_and_exits(
            timestamp,
            side,
            price,
            stop_loss,
            take_profit,
            max_bars_hold,
            risk_fraction,
            None,
            None,
            None,
        )
    }

    /// Constructs a validated OrderIntent with optional risk fraction, trailing ATR distance, and deadline.
    #[allow(clippy::too_many_arguments)]
    pub fn with_risk_and_trailing(
        timestamp: i64,
        side: Side,
        price: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
        max_bars_hold: usize,
        risk_fraction: Option<Decimal>,
        trailing_atr: Option<Decimal>,
        deadline_ms: Option<i64>,
    ) -> Result<Self, DomainError> {
        Self::with_risk_and_exits(
            timestamp,
            side,
            price,
            stop_loss,
            take_profit,
            max_bars_hold,
            risk_fraction,
            trailing_atr,
            None,
            deadline_ms,
        )
    }

    /// Constructs a validated OrderIntent with optional risk fraction, trailing ATR distance, breakeven distance, and deadline.
    #[allow(clippy::too_many_arguments)]
    pub fn with_risk_and_exits(
        timestamp: i64,
        side: Side,
        price: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
        max_bars_hold: usize,
        risk_fraction: Option<Decimal>,
        trailing_atr: Option<Decimal>,
        breakeven_atr: Option<Decimal>,
        deadline_ms: Option<i64>,
    ) -> Result<Self, DomainError> {
        if price <= Decimal::ZERO {
            return Err(DomainError::InvalidPrice(price.to_string()));
        }
        if stop_loss <= Decimal::ZERO {
            return Err(DomainError::InvalidPrice(stop_loss.to_string()));
        }
        if take_profit <= Decimal::ZERO {
            return Err(DomainError::InvalidPrice(take_profit.to_string()));
        }
        if let Some(risk) = risk_fraction {
            if risk <= Decimal::ZERO || risk > Decimal::ONE {
                return Err(DomainError::InvalidTarget(format!(
                    "risk fraction {risk} outside (0, 1]"
                )));
            }
        }
        if let Some(atr) = trailing_atr {
            if atr <= Decimal::ZERO {
                return Err(DomainError::InvalidTarget(format!(
                    "trailing ATR distance {atr} must be positive"
                )));
            }
        }
        if let Some(be) = breakeven_atr {
            if be <= Decimal::ZERO {
                return Err(DomainError::InvalidTarget(format!(
                    "breakeven ATR distance {be} must be positive"
                )));
            }
        }
        Ok(Self {
            timestamp,
            side,
            price,
            stop_loss,
            take_profit,
            max_bars_hold,
            risk_fraction,
            trailing_atr,
            breakeven_atr,
            deadline_ms,
            order_type: OrderType::Market,
            time_in_force_bars: None,
            post_only: false,
        })
    }

    /// Converts this intent into a passive Limit order with an optional Time-In-Force (bars) and post-only flag.
    pub fn into_limit(mut self, time_in_force_bars: Option<usize>, post_only: bool) -> Self {
        self.order_type = OrderType::Limit;
        self.time_in_force_bars = time_in_force_bars;
        self.post_only = post_only;
        self
    }

    /// Constructs a Limit order intent with a specific limit price, time-in-force, and post-only flag.
    #[allow(clippy::too_many_arguments)]
    pub fn limit(
        timestamp: i64,
        side: Side,
        limit_price: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
        max_bars_hold: usize,
        time_in_force_bars: Option<usize>,
        post_only: bool,
    ) -> Result<Self, DomainError> {
        let mut intent = Self::new(
            timestamp,
            side,
            limit_price,
            stop_loss,
            take_profit,
            max_bars_hold,
        )?;
        intent.order_type = OrderType::Limit;
        intent.time_in_force_bars = time_in_force_bars;
        intent.post_only = post_only;
        Ok(intent)
    }
}

/// Managed order entity with explicit lifecycle state transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub timestamp: i64,
    pub side: Side,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub price: Option<Decimal>,
    pub quantity: Decimal,
    pub filled_quantity: Decimal,
    pub status: OrderStatus,
}

impl Order {
    /// Constructs a new order initialized in `PendingNew` state.
    pub fn new(
        id: String,
        timestamp: i64,
        side: Side,
        order_type: OrderType,
        time_in_force: TimeInForce,
        price: Option<Decimal>,
        quantity: Decimal,
    ) -> Result<Self, DomainError> {
        if quantity <= Decimal::ZERO {
            return Err(DomainError::InvalidQuantity(quantity.to_string()));
        }
        if let Some(p) = price {
            if p <= Decimal::ZERO {
                return Err(DomainError::InvalidPrice(p.to_string()));
            }
        }
        Ok(Self {
            id,
            timestamp,
            side,
            order_type,
            time_in_force,
            price,
            quantity,
            filled_quantity: Decimal::ZERO,
            status: OrderStatus::PendingNew,
        })
    }

    /// Remaining quantity waiting to be filled.
    #[inline]
    pub fn remaining_quantity(&self) -> Decimal {
        self.quantity - self.filled_quantity
    }

    /// Transitions order from `PendingNew` to `New` when accepted by the exchange or book.
    pub fn transition_to_new(&mut self) -> Result<(), DomainError> {
        match self.status {
            OrderStatus::PendingNew => {
                self.status = OrderStatus::New;
                Ok(())
            }
            other => Err(DomainError::InvalidStateTransition {
                from: format!("{other:?}"),
                to: format!("{:?}", OrderStatus::New),
            }),
        }
    }

    /// Records an execution fill, transitioning to `PartiallyFilled` or `Filled`.
    pub fn transition_to_fill(&mut self, fill_qty: Decimal) -> Result<(), DomainError> {
        if fill_qty <= Decimal::ZERO {
            return Err(DomainError::InvalidQuantity(fill_qty.to_string()));
        }

        match self.status {
            OrderStatus::New | OrderStatus::PartiallyFilled => {
                let new_filled = self.filled_quantity + fill_qty;
                if new_filled > self.quantity {
                    return Err(DomainError::OverfillError);
                }

                self.filled_quantity = new_filled;
                if self.filled_quantity == self.quantity {
                    self.status = OrderStatus::Filled;
                } else {
                    self.status = OrderStatus::PartiallyFilled;
                }
                Ok(())
            }
            other => Err(DomainError::InvalidStateTransition {
                from: format!("{other:?}"),
                to: format!("{:?}", OrderStatus::Filled),
            }),
        }
    }

    /// Transitions order to `Canceled`. Cannot cancel already terminated orders.
    pub fn transition_to_canceled(&mut self) -> Result<(), DomainError> {
        match self.status {
            OrderStatus::PendingNew | OrderStatus::New | OrderStatus::PartiallyFilled => {
                self.status = OrderStatus::Canceled;
                Ok(())
            }
            other => Err(DomainError::InvalidStateTransition {
                from: format!("{other:?}"),
                to: format!("{:?}", OrderStatus::Canceled),
            }),
        }
    }

    /// Transitions order to `Rejected` (e.g., margin check or exchange validation failure).
    pub fn transition_to_rejected(&mut self) -> Result<(), DomainError> {
        match self.status {
            OrderStatus::PendingNew | OrderStatus::New => {
                self.status = OrderStatus::Rejected;
                Ok(())
            }
            other => Err(DomainError::InvalidStateTransition {
                from: format!("{other:?}"),
                to: format!("{:?}", OrderStatus::Rejected),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn order_lifecycle_full_fill() {
        let mut order = Order::new(
            "ord-1".into(),
            1000,
            Side::Buy,
            OrderType::Limit,
            TimeInForce::GoodTilCanceled,
            Some(dec!(60000)),
            dec!(2.0),
        )
        .unwrap();

        assert_eq!(order.status, OrderStatus::PendingNew);
        assert_eq!(order.remaining_quantity(), dec!(2.0));

        // Accept order
        assert!(order.transition_to_new().is_ok());
        assert_eq!(order.status, OrderStatus::New);

        // Partial fill of 0.5
        assert!(order.transition_to_fill(dec!(0.5)).is_ok());
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
        assert_eq!(order.remaining_quantity(), dec!(1.5));

        // Final fill of 1.5
        assert!(order.transition_to_fill(dec!(1.5)).is_ok());
        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(order.remaining_quantity(), dec!(0.0));

        // Attempting to cancel a Filled order must fail
        assert!(order.transition_to_canceled().is_err());
    }

    #[test]
    fn order_lifecycle_rejection_and_cancellation() {
        let mut ord_to_cancel = Order::new(
            "ord-2".into(),
            1000,
            Side::Sell,
            OrderType::Market,
            TimeInForce::ImmediateOrCancel,
            None,
            dec!(1.0),
        )
        .unwrap();

        assert!(ord_to_cancel.transition_to_canceled().is_ok());
        assert_eq!(ord_to_cancel.status, OrderStatus::Canceled);

        // Cannot fill a canceled order
        assert!(ord_to_cancel.transition_to_fill(dec!(0.5)).is_err());

        let mut ord_to_reject = Order::new(
            "ord-3".into(),
            1000,
            Side::Buy,
            OrderType::Limit,
            TimeInForce::GoodTilCanceled,
            Some(dec!(50000)),
            dec!(1.0),
        )
        .unwrap();

        assert!(ord_to_reject.transition_to_rejected().is_ok());
        assert_eq!(ord_to_reject.status, OrderStatus::Rejected);
    }

    #[test]
    fn prevent_overfilling_order() {
        let mut order = Order::new(
            "ord-4".into(),
            1000,
            Side::Buy,
            OrderType::Limit,
            TimeInForce::GoodTilCanceled,
            Some(dec!(50000)),
            dec!(1.0),
        )
        .unwrap();

        order.transition_to_new().unwrap();
        // Overfill of 1.5 against 1.0 quantity
        let res = order.transition_to_fill(dec!(1.5));
        assert_eq!(res, Err(DomainError::OverfillError));
        assert_eq!(order.status, OrderStatus::New);
    }

    #[test]
    fn order_intent_limit_properties() {
        let default_intent =
            OrderIntent::new(1000, Side::Buy, dec!(50000), dec!(49000), dec!(52000), 5).unwrap();
        assert_eq!(default_intent.order_type, OrderType::Market);
        assert_eq!(default_intent.time_in_force_bars, None);
        assert!(!default_intent.post_only);

        let limit_converted = default_intent.into_limit(Some(3), true);
        assert_eq!(limit_converted.order_type, OrderType::Limit);
        assert_eq!(limit_converted.time_in_force_bars, Some(3));
        assert!(limit_converted.post_only);

        let direct_limit = OrderIntent::limit(
            2000,
            Side::Sell,
            dec!(51000),
            dec!(52000),
            dec!(48000),
            10,
            Some(2),
            true,
        )
        .unwrap();
        assert_eq!(direct_limit.order_type, OrderType::Limit);
        assert_eq!(direct_limit.time_in_force_bars, Some(2));
        assert!(direct_limit.post_only);
    }
}
