use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use serde::{Deserialize, Serialize};

use crate::error::DomainError;

/// Directional structural shift event emitted by the symmetric CUSUM filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CusumEvent {
    /// Positive upward regime shift: S+ reached threshold h.
    PositiveShift {
        /// Accumulated positive return at trigger time.
        return_accumulated: Decimal,
    },
    /// Negative downward regime shift: S- reached threshold -h.
    NegativeShift {
        /// Accumulated negative return at trigger time.
        return_accumulated: Decimal,
    },
}

/// Symmetric Cumulative Sum (CUSUM) filter for change-point detection
/// as described in Marcos López de Prado's *Advances in Financial Machine Learning* (Chapter 2.5).
///
/// Filters Gaussian noise and emits discrete events only when cumulative log returns
/// breach a volatility threshold $h$, resetting memory upon trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CusumFilter {
    s_pos: Decimal,
    s_neg: Decimal,
    last_price: Option<Decimal>,
}

impl CusumFilter {
    /// Initializes an idle CUSUM filter with zeroed accumulators.
    pub fn new() -> Self {
        Self {
            s_pos: Decimal::ZERO,
            s_neg: Decimal::ZERO,
            last_price: None,
        }
    }

    /// Current value of the positive accumulator S+.
    #[inline]
    pub fn s_pos(&self) -> Decimal {
        self.s_pos
    }

    /// Current value of the negative accumulator S-.
    #[inline]
    pub fn s_neg(&self) -> Decimal {
        self.s_neg
    }

    /// Last ingested reference price.
    #[inline]
    pub fn last_price(&self) -> Option<Decimal> {
        self.last_price
    }

    /// Resets accumulators to zero while retaining last ingested price.
    pub fn reset_accumulators(&mut self) {
        self.s_pos = Decimal::ZERO;
        self.s_neg = Decimal::ZERO;
    }

    /// Processes a new price point against a dynamic threshold $h$.
    ///
    /// The threshold `h` must be strictly positive (typically calibrated as `mult * rolling_std_dev`).
    /// Returns `Some(CusumEvent)` if a structural shift occurs, resetting that accumulator.
    pub fn update(
        &mut self,
        price: Decimal,
        threshold: Decimal,
    ) -> Result<Option<CusumEvent>, DomainError> {
        if price <= Decimal::ZERO {
            return Err(DomainError::InvalidPrice(price.to_string()));
        }
        if threshold <= Decimal::ZERO {
            return Err(DomainError::InvalidThreshold(threshold.to_string()));
        }

        let prev_price = match self.last_price {
            Some(prev) => prev,
            None => {
                // First observation establishes baseline price
                self.last_price = Some(price);
                return Ok(None);
            }
        };

        // Log return: r_t = ln(P_t / P_{t-1})
        let price_ratio = price / prev_price;
        let log_return = price_ratio.ln();

        // Recursive accumulators with zero-bounding
        // S+ = max(0, S+ + r_t)
        self.s_pos = (self.s_pos + log_return).max(Decimal::ZERO);

        // S- = min(0, S- + r_t)
        self.s_neg = (self.s_neg + log_return).min(Decimal::ZERO);

        self.last_price = Some(price);

        // Check breach of threshold
        if self.s_pos >= threshold {
            let event = CusumEvent::PositiveShift {
                return_accumulated: self.s_pos,
            };
            self.s_pos = Decimal::ZERO;
            Ok(Some(event))
        } else if self.s_neg <= -threshold {
            let event = CusumEvent::NegativeShift {
                return_accumulated: self.s_neg,
            };
            self.s_neg = Decimal::ZERO;
            Ok(Some(event))
        } else {
            Ok(None)
        }
    }
}

impl Default for CusumFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn invalid_price_or_threshold_rejected() {
        let mut filter = CusumFilter::new();
        assert!(filter.update(dec!(0), dec!(0.05)).is_err());
        assert!(filter.update(dec!(-10), dec!(0.05)).is_err());
        assert!(filter.update(dec!(100), dec!(0)).is_err());
        assert!(filter.update(dec!(100), dec!(-0.01)).is_err());
    }

    #[test]
    fn first_price_initializes_baseline() {
        let mut filter = CusumFilter::new();
        let res = filter.update(dec!(100), dec!(0.05)).unwrap();
        assert_eq!(res, None);
        assert_eq!(filter.last_price(), Some(dec!(100)));
        assert_eq!(filter.s_pos(), dec!(0));
        assert_eq!(filter.s_neg(), dec!(0));
    }

    #[test]
    fn small_noise_is_absorbed_at_zero() {
        let mut filter = CusumFilter::new();
        let h = dec!(0.05); // 5% threshold

        filter.update(dec!(100), h).unwrap();

        // Up small
        filter.update(dec!(101), h).unwrap();
        assert!(filter.s_pos() > dec!(0));
        assert_eq!(filter.s_neg(), dec!(0));

        // Down washes out positive accumulation back to zero floor
        filter.update(dec!(98), h).unwrap();
        assert_eq!(filter.s_pos(), dec!(0));
        assert!(filter.s_neg() < dec!(0));

        // Up again washes out negative accumulation back to zero ceiling
        filter.update(dec!(102), h).unwrap();
        assert_eq!(filter.s_neg(), dec!(0));
        assert!(filter.s_pos() > dec!(0));
    }

    #[test]
    fn upward_trend_triggers_positive_event_and_resets() {
        let mut filter = CusumFilter::new();
        let h = dec!(0.04); // 4% threshold

        filter.update(dec!(100), h).unwrap();
        assert!(filter.update(dec!(102), h).unwrap().is_none()); // ~1.98%
        assert!(filter.update(dec!(103), h).unwrap().is_none()); // ~0.97% -> total ~2.95%

        // Jump to 105: total return from 100 is ln(1.05) ~ 4.879% >= 4%
        let res = filter.update(dec!(105), h).unwrap();
        assert!(res.is_some());

        match res.unwrap() {
            CusumEvent::PositiveShift { return_accumulated } => {
                assert!(return_accumulated >= h);
            }
            CusumEvent::NegativeShift { .. } => panic!("Expected PositiveShift"),
        }

        // Memory reset check
        assert_eq!(filter.s_pos(), dec!(0));
    }

    #[test]
    fn downward_trend_triggers_negative_event_and_resets() {
        let mut filter = CusumFilter::new();
        let h = dec!(0.04); // 4% threshold

        filter.update(dec!(100), h).unwrap();
        assert!(filter.update(dec!(98), h).unwrap().is_none());
        assert!(filter.update(dec!(97), h).unwrap().is_none());

        // Jump down to 95: ln(95/100) ~ -5.129% <= -4%
        let res = filter.update(dec!(95), h).unwrap();
        assert!(res.is_some());

        match res.unwrap() {
            CusumEvent::NegativeShift { return_accumulated } => {
                assert!(return_accumulated <= -h);
            }
            CusumEvent::PositiveShift { .. } => panic!("Expected NegativeShift"),
        }

        // Memory reset check
        assert_eq!(filter.s_neg(), dec!(0));
    }
}
