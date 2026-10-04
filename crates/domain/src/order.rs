use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::trade::Side;

/// Order execution type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderType {
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
}

impl OrderIntent {
    /// Constructs a validated OrderIntent.
    pub fn new(
        timestamp: i64,
        side: Side,
        price: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
        max_bars_hold: usize,
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
        Ok(Self {
            timestamp,
            side,
            price,
            stop_loss,
            take_profit,
            max_bars_hold,
        })
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
}
