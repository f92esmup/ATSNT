use std::ops::Range;

use crate::error::BacktestError;

/// A discrete Walk-Forward validation fold consisting of In-Sample (Train),
/// an Embargo buffer (anti-leakage), and Out-of-Sample (Test) slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkForwardFold {
    /// Zero-based index of this fold.
    pub fold_idx: usize,
    /// In-Sample slice indices [start..end).
    pub train_range: Range<usize>,
    /// Embargo slice indices [start..end). Discarded to prevent serial leakage.
    pub embargo_range: Range<usize>,
    /// Out-of-Sample slice indices [start..end).
    pub test_range: Range<usize>,
}

impl WalkForwardFold {
    /// Extracts the In-Sample (Train) slice from a collection.
    #[inline]
    pub fn train_slice<'a, T>(&self, data: &'a [T]) -> &'a [T] {
        &data[self.train_range.clone()]
    }

    /// Extracts the Out-of-Sample (Test) slice from a collection.
    #[inline]
    pub fn test_slice<'a, T>(&self, data: &'a [T]) -> &'a [T] {
        &data[self.test_range.clone()]
    }

    /// Total number of In-Sample bars.
    #[inline]
    pub fn train_len(&self) -> usize {
        self.train_range.len()
    }

    /// Total number of Out-of-Sample bars.
    #[inline]
    pub fn test_len(&self) -> usize {
        self.test_range.len()
    }

    /// Total number of discarded Embargo bars.
    #[inline]
    pub fn embargo_len(&self) -> usize {
        self.embargo_range.len()
    }
}

/// Configuration parameters for temporal Walk-Forward splitting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkForwardConfig {
    /// Total number of sequential folds to generate.
    pub num_folds: usize,
    /// Ratio of each window dedicated to In-Sample training (e.g. 0.70 for 70%).
    pub train_ratio: f64,
    /// Number of bars quarantined between train and test to prevent indicator lookahead leakage.
    pub embargo_bars: usize,
}

impl WalkForwardConfig {
    /// Validates the configuration parameters.
    pub fn validate(&self) -> Result<(), BacktestError> {
        if self.num_folds == 0 {
            return Err(BacktestError::InvalidFoldConfig(
                "num_folds must be greater than zero".to_string(),
            ));
        }
        if self.train_ratio <= 0.0 || self.train_ratio >= 1.0 {
            return Err(BacktestError::InvalidFoldConfig(format!(
                "train_ratio must be between 0.0 and 1.0, got {}",
                self.train_ratio
            )));
        }
        Ok(())
    }
}

/// Generates purged and embargoed Walk-Forward temporal splits (AFML Chapter 7).
pub struct WalkForwardSplitter;

impl WalkForwardSplitter {
    /// Generates rolling Walk-Forward folds across a discrete dataset length.
    ///
    /// The algorithm splits the available time series into overlapping rolling windows,
    /// ensuring each test segment is strictly Out-of-Sample and preceded by an embargo buffer:
    ///
    /// ```text
    /// Fold 0: [  Train  ]---[Embargo]---►[  Test  ]
    /// Fold 1:       [  Train  ]---[Embargo]---►[  Test  ]
    /// Fold 2:             [  Train  ]---[Embargo]---►[  Test  ]
    /// ```
    pub fn generate_rolling_folds(
        total_bars: usize,
        config: &WalkForwardConfig,
    ) -> Result<Vec<WalkForwardFold>, BacktestError> {
        config.validate()?;

        // Minimum required length to construct a single valid fold
        let min_required = config.embargo_bars + 10;
        if total_bars < min_required {
            return Err(BacktestError::InsufficientData {
                needed: min_required,
                found: total_bars,
            });
        }

        // Mathematical relationship between step size S (test_len), train_len, and total_bars:
        // train_len = S * (train_ratio / (1 - train_ratio))
        // total_bars - embargo >= S * (ratio_factor + num_folds)
        let ratio_factor = config.train_ratio / (1.0 - config.train_ratio);
        let denominator = ratio_factor + (config.num_folds as f64);

        if total_bars <= config.embargo_bars {
            return Err(BacktestError::InsufficientData {
                needed: config.embargo_bars + 10,
                found: total_bars,
            });
        }

        let available = (total_bars - config.embargo_bars) as f64;
        let step_size = (available / denominator).floor() as usize;

        if step_size == 0 {
            return Err(BacktestError::InsufficientData {
                needed: ((denominator * 2.0).ceil() as usize) + config.embargo_bars,
                found: total_bars,
            });
        }

        let test_len = step_size;
        let train_len = ((test_len as f64) * ratio_factor).round() as usize;

        if train_len == 0 {
            return Err(BacktestError::InvalidFoldConfig(
                "train length computed to 0; check train_ratio".to_string(),
            ));
        }

        let mut folds = Vec::with_capacity(config.num_folds);

        for fold_idx in 0..config.num_folds {
            let start = fold_idx * step_size;
            let train_end = start + train_len;
            let embargo_end = train_end + config.embargo_bars;
            let test_end = embargo_end + test_len;

            if test_end > total_bars {
                break;
            }

            folds.push(WalkForwardFold {
                fold_idx,
                train_range: start..train_end,
                embargo_range: train_end..embargo_end,
                test_range: embargo_end..test_end,
            });
        }

        if folds.is_empty() {
            return Err(BacktestError::InvalidFoldConfig(
                "no valid folds could be formed with the specified configuration".to_string(),
            ));
        }

        Ok(folds)
    }

