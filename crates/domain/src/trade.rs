use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::DomainError;

/// Side of market execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// Normalized market transaction emitted by an exchange or historical replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trade {
    /// Timestamp in milliseconds (Unix epoch).
    pub timestamp: i64,
    /// Transaction price in quote currency (e.g., USDT).
    pub price: Decimal,
    /// Transaction volume in base asset (e.g., BTC).
    pub quantity: Decimal,
    /// Aggressor side of trade.
    pub side: Side,
}

impl Trade {
    /// Creates a validated Trade instance.
    pub fn new(
        timestamp: i64,
        price: Decimal,
        quantity: Decimal,
        side: Side,
    ) -> Result<Self, DomainError> {
        if price <= Decimal::ZERO {
            return Err(DomainError::InvalidPrice(price.to_string()));
        }
        if quantity <= Decimal::ZERO {
            return Err(DomainError::InvalidQuantity(quantity.to_string()));
        }
        Ok(Self {
            timestamp,
            price,
            quantity,
            side,
        })
    }

    /// Computes the notional dollar value of the trade (Price * Quantity).
    #[inline]
    pub fn dollar_value(&self) -> Decimal {
        self.price * self.quantity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn valid_trade_creation() {
        let trade = Trade::new(1700000000, dec!(50000), dec!(1.5), Side::Buy);
        assert!(trade.is_ok());
        let trade = trade.unwrap();
        assert_eq!(trade.dollar_value(), dec!(75000));
    }

    #[test]
    fn reject_zero_or_negative_price() {
        let zero_res = Trade::new(1700000000, dec!(0), dec!(1.0), Side::Buy);
        assert_eq!(zero_res, Err(DomainError::InvalidPrice("0".to_string())));

        let neg_res = Trade::new(1700000000, dec!(-100), dec!(1.0), Side::Buy);
        assert_eq!(neg_res, Err(DomainError::InvalidPrice("-100".to_string())));
    }

    #[test]
    fn reject_zero_or_negative_quantity() {
        let zero_res = Trade::new(1700000000, dec!(50000), dec!(0), Side::Buy);
        assert_eq!(zero_res, Err(DomainError::InvalidQuantity("0".to_string())));

        let neg_res = Trade::new(1700000000, dec!(50000), dec!(-0.5), Side::Buy);
        assert_eq!(
            neg_res,
            Err(DomainError::InvalidQuantity("-0.5".to_string()))
        );
    }
}
