//! Perpetual-futures account rules shared by every environment: funding settlements, minimum
//! leverage and isolated-margin liquidation.
//!
//! Pure functions over `Decimal`, so the simulators and the live runner price the same events
//! with the same formulas.

use rust_decimal::Decimal;

use crate::trade::Side;

/// Milliseconds between funding settlements of an eight-hour perpetual (00:00, 08:00 and 16:00
/// UTC), the schedule of Binance USD-M BTC and most majors.
pub const FUNDING_INTERVAL_MS: i64 = 8 * 60 * 60 * 1_000;

fn direction(side: Side) -> Decimal {
    match side {
        Side::Buy => Decimal::ONE,
        Side::Sell => -Decimal::ONE,
    }
}

/// Funding settlement instants in `(after_ms, up_to_ms]`, oldest first.
///
/// Settlements sit on multiples of `interval_ms` since the Unix epoch. An empty list is
/// returned for a non-positive interval or an empty range.
pub fn funding_settlements(interval_ms: i64, after_ms: i64, up_to_ms: i64) -> Vec<i64> {
    if interval_ms <= 0 || up_to_ms <= after_ms {
        return Vec::new();
    }
    let first = after_ms.div_euclid(interval_ms) + 1;
    let last = up_to_ms.div_euclid(interval_ms);
    (first..=last).map(|n| n * interval_ms).collect()
}

/// Funding paid by a position at one settlement; negative when the position receives it.
///
/// With a positive rate longs pay shorts, with a negative rate shorts pay longs.
pub fn funding_payment(
    side: Side,
    quantity: Decimal,
    mark_price: Decimal,
    rate: Decimal,
) -> Decimal {
    direction(side) * quantity * mark_price * rate
}

/// Lowest leverage in `1..=maximum_leverage` whose initial margin (never below the maintenance
/// margin) plus the entry commission fits in `available_balance`.
pub fn minimum_leverage(
    notional: Decimal,
    maintenance_margin: Decimal,
    commission: Decimal,
    available_balance: Decimal,
    maximum_leverage: u32,
) -> Option<u32> {
    if available_balance <= Decimal::ZERO {
        return None;
    }
    (1..=maximum_leverage).find(|leverage| {
        let initial_margin = notional / Decimal::from(*leverage);
        initial_margin.max(maintenance_margin) + commission <= available_balance
    })
}

/// Price at which an isolated position is liquidated: the mark where the margin left after the
/// unrealized loss only covers the maintenance margin.
pub fn liquidation_price(
    side: Side,
    entry_price: Decimal,
    quantity: Decimal,
    initial_margin: Decimal,
    maintenance_margin: Decimal,
) -> Decimal {
    entry_price - direction(side) * (initial_margin - maintenance_margin) / quantity
}

/// Price at which an isolated position has lost its whole initial margin. The liquidation engine
/// closes there, so a liquidated position loses exactly its margin.
pub fn bankruptcy_price(
    side: Side,
    entry_price: Decimal,
    quantity: Decimal,
    initial_margin: Decimal,
) -> Decimal {
    entry_price - direction(side) * initial_margin / quantity
}

