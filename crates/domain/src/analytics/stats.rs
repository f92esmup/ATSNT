use std::collections::VecDeque;

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use serde::{Deserialize, Serialize};

use crate::error::DomainError;

/// Output snapshot of rolling statistical metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZScoreResult {
    /// Rolling arithmetic mean (mu).
    pub mean: Decimal,
    /// Rolling standard deviation (sigma).
    pub std_dev: Decimal,
    /// Standardized Z-Score: (value - mean) / std_dev.
    pub z_score: Decimal,
}

/// Rolling statistical window calculating Mean, Standard Deviation, and Z-Score
/// using deterministic fixed-point arithmetic (`rust_decimal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollingZScore {
    window_size: usize,
    window: VecDeque<Decimal>,
    running_sum: Decimal,
}

impl RollingZScore {
    /// Creates a new rolling window with the designated size (must be >= 2).
    pub fn new(window_size: usize) -> Result<Self, DomainError> {
        if window_size < 2 {
            return Err(DomainError::InvalidWindowSize(window_size));
        }
        Ok(Self {
            window_size,
            window: VecDeque::with_capacity(window_size),
            running_sum: Decimal::ZERO,
        })
    }

    /// Configured window lookback capacity.
    #[inline]
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// Number of elements currently present in the window.
    #[inline]
    pub fn len(&self) -> usize {
        self.window.len()
    }

    /// Returns true if no elements have been ingested into the window yet.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    /// True if the window is fully warmed up and ready to produce statistics.
    #[inline]
    pub fn is_ready(&self) -> bool {
        self.window.len() == self.window_size
    }

    /// Ingests a new value into the rolling window.
    ///
    /// Returns `Some(ZScoreResult)` if the window is fully saturated (length == window_size),
    /// or `None` during the initial warm-up period.
    pub fn update(&mut self, value: Decimal) -> Option<ZScoreResult> {
        if self.window.len() == self.window_size {
            if let Some(evicted) = self.window.pop_front() {
                self.running_sum -= evicted;
            }
        }

        self.window.push_back(value);
        self.running_sum += value;

        if self.window.len() < self.window_size {
            return None;
        }

        let n = Decimal::from(self.window_size);
        let mean = self.running_sum / n;

        // Sum of squared deviations: sum((x - mean)^2)
        let mut sum_squared_diff = Decimal::ZERO;
        for &item in &self.window {
            let diff = item - mean;
            sum_squared_diff += diff * diff;
        }

        let variance = sum_squared_diff / n;
        let std_dev = variance.sqrt().unwrap_or(Decimal::ZERO);

        let z_score = if std_dev == Decimal::ZERO {
            Decimal::ZERO
        } else {
            (value - mean) / std_dev
        };

        Some(ZScoreResult {
            mean,
            std_dev,
            z_score,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn invalid_window_size_rejected() {
        assert_eq!(
            RollingZScore::new(0),
            Err(DomainError::InvalidWindowSize(0))
        );
        assert_eq!(
            RollingZScore::new(1),
            Err(DomainError::InvalidWindowSize(1))
        );
        assert!(RollingZScore::new(2).is_ok());
    }

    #[test]
    fn warm_up_period_returns_none() {
        let mut r = RollingZScore::new(3).unwrap();
        assert_eq!(r.update(dec!(10)), None);
        assert_eq!(r.update(dec!(20)), None);
        assert!(!r.is_ready());

        let res = r.update(dec!(30));
        assert!(res.is_some());
        assert!(r.is_ready());
    }

    #[test]
    fn calculate_exact_z_score_and_mean() {
        // Window size 4: [10, 20, 30, 40]
        // Mean = 25
        // Devs: -15, -5, 5, 15
        // Squared devs: 225, 25, 25, 225 -> sum = 500
        // Variance = 500 / 4 = 125
        // StdDev = sqrt(125) approx 11.180339887498948482045868344
        // Z-Score for 40: (40 - 25) / 11.180339887498948482... approx 1.3416407864998738178
        let mut r = RollingZScore::new(4).unwrap();
        r.update(dec!(10));
        r.update(dec!(20));
        r.update(dec!(30));
        let res = r.update(dec!(40)).unwrap();

        assert_eq!(res.mean, dec!(25));
        let expected_std = dec!(125).sqrt().unwrap();
        assert_eq!(res.std_dev, expected_std);

        let expected_z = dec!(15) / expected_std;
        assert_eq!(res.z_score, expected_z);
    }

    #[test]
    fn zero_standard_deviation_handled_gracefully() {
        // Flat price line: all values are 100
        let mut r = RollingZScore::new(3).unwrap();
        r.update(dec!(100));
        r.update(dec!(100));
        let res = r.update(dec!(100)).unwrap();

        assert_eq!(res.mean, dec!(100));
        assert_eq!(res.std_dev, dec!(0));
        assert_eq!(res.z_score, dec!(0));
    }

    #[test]
    fn rolling_eviction_updates_statistics() {
        let mut r = RollingZScore::new(2).unwrap();
        // [10, 20] -> mean 15
        r.update(dec!(10));
        let res1 = r.update(dec!(20)).unwrap();
        assert_eq!(res1.mean, dec!(15));

        // [20, 30] -> mean 25 (10 was evicted)
        let res2 = r.update(dec!(30)).unwrap();
        assert_eq!(res2.mean, dec!(25));
    }
}
