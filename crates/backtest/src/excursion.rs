//! Mark-to-market equity path of the open position, shared by live and simulated sessions.

use domain::Side;
use rust_decimal::Decimal;

use crate::metrics::TradeExcursion;

/// Tracks the mark-to-market equity path of the open position at bar closes,
/// mirroring the backtest engine's per-trade excursion semantics.
#[derive(Debug, Clone)]
pub struct ExcursionTracker {
    session_peak: Decimal,
    open: Option<TradeExcursion>,
}

impl ExcursionTracker {
    pub fn new(initial_equity: Decimal) -> Self {
        Self {
            session_peak: initial_equity,
            open: None,
        }
    }

    /// Folds a wallet balance update into the session peak.
    pub fn observe_cash(&mut self, equity: Decimal) {
        self.session_peak = self.session_peak.max(equity);
    }

    /// Starts tracking a newly filled position.
    pub fn open(&mut self, time: i64, equity: Decimal) {
        self.observe_cash(equity);
        self.open = Some(TradeExcursion::start(time, equity));
    }

    /// Entry equity of the open position, if any.
    pub fn entry_equity(&self) -> Option<Decimal> {
        self.open.as_ref().map(|e| e.entry_equity)
    }

    /// Marks the open position at `mark_price`; ignored when flat.
    pub fn mark(
        &mut self,
        time: i64,
        cash: Decimal,
        side: Side,
        entry_price: Decimal,
        quantity: Decimal,
        mark_price: Decimal,
    ) {
        let unrealized = match side {
            Side::Buy => (mark_price - entry_price) * quantity,
            Side::Sell => (entry_price - mark_price) * quantity,
        };
        self.observe(time, cash + unrealized);
    }

    /// Closes the open position at `exit_equity`, returning its excursion.
    pub fn close(&mut self, time: i64, exit_equity: Decimal) -> Option<TradeExcursion> {
        self.open.as_ref()?;
        self.observe(time, exit_equity);
        let mut excursion = self.open.take()?;
        excursion.exit_equity = exit_equity;
        Some(excursion)
    }

    fn observe(&mut self, time: i64, equity: Decimal) {
        let Some(excursion) = self.open.as_mut() else {
            return;
        };
        excursion.observe_equity(time, equity);
        self.session_peak = self.session_peak.max(equity);
        let dd_amount = (self.session_peak - equity).max(Decimal::ZERO);
        let dd_pct = if self.session_peak > Decimal::ZERO {
            dd_amount / self.session_peak
        } else {
            Decimal::ZERO
        };
        excursion.observe_drawdown(dd_amount, dd_pct);
    }
}

#[cfg(test)]
mod tests {
    use super::ExcursionTracker;
    use domain::Side;
    use rust_decimal_macros::dec;

    #[test]
    fn tracker_records_mae_mfe_and_drawdown_from_bar_marks() {
        let mut tracker = ExcursionTracker::new(dec!(1000));
        tracker.open(1, dec!(1000));
        // Long 2 units from 100: marks at 95 (-10) and 110 (+20).
        tracker.mark(2, dec!(1000), Side::Buy, dec!(100), dec!(2), dec!(95));
        tracker.mark(3, dec!(1000), Side::Buy, dec!(100), dec!(2), dec!(110));
        let excursion = tracker.close(4, dec!(1015)).expect("open trade");

        assert_eq!(excursion.entry_equity, dec!(1000));
        assert_eq!(
            (excursion.trough_equity, excursion.trough_time),
            (dec!(990), 2)
        );
        assert_eq!(
            (excursion.peak_equity, excursion.peak_time),
            (dec!(1020), 3)
        );
        assert_eq!(excursion.exit_equity, dec!(1015));
        // Deepest drawdown: peak 1020 -> exit 1015 = 5 is smaller than 1000 -> 990 = 10.
        assert_eq!(excursion.max_drawdown_amount, dec!(10));
        assert_eq!(excursion.max_drawdown_pct, dec!(0.01));
    }

    #[test]
    fn tracker_marks_short_positions_inversely_and_ignores_flat_marks() {
        let mut tracker = ExcursionTracker::new(dec!(1000));
        tracker.mark(1, dec!(1000), Side::Sell, dec!(100), dec!(1), dec!(90));
        assert!(tracker.close(2, dec!(1000)).is_none(), "no open trade");

        tracker.open(3, dec!(1000));
        tracker.mark(4, dec!(1000), Side::Sell, dec!(100), dec!(1), dec!(90));
        let excursion = tracker.close(5, dec!(1010)).expect("open trade");
        assert_eq!(excursion.peak_equity, dec!(1010));
    }
}
