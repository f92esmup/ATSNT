use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::trade::Side;

/// Directional orientation of an open market position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PositionSide {
    Long,
    Short,
}

/// Active portfolio position with deterministic fixed-point PnL accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub side: PositionSide,
    pub quantity: Decimal,
    pub entry_price: Decimal,
    pub realized_pnl: Decimal,
}

impl Position {
    /// Opens a new position with non-zero quantity and positive entry price.
    pub fn new(
        side: PositionSide,
        quantity: Decimal,
        entry_price: Decimal,
    ) -> Result<Self, DomainError> {
        if quantity <= Decimal::ZERO {
            return Err(DomainError::InvalidQuantity(quantity.to_string()));
        }
        if entry_price <= Decimal::ZERO {
            return Err(DomainError::InvalidPrice(entry_price.to_string()));
        }
        Ok(Self {
            side,
            quantity,
            entry_price,
            realized_pnl: Decimal::ZERO,
        })
    }

    /// Computes unrealized PnL based on a mark/current market price.
    #[inline]
    pub fn unrealized_pnl(&self, current_price: Decimal) -> Decimal {
        match self.side {
            PositionSide::Long => (current_price - self.entry_price) * self.quantity,
            PositionSide::Short => (self.entry_price - current_price) * self.quantity,
        }
    }

    /// Applies a trade fill to the position:
    /// - If in the same direction, increases position size and adjusts average entry price.
    /// - If in the opposite direction, closes/reduces position and calculates realized PnL.
    pub fn apply_fill(
        &mut self,
        fill_side: Side,
        fill_qty: Decimal,
        fill_price: Decimal,
    ) -> Result<(), DomainError> {
        if fill_qty <= Decimal::ZERO {
            return Err(DomainError::InvalidQuantity(fill_qty.to_string()));
        }
        if fill_price <= Decimal::ZERO {
            return Err(DomainError::InvalidPrice(fill_price.to_string()));
        }

        let is_increasing = match (self.side, fill_side) {
            (PositionSide::Long, Side::Buy) | (PositionSide::Short, Side::Sell) => true,
            (PositionSide::Long, Side::Sell) | (PositionSide::Short, Side::Buy) => false,
        };

        if is_increasing {
            let total_notional = (self.entry_price * self.quantity) + (fill_price * fill_qty);
            self.quantity += fill_qty;
            self.entry_price = total_notional / self.quantity;
        } else {
            let close_qty = fill_qty.min(self.quantity);
            let pnl = match self.side {
                PositionSide::Long => (fill_price - self.entry_price) * close_qty,
                PositionSide::Short => (self.entry_price - fill_price) * close_qty,
            };
            self.realized_pnl += pnl;
            self.quantity -= close_qty;

            // Note: If fill_qty > self.quantity (position flip), remaining qty could reverse position.
            // For strict domain boundaries, our simulator handles position flips as two discrete steps.
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn long_position_unrealized_and_realized_pnl() {
        let mut pos = Position::new(PositionSide::Long, dec!(2.0), dec!(50000)).unwrap();

        // Price rises to 55,000 -> Unrealized PnL: (55,000 - 50,000) * 2 = 10,000
        assert_eq!(pos.unrealized_pnl(dec!(55000)), dec!(10000));

        // Close 1.0 at 55,000 -> Realized PnL: +5,000, Remaining Qty: 1.0
        assert!(pos.apply_fill(Side::Sell, dec!(1.0), dec!(55000)).is_ok());
        assert_eq!(pos.realized_pnl, dec!(5000));
        assert_eq!(pos.quantity, dec!(1.0));
        assert_eq!(pos.entry_price, dec!(50000));
    }

    #[test]
    fn short_position_unrealized_and_realized_pnl() {
        let mut pos = Position::new(PositionSide::Short, dec!(1.0), dec!(60000)).unwrap();

        // Price drops to 58,000 -> Unrealized PnL: (60,000 - 58,000) * 1 = 2,000
        assert_eq!(pos.unrealized_pnl(dec!(58000)), dec!(2000));

        // Price rises to 62,000 -> Unrealized PnL: (60,000 - 62,000) * 1 = -2,000
        assert_eq!(pos.unrealized_pnl(dec!(62000)), dec!(-2000));

        // Close at 58,000 -> Realized PnL: +2,000
        assert!(pos.apply_fill(Side::Buy, dec!(1.0), dec!(58000)).is_ok());
        assert_eq!(pos.realized_pnl, dec!(2000));
        assert_eq!(pos.quantity, dec!(0.0));
    }

    #[test]
    fn position_scale_in_averages_entry_price() {
        let mut pos = Position::new(PositionSide::Long, dec!(1.0), dec!(50000)).unwrap();

        // Add 1.0 at 60,000 -> Average entry = (50,000 + 60,000) / 2 = 55,000
        assert!(pos.apply_fill(Side::Buy, dec!(1.0), dec!(60000)).is_ok());
        assert_eq!(pos.quantity, dec!(2.0));
        assert_eq!(pos.entry_price, dec!(55000));
    }
}
