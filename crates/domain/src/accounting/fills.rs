//! Fill arithmetic: the price a market order fills at, the fee a fill is charged and the average
//! price of several fills. Decimal math over plain inputs, shared by every runner that simulates
//! fills.

use rust_decimal::Decimal;

use crate::{PositionSide, Side};

/// Fill price of a market order on `side` against `reference`, and the slippage per unit.
///
/// A buy pays `reference + reference * pct`; a sell receives `reference - reference * pct`.
pub fn market_fill_price(
    side: Side,
    reference: Decimal,
    slippage_pct: Decimal,
) -> (Decimal, Decimal) {
    let slip = reference * slippage_pct;
    let fill_price = match side {
        Side::Buy => reference + slip,
        Side::Sell => reference - slip,
    };
    (fill_price, slip)
}

/// The order side that closes a position on `side`.
pub fn closing_side(side: PositionSide) -> Side {
    match side {
        PositionSide::Long => Side::Sell,
        PositionSide::Short => Side::Buy,
    }
}

/// The order side that opened a position on `side`.
pub fn opening_side(side: PositionSide) -> Side {
    match side {
        PositionSide::Long => Side::Buy,
        PositionSide::Short => Side::Sell,
    }
}

/// The position side an opening order on `side` creates.
pub fn position_side(side: Side) -> PositionSide {
    match side {
        Side::Buy => PositionSide::Long,
        Side::Sell => PositionSide::Short,
    }
}

/// The side of a signed quantity; `None` for 0.
pub fn signed_side(quantity: Decimal) -> Option<PositionSide> {
    if quantity > Decimal::ZERO {
        Some(PositionSide::Long)
    } else if quantity < Decimal::ZERO {
        Some(PositionSide::Short)
    } else {
        None
    }
}

/// Fee of a fill of `quantity` at `price` charged at `rate`: `(price · quantity) · rate`.
pub fn fill_fee(price: Decimal, quantity: Decimal, rate: Decimal) -> Decimal {
    price * quantity * rate
}

/// Volume-weighted average price of fills summing to `notional` over `quantity`.
pub fn vwap(notional: Decimal, quantity: Decimal) -> Decimal {
    if quantity > Decimal::ZERO {
        notional / quantity
    } else {
        Decimal::ZERO
    }
}

/// Slippage per unit an opening market order on `side` reports: the model slippage `slip` on
/// the `open` plus the adverse part of the gap from the `reference` price the order was sized
/// at. A gap in favour does not cancel the slippage.
pub fn opening_slippage_per_unit(
    side: Side,
    slip: Decimal,
    open: Decimal,
    reference: Decimal,
) -> Decimal {
    slip + match side {
        Side::Buy => (open - reference).max(Decimal::ZERO),
        Side::Sell => (reference - open).max(Decimal::ZERO),
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn market_fill_price_adds_slippage_on_buys_and_subtracts_on_sells() {
        assert_eq!(
            market_fill_price(Side::Buy, dec!(100), dec!(0.001)),
            (dec!(100.1), dec!(0.1))
        );
        assert_eq!(
            market_fill_price(Side::Sell, dec!(100), dec!(0.001)),
            (dec!(99.9), dec!(0.1))
        );
    }

    #[test]
    fn an_adverse_gap_adds_to_the_opening_slippage_and_a_gap_in_favour_does_not_cancel_it() {
        assert_eq!(
            opening_slippage_per_unit(Side::Buy, dec!(0.1), dec!(101), dec!(100)),
            dec!(1.1)
        );
        assert_eq!(
            opening_slippage_per_unit(Side::Buy, dec!(0.1), dec!(99), dec!(100)),
            dec!(0.1)
        );
    }

    #[test]
    fn a_fill_is_charged_its_notional_times_the_rate() {
        assert_eq!(fill_fee(dec!(100), dec!(2), dec!(0.001)), dec!(0.2));
    }

    #[test]
    fn a_zero_quantity_has_no_side_and_no_average_price() {
        assert_eq!(signed_side(Decimal::ZERO), None);
        assert_eq!(vwap(dec!(100), Decimal::ZERO), Decimal::ZERO);
    }
}