/// Whether `mark_price` has reached the liquidation price of a position on `side`.
pub fn is_liquidated(side: Side, mark_price: Decimal, liquidation_price: Decimal) -> bool {
    match side {
        Side::Buy => mark_price <= liquidation_price,
        Side::Sell => mark_price >= liquidation_price,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const HOUR: i64 = 60 * 60 * 1_000;

    #[test]
    fn settlements_sit_on_eight_hour_multiples_since_the_epoch() {
        // 2026-01-01T07:00Z to 2026-01-01T17:00Z crosses the 08:00 and 16:00 settlements.
        let day = 1_767_225_600_000; // 2026-01-01T00:00:00Z
        let found = funding_settlements(FUNDING_INTERVAL_MS, day + 7 * HOUR, day + 17 * HOUR);

        assert_eq!(found, vec![day + 8 * HOUR, day + 16 * HOUR]);
    }

    #[test]
    fn a_settlement_belongs_to_the_range_that_ends_on_it_but_not_to_the_next() {
        let at = 5 * FUNDING_INTERVAL_MS;

        assert_eq!(
            funding_settlements(FUNDING_INTERVAL_MS, at - 1, at),
            vec![at]
        );
        assert!(funding_settlements(FUNDING_INTERVAL_MS, at, at + 1).is_empty());
    }

    #[test]
    fn empty_ranges_and_bad_intervals_have_no_settlements() {
        assert!(funding_settlements(FUNDING_INTERVAL_MS, 10, 10).is_empty());
        assert!(funding_settlements(FUNDING_INTERVAL_MS, 10, 5).is_empty());
        assert!(funding_settlements(0, 0, 100).is_empty());
        assert!(funding_settlements(-1, 0, 100).is_empty());
    }

    #[test]
    fn a_long_pays_a_positive_rate_and_a_short_receives_it() {
        let long = funding_payment(Side::Buy, dec!(2), dec!(50_000), dec!(0.0001));
        let short = funding_payment(Side::Sell, dec!(2), dec!(50_000), dec!(0.0001));

        assert_eq!(long, dec!(10));
        assert_eq!(short, dec!(-10));
    }

    #[test]
    fn a_negative_rate_reverses_who_pays() {
        let long = funding_payment(Side::Buy, dec!(2), dec!(50_000), dec!(-0.0001));

        assert_eq!(long, dec!(-10));
    }

    #[test]
    fn minimum_leverage_is_the_lowest_that_fits_the_balance() {
        // 1000 notional: 1x needs 1000, 2x needs 500, 5x needs 200.
        assert_eq!(
            minimum_leverage(dec!(1000), dec!(4), dec!(0.5), dec!(1000.5), 20),
            Some(1)
        );
        assert_eq!(
            minimum_leverage(dec!(1000), dec!(4), dec!(0.5), dec!(600), 20),
            Some(2)
        );
        assert_eq!(
            minimum_leverage(dec!(1000), dec!(4), dec!(0.5), dec!(250), 20),
            Some(5)
        );
    }

    #[test]
    fn minimum_leverage_is_none_when_nothing_fits() {
        assert_eq!(
            minimum_leverage(dec!(1000), dec!(4), dec!(0.5), dec!(40), 20),
            None
        );
        assert_eq!(
            minimum_leverage(dec!(1000), dec!(4), dec!(0.5), Decimal::ZERO, 20),
            None
        );
        assert_eq!(
            minimum_leverage(dec!(1000), dec!(4), dec!(0.5), dec!(5000), 0),
            None
        );
    }

    #[test]
    fn a_long_is_liquidated_below_its_entry_and_a_short_above() {
        // 1 unit at 100 with 10x leverage: 10 initial margin, 0.4 maintenance.
        let long = liquidation_price(Side::Buy, dec!(100), dec!(1), dec!(10), dec!(0.4));
        let short = liquidation_price(Side::Sell, dec!(100), dec!(1), dec!(10), dec!(0.4));

        assert_eq!(long, dec!(90.4));
        assert_eq!(short, dec!(109.6));
    }

    #[test]
    fn the_bankruptcy_price_has_lost_the_whole_margin() {
        let long = bankruptcy_price(Side::Buy, dec!(100), dec!(1), dec!(10));
        let short = bankruptcy_price(Side::Sell, dec!(100), dec!(1), dec!(10));

        assert_eq!(long, dec!(90));
        assert_eq!(short, dec!(110));
    }

    #[test]
    fn liquidation_is_reached_at_or_beyond_the_price() {
        assert!(is_liquidated(Side::Buy, dec!(90.4), dec!(90.4)));
        assert!(is_liquidated(Side::Buy, dec!(80), dec!(90.4)));
        assert!(!is_liquidated(Side::Buy, dec!(90.5), dec!(90.4)));
        assert!(is_liquidated(Side::Sell, dec!(109.6), dec!(109.6)));
        assert!(!is_liquidated(Side::Sell, dec!(109.5), dec!(109.6)));
    }
}
