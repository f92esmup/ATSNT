//! Monthly drawdown guard shared by the simulator and the live session loop.

use crate::Side;
use rust_decimal::Decimal;

/// Wallet balance plus the open position's unrealized PnL at `mark_price`.
///
/// `position` is `(entry side, entry price, quantity)`; `None` means flat. The exchange wallet
/// balance only moves on realization, so a guard fed with it alone reacts late to open losses.
pub fn marked_equity(
    wallet_balance: Decimal,
    position: Option<(Side, Decimal, Decimal)>,
    mark_price: Decimal,
) -> Decimal {
    match position {
        Some((Side::Buy, entry, quantity)) => wallet_balance + (mark_price - entry) * quantity,
        Some((Side::Sell, entry, quantity)) => wallet_balance + (entry - mark_price) * quantity,
        None => wallet_balance,
    }
}

/// Converts a Unix-millisecond timestamp into a `YYYYMM` UTC month key.
pub fn utc_month_key(timestamp_ms: i64) -> i64 {
    // Howard Hinnant's civil-from-days algorithm (proleptic Gregorian calendar).
    let z = timestamp_ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    year * 100 + month
}

/// Tracks equity against the start-of-month reference and latches once the limit is hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyGuard {
    limit_pct: Decimal,
    month_key: Option<i64>,
    open_equity: Decimal,
    halted: bool,
}

impl MonthlyGuard {
    /// Creates a guard that trips when equity loses `limit_pct` versus the month's first observation.
    pub fn new(limit_pct: Decimal) -> Self {
        Self {
            limit_pct,
            month_key: None,
            open_equity: Decimal::ZERO,
            halted: false,
        }
    }

    /// Whether the guard has tripped; it never clears on its own.
    #[inline]
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Feeds one equity observation; returns the drawdown on the observation that trips the guard.
    pub fn observe(&mut self, timestamp_ms: i64, equity: Decimal) -> Option<Decimal> {
        if self.halted {
            return None;
        }
        let key = utc_month_key(timestamp_ms);
        if self.month_key != Some(key) {
            self.month_key = Some(key);
            self.open_equity = equity;
        }
        if self.open_equity <= Decimal::ZERO {
            return None;
        }
        let drawdown = (self.open_equity - equity).max(Decimal::ZERO) / self.open_equity;
        if drawdown >= self.limit_pct {
            self.halted = true;
            return Some(drawdown);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const JAN_1: i64 = 1_704_067_200_000;
    const DAY: i64 = 86_400_000;

    #[test]
    fn marked_equity_adds_unrealized_pnl_for_long_and_short() {
        assert_eq!(marked_equity(dec!(1000), None, dec!(100)), dec!(1000));
        assert_eq!(
            marked_equity(dec!(1000), Some((Side::Buy, dec!(100), dec!(2))), dec!(90)),
            dec!(980)
        );
        assert_eq!(
            marked_equity(dec!(1000), Some((Side::Sell, dec!(100), dec!(2))), dec!(90)),
            dec!(1020)
        );
    }

    #[test]
    fn unrealized_loss_alone_can_trip_the_guard() {
        let mut guard = MonthlyGuard::new(dec!(0.15));
        let wallet = dec!(1000);
        let long = Some((Side::Buy, dec!(100), dec!(10)));

        assert_eq!(
            guard.observe(JAN_1, marked_equity(wallet, long, dec!(100))),
            None
        );
        // Wallet is unchanged, but the open long is down 150 -> -15% marked.
        assert_eq!(
            guard.observe(JAN_1 + DAY, marked_equity(wallet, long, dec!(85))),
            Some(dec!(0.15))
        );
    }

    #[test]
    fn trips_once_when_loss_versus_month_open_reaches_the_limit() {
        let mut guard = MonthlyGuard::new(dec!(0.15));

        assert_eq!(guard.observe(JAN_1, dec!(1000)), None);
        assert_eq!(guard.observe(JAN_1 + DAY, dec!(1200)), None);
        // -15% vs the 1000 month open (the 1200 peak is irrelevant).
        assert_eq!(guard.observe(JAN_1 + 2 * DAY, dec!(850)), Some(dec!(0.15)));
        assert!(guard.is_halted());
        assert_eq!(guard.observe(JAN_1 + 3 * DAY, dec!(500)), None);
    }

    #[test]
    fn new_month_rebases_the_reference() {
        let mut guard = MonthlyGuard::new(dec!(0.15));

        assert_eq!(guard.observe(JAN_1, dec!(1000)), None);
        assert_eq!(guard.observe(JAN_1 + 20 * DAY, dec!(900)), None);
        assert_eq!(guard.observe(JAN_1 + 31 * DAY, dec!(900)), None);
        // -11% vs the February open of 900 would still be inside the limit.
        assert_eq!(guard.observe(JAN_1 + 40 * DAY, dec!(800)), None);
        assert!(!guard.is_halted());
    }
}
