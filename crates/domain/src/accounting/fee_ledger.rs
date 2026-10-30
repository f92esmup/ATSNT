//! Accumulates the commissions the exchange really charged over a live position's lifetime.

use rust_decimal::Decimal;

/// Quote asset the ledger accounts in; commissions in other assets are priced by estimate.
const QUOTE_ASSET: &str = "USDT";

/// Running total of the commissions reported by the exchange for the current position.
///
/// The simulator charges `taker_fee_pct` on every fill; live must report what Binance charged
/// instead, otherwise fee tiers, BNB discounts and partial fills silently skew the live PnL
/// that is compared against the backtest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeeLedger {
    accrued: Option<Decimal>,
}

impl FeeLedger {
    /// Creates an empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the commission of one fill.
    ///
    /// A commission charged in another asset than USDT cannot be valued here, so that fill is
    /// priced at `fallback_fee_pct` of its notional rather than dropped.
    pub fn record_fill(
        &mut self,
        commission: Decimal,
        commission_asset: Option<&str>,
        fill_notional: Decimal,
        fallback_fee_pct: Decimal,
    ) {
        let charged = match commission_asset {
            Some(asset) if !asset.eq_ignore_ascii_case(QUOTE_ASSET) => {
                fill_notional * fallback_fee_pct
            }
            _ => commission,
        };
        self.accrued = Some(self.accrued.unwrap_or(Decimal::ZERO) + charged);
    }

    /// Returns the fees accrued since the last call and resets the ledger.
    ///
    /// `None` means no fill was seen, so the caller must fall back to an estimate.
    pub fn take(&mut self) -> Option<Decimal> {
        self.accrued.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn usdt_commission_is_recorded_exactly_as_charged() {
        let mut ledger = FeeLedger::new();

        ledger.record_fill(dec!(0.0336), Some("USDT"), dec!(84), dec!(0.0005));
        ledger.record_fill(dec!(0.0210), Some("usdt"), dec!(52), dec!(0.0005));

        assert_eq!(ledger.take(), Some(dec!(0.0546)));
    }

    #[test]
    fn commission_in_another_asset_falls_back_to_the_estimate_for_that_fill() {
        let mut ledger = FeeLedger::new();

        // Paid in BNB: its USDT value is unknown here, so the fill is priced at the taker estimate.
        ledger.record_fill(dec!(0.00004), Some("BNB"), dec!(1000), dec!(0.0005));

        assert_eq!(ledger.take(), Some(dec!(0.5)));
    }

    #[test]
    fn a_fill_with_no_commission_costs_nothing() {
        let mut ledger = FeeLedger::new();

        ledger.record_fill(Decimal::ZERO, None, dec!(1000), dec!(0.0005));

        assert_eq!(ledger.take(), Some(Decimal::ZERO));
    }

    #[test]
    fn take_reports_nothing_when_no_fill_was_seen_and_resets_after_reading() {
        let mut ledger = FeeLedger::new();
        assert_eq!(ledger.take(), None);

        ledger.record_fill(dec!(1), Some("USDT"), dec!(100), dec!(0.0005));
        assert_eq!(ledger.take(), Some(dec!(1)));
        assert_eq!(ledger.take(), None);
    }
}
