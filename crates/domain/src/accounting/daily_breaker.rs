//! The daily drawdown breaker: the day's equity peak and whether the loss from it has reached
//! the limit.

use rust_decimal::Decimal;

use super::daily_watermark::DailyWatermark;

/// Trips when marked equity falls `max_drawdown_pct` below the peak of its UTC day, and stays
/// tripped until the first observation of the next UTC day.
///
/// The latch matters: a position that recovers after the loss does not re-open the day. Every
/// runner feeds the same observations here, so a simulation and a live session stop on the same
/// event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyBreaker {
    watermark: DailyWatermark,
    max_drawdown_pct: Decimal,
    day_key: Option<i64>,
    tripped: bool,
}

const MS_PER_DAY: i64 = 86_400_000;

impl DailyBreaker {
    /// A breaker that trips at a loss of `max_drawdown_pct` (a fraction) from the day's peak.
    pub fn new(max_drawdown_pct: Decimal) -> Self {
        Self {
            watermark: DailyWatermark::new(),
            max_drawdown_pct,
            day_key: None,
            tripped: false,
        }
    }

    /// Feeds one marked-equity observation and returns the peak of its UTC day.
    ///
    /// The first observation of a new day re-arms the breaker and starts a new peak. An
    /// observation at or beyond the limit trips it.
    pub fn observe(&mut self, timestamp_ms: i64, equity: Decimal) -> Decimal {
        let day_key = timestamp_ms.div_euclid(MS_PER_DAY);
        if self.day_key != Some(day_key) {
            self.day_key = Some(day_key);
            self.tripped = false;
        }
        let peak = self.watermark.observe(timestamp_ms, equity);
        if self.watermark.drawdown_pct(equity) >= self.max_drawdown_pct {
            self.tripped = true;
        }
        peak
    }

    /// Whether the day's loss reached the limit.
    #[inline]
    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    /// Peak equity of the current UTC day (zero before the first observation).
    #[inline]
    pub fn peak(&self) -> Decimal {
        self.watermark.peak()
    }

    /// Loss of `equity` versus the day peak as a fraction, never negative.
    pub fn drawdown_pct(&self, equity: Decimal) -> Decimal {
        self.watermark.drawdown_pct(equity)
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
    fn it_trips_at_the_limit_and_stays_tripped_when_equity_recovers() {
        let mut breaker = DailyBreaker::new(dec!(0.05));
        breaker.observe(JAN_1, dec!(1000));
        breaker.observe(JAN_1 + 1, dec!(951));
        assert!(!breaker.is_tripped());

        // Exactly 5 % below the day's peak.
        breaker.observe(JAN_1 + 2, dec!(950));
        assert!(breaker.is_tripped());

        breaker.observe(JAN_1 + 3, dec!(1200));
        assert!(breaker.is_tripped());
        assert_eq!(breaker.peak(), dec!(1200));
    }

    #[test]
    fn the_first_observation_of_a_new_utc_day_re_arms_it() {
        let mut breaker = DailyBreaker::new(dec!(0.05));
        breaker.observe(JAN_1, dec!(1000));
        breaker.observe(JAN_1 + DAY - 1, dec!(900));
        assert!(breaker.is_tripped());

        assert_eq!(breaker.observe(JAN_1 + DAY, dec!(900)), dec!(900));
        assert!(!breaker.is_tripped());
        assert_eq!(breaker.drawdown_pct(dec!(855)), dec!(0.05));
    }
}