    /// Generates anchored (expanding window) Walk-Forward folds.
    ///
    /// In anchored mode, In-Sample training always starts from index 0 and grows,
    /// modeling a strategy that incorporates all historical data up to the embargo point.
    pub fn generate_anchored_folds(
        total_bars: usize,
        config: &WalkForwardConfig,
    ) -> Result<Vec<WalkForwardFold>, BacktestError> {
        config.validate()?;

        let min_required = config.embargo_bars + (config.num_folds * 10);
        if total_bars < min_required {
            return Err(BacktestError::InsufficientData {
                needed: min_required,
                found: total_bars,
            });
        }

        let test_step = (total_bars.saturating_sub(config.embargo_bars)) / (config.num_folds + 2);
        if test_step == 0 {
            return Err(BacktestError::InvalidFoldConfig(
                "test step computed to 0".to_string(),
            ));
        }

        let mut folds = Vec::with_capacity(config.num_folds);
        let initial_train_len = test_step * 2;

        for fold_idx in 0..config.num_folds {
            let train_end = initial_train_len + (fold_idx * test_step);
            let embargo_end = train_end + config.embargo_bars;
            let test_end = embargo_end + test_step;

            if test_end > total_bars {
                break;
            }

            folds.push(WalkForwardFold {
                fold_idx,
                train_range: 0..train_end,
                embargo_range: train_end..embargo_end,
                test_range: embargo_end..test_end,
            });
        }

        if folds.is_empty() {
            return Err(BacktestError::InvalidFoldConfig(
                "no valid anchored folds formed".to_string(),
            ));
        }

        Ok(folds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_folds_non_overlapping_and_embargoed() {
        let total_bars = 1000;
        let config = WalkForwardConfig {
            num_folds: 4,
            train_ratio: 0.70,
            embargo_bars: 20,
        };

        let folds = WalkForwardSplitter::generate_rolling_folds(total_bars, &config).unwrap();
        assert!(!folds.is_empty());

        for fold in &folds {
            // Train precedes embargo
            assert_eq!(fold.train_range.end, fold.embargo_range.start);
            // Embargo precedes test
            assert_eq!(fold.embargo_range.end, fold.test_range.start);
            // Embargo length matches config
            assert_eq!(fold.embargo_len(), 20);
            // All ranges within total_bars
            assert!(fold.test_range.end <= total_bars);
            // Train and test have zero intersection
            assert!(fold.train_range.end <= fold.test_range.start);
        }
    }

    #[test]
    fn anchored_folds_expand_training_window() {
        let total_bars = 2000;
        let config = WalkForwardConfig {
            num_folds: 3,
            train_ratio: 0.75,
            embargo_bars: 30,
        };

        let folds = WalkForwardSplitter::generate_anchored_folds(total_bars, &config).unwrap();
        assert_eq!(folds.len(), 3);

        let mut prev_train_len = 0;
        for fold in &folds {
            // Training always starts at 0
            assert_eq!(fold.train_range.start, 0);
            // Training window grows with each fold
            assert!(fold.train_len() > prev_train_len);
            prev_train_len = fold.train_len();

            // Embargo correctly positioned
            assert_eq!(fold.embargo_len(), 30);
            assert_eq!(fold.train_range.end, fold.embargo_range.start);
            assert_eq!(fold.embargo_range.end, fold.test_range.start);
        }
    }

    #[test]
    fn fold_slice_helpers() {
        let data: Vec<usize> = (0..500).collect();
        let fold = WalkForwardFold {
            fold_idx: 0,
            train_range: 0..100,
            embargo_range: 100..120,
            test_range: 120..150,
        };

        let train = fold.train_slice(&data);
        assert_eq!(train.len(), 100);
        assert_eq!(train[0], 0);
        assert_eq!(train[99], 99);

        let test = fold.test_slice(&data);
        assert_eq!(test.len(), 30);
        assert_eq!(test[0], 120);
        assert_eq!(test[29], 149);
    }

    #[test]
    fn reject_invalid_config() {
        let config = WalkForwardConfig {
            num_folds: 0,
            train_ratio: 0.7,
            embargo_bars: 10,
        };
        assert!(config.validate().is_err());

        let config_ratio = WalkForwardConfig {
            num_folds: 3,
            train_ratio: 1.5,
            embargo_bars: 10,
        };
        assert!(config_ratio.validate().is_err());
    }
}
