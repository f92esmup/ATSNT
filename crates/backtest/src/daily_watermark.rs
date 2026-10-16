//! Per-UTC-day equity peak shared by the simulator's semantics and the live session loop.

use rust_decimal::Decimal;

const MS_PER_DAY: i64 = 86_400_000;

/// Highest marked equity seen since 00:00:00 UTC of the current day.
///
/// The daily drawdown breaker measures loss against this peak, so it must reset at the UTC
/// day boundary exactly like [`crate::BacktestEngine`] does. A whole-session peak would keep
/// the live breaker tripped by losses the simulation had already written off.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DailyWatermark {
    day_key: Option<i64>,
    peak: Decimal,
}

impl DailyWatermark {
    /// Creates a watermark with no observations.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one marked-equity observation and returns the peak of its UTC day.
    ///
    /// The first observation of a new day replaces the previous day's peak.
    pub fn observe(&mut self, timestamp_ms: i64, equity: Decimal) -> Decimal {
        let day_key = timestamp_ms.div_euclid(MS_PER_DAY);
        if self.day_key != Some(day_key) {
            self.day_key = Some(day_key);
            self.peak = equity;
        } else if equity > self.peak {
            self.peak = equity;
        }
        self.peak
    }

    /// Peak equity of the current UTC day (zero before the first observation).
    #[inline]
    pub fn peak(&self) -> Decimal {
        self.peak
    }

    /// Loss of `equity` versus the day peak as a fraction, never negative.
    pub fn drawdown_pct(&self, equity: Decimal) -> Decimal {
        if self.peak <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.peak - equity).max(Decimal::ZERO) / self.peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const DAY: i64 = 86_400_000;
    // 2024-01-01T00:00:00Z
    const JAN_1: i64 = 1_704_067_200_000;

    #[test]
    fn peak_tracks_the_highest_equity_within_a_utc_day() {
        let mut watermark = DailyWatermark::new();

        assert_eq!(watermark.observe(JAN_1 + 1_000, dec!(1000)), dec!(1000));
        assert_eq!(watermark.observe(JAN_1 + 2_000, dec!(1100)), dec!(1100));
        assert_eq!(watermark.observe(JAN_1 + 3_000, dec!(1050)), dec!(1100));
        assert_eq!(watermark.peak(), dec!(1100));
    }

    #[test]
    fn peak_resets_to_the_current_equity_at_the_utc_day_boundary() {
        let mut watermark = DailyWatermark::new();
        watermark.observe(JAN_1 + DAY - 1, dec!(1200));

        // 00:00:00.000 UTC of the next day: the previous day's peak must not carry over.
        assert_eq!(watermark.observe(JAN_1 + DAY, dec!(900)), dec!(900));
        assert_eq!(watermark.observe(JAN_1 + DAY + 1, dec!(950)), dec!(950));
    }

    #[test]
    fn drawdown_is_measured_against_the_day_peak() {
        let mut watermark = DailyWatermark::new();
        watermark.observe(JAN_1, dec!(1000));
        watermark.observe(JAN_1 + 1, dec!(1250));

        assert_eq!(watermark.drawdown_pct(dec!(1000)), dec!(0.2));
        assert_eq!(watermark.drawdown_pct(dec!(1300)), Decimal::ZERO);
    }
}
