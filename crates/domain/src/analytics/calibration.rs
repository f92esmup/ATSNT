//! Calibration of tick, volume, and range bar thresholds.
//!
//! Each threshold is chosen so that the feed's mean bar duration over a
//! calibration period equals a target horizon `Δ` (swarm consensus strategy
//! specification §5.4). With `N` trades, total base volume `V`, and duration
//! `T = last.timestamp − first.timestamp` ms:
//!
//! - tick threshold `= ceil(N · Δ / T)`;
//! - volume threshold `= V · Δ / T`, rounded to 3 decimals;
//! - range threshold: the relative range `r` whose [`RangeBarAggregator`] pass
//!   over the calibration trades yields `round(T / Δ)` bars, found by
//!   bisection on `[0.00001, 0.5]` until the bar count is within 1% of the
//!   target or after 60 iterations, then rounded to 6 decimals.
//!
//! Midpoints round away from zero. All arithmetic is `Decimal`.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::analytics::feeds::{BarAggregator, RangeBarAggregator};
use crate::error::DomainError;
use crate::trade::Trade;

/// Calibrated thresholds for the four horizons of the tick, volume, and range feeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedThresholds {
    /// Trades per tick bar, one per horizon.
    pub tick: [u64; 4],
    /// Base volume per volume bar, one per horizon (3 decimals).
    pub volume: [Decimal; 4],
    /// Relative range `(high - low) / open` per range bar, one per horizon (6 decimals).
    pub range: [Decimal; 4],
    /// Timestamp (ms) of the last calibration trade; evaluated data must start after it.
    pub calibration_end_ms: i64,
}

/// Calibrates the tick, volume, and range thresholds of four horizons (ms) on `trades`.
///
/// `trades` must be in timestamp order.
///
/// # Errors
///
/// - [`DomainError::InvalidThreshold`] when a horizon is not strictly positive.
/// - [`DomainError::InsufficientData`] with fewer than 2 trades, a zero
///   duration, or a volume threshold that rounds to zero.
pub fn calibrate(trades: &[Trade], horizons_ms: [i64; 4]) -> Result<FeedThresholds, DomainError> {
    if let Some(horizon) = horizons_ms.iter().find(|h| **h <= 0) {
        return Err(DomainError::InvalidThreshold(horizon.to_string()));
    }
    let (first, last) = match trades {
        [first, .., last] => (first, last),
        _ => {
            return Err(DomainError::InsufficientData(format!(
                "calibration needs at least 2 trades, got {}",
                trades.len()
            )))
        }
    };
    let duration_ms = last.timestamp - first.timestamp;
    if duration_ms <= 0 {
        return Err(DomainError::InsufficientData(format!(
            "calibration period has a non-positive duration of {duration_ms} ms"
        )));
    }

    let trade_count = Decimal::from(trades.len());
    let total_volume = trades.iter().map(|t| t.quantity).sum::<Decimal>();
    let duration = Decimal::from(duration_ms);

    let mut tick = [0_u64; 4];
    let mut volume = [Decimal::ZERO; 4];
    let mut range = [Decimal::ZERO; 4];
    for (i, horizon_ms) in horizons_ms.iter().enumerate() {
        let horizon = Decimal::from(*horizon_ms);
        let per_horizon = |total: Decimal| {
            total
                .checked_mul(horizon)
                .and_then(|scaled| scaled.checked_div(duration))
                .ok_or_else(|| {
                    DomainError::InvalidThreshold(format!(
                        "horizon {horizon_ms} ms overflows the calibration arithmetic"
                    ))
                })
        };

        tick[i] = per_horizon(trade_count)?
            .ceil()
            .to_u64()
            .ok_or_else(|| DomainError::InvalidThreshold(horizon_ms.to_string()))?;

        volume[i] = per_horizon(total_volume)?
            .round_dp_with_strategy(VOLUME_DP, RoundingStrategy::MidpointAwayFromZero);
        if volume[i] <= Decimal::ZERO {
            return Err(DomainError::InsufficientData(format!(
                "volume threshold for horizon {horizon_ms} ms rounds to zero"
            )));
        }

        let target_bars = (duration / horizon)
            .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
            .to_u64()
            .ok_or_else(|| DomainError::InvalidThreshold(horizon_ms.to_string()))?;
        range[i] = calibrate_range(trades, target_bars)?;
    }

    Ok(FeedThresholds {
        tick,
        volume,
        range,
        calibration_end_ms: last.timestamp,
    })
}

/// Decimals kept by a volume threshold.
const VOLUME_DP: u32 = 3;
/// Decimals kept by a range threshold.
const RANGE_DP: u32 = 6;
/// Maximum number of bisection steps of the range search.
const RANGE_MAX_ITERATIONS: usize = 60;

/// Bisects the relative range on `[0.00001, 0.5]` until a range-bar pass over
/// `trades` yields a bar count within 1% of `target_bars`.
///
/// Each step evaluates the candidate already rounded to 6 decimals, so the
/// returned threshold is the one whose bar count was checked. Bar count is
/// non-increasing in the threshold: too many bars raise the lower bound, too
/// few lower the upper bound.
fn calibrate_range(trades: &[Trade], target_bars: u64) -> Result<Decimal, DomainError> {
    let mut low = Decimal::new(1, 5);
    let mut high = Decimal::new(5, 1);
    let mut candidate = low;
    for _ in 0..RANGE_MAX_ITERATIONS {
        let mid = (low + high) / Decimal::TWO;
        candidate = mid.round_dp_with_strategy(RANGE_DP, RoundingStrategy::MidpointAwayFromZero);
        let bars = count_range_bars(trades, candidate)?;
        if bars.abs_diff(target_bars) * 100 <= target_bars {
            break;
        }
        if bars > target_bars {
            low = mid;
        } else {
            high = mid;
        }
    }
    Ok(candidate)
}

/// Number of range bars closed by one pass over `trades`.
fn count_range_bars(trades: &[Trade], threshold: Decimal) -> Result<u64, DomainError> {
    let mut aggregator = RangeBarAggregator::new(threshold)?;
    let mut bars = 0_u64;
    for trade in trades {
        if aggregator.process_trade(trade).is_some() {
            bars += 1;
        }
    }
    Ok(bars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::Side;
    use rust_decimal_macros::dec;

    const HORIZONS: [i64; 4] = [300_000, 3_600_000, 900_000, 60_000];

    /// `n` trades one second apart, each of quantity 0.5 at price 100.
    fn uniform_trades(n: i64) -> Vec<Trade> {
        (0..n)
            .map(|i| Trade::new(i * 1_000, dec!(100), dec!(0.5), Side::Buy).unwrap())
            .collect()
    }

    /// Seeded random walk built with a 64-bit LCG (no floats, no `rand`).
    ///
    /// Trades are one second apart; each step moves the price by an integer
    /// number of cents in `[-100, 100]` around 50,000.
    fn random_walk(seed: u64, n: i64) -> Vec<Trade> {
        let mut state = seed;
        let mut cents: i64 = 5_000_000;
        (0..n)
            .map(|i| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let step = ((state >> 33) % 201) as i64 - 100;
                let qty = 1 + ((state >> 45) % 100) as i64;
                cents += step;
                let side = if step >= 0 { Side::Buy } else { Side::Sell };
                Trade::new(
                    i * 1_000,
                    Decimal::new(cents, 2),
                    Decimal::new(qty, 3),
                    side,
                )
                .unwrap()
            })
            .collect()
    }

    fn range_bar_count(trades: &[Trade], threshold: Decimal) -> i64 {
        let mut agg = RangeBarAggregator::new(threshold).unwrap();
        trades.iter().filter_map(|t| agg.process_trade(t)).count() as i64
    }

    #[test]
    fn tick_and_volume_thresholds_are_exact_on_uniform_trades() {
        // N = 3601, V = 1800.5, T = 3,600,000 ms.
        let thresholds = calibrate(&uniform_trades(3_601), HORIZONS).unwrap();
        // ceil(3601·Δ/3.6e6): 300.083 → 301, 3601, 900.25 → 901, 60.016 → 61.
        assert_eq!(thresholds.tick, [301, 3_601, 901, 61]);
        // 1800.5·Δ/3.6e6: 150.041666… → 150.042, 1800.5, 450.125, 30.008333… → 30.008.
        assert_eq!(
            thresholds.volume,
            [dec!(150.042), dec!(1800.500), dec!(450.125), dec!(30.008)]
        );
        assert_eq!(thresholds.calibration_end_ms, 3_600_000);
    }

    #[test]
    fn range_threshold_hits_target_count_within_one_percent() {
        let trades = random_walk(42, 20_000);
        let horizons = [20_000, 60_000, 120_000, 300_000];
        let thresholds = calibrate(&trades, horizons).unwrap();
        let duration = 19_999_000_i64;
        for (horizon, range) in horizons.iter().zip(thresholds.range.iter()) {
            assert_eq!(*range, range.round_dp(6), "range threshold rounded to 6 dp");
            let target = (duration + horizon / 2) / horizon;
            let count = range_bar_count(&trades, *range);
            assert!(
                (count - target).abs() * 100 <= target,
                "horizon {horizon}: {count} bars vs target {target} (r = {range})"
            );
        }
    }

    #[test]
    fn calibration_is_deterministic() {
        let trades = random_walk(7, 5_000);
        let horizons = [10_000, 30_000, 60_000, 120_000];
        let first = calibrate(&trades, horizons).unwrap();
        let second = calibrate(&trades, horizons).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn fewer_than_two_trades_is_insufficient_data() {
        assert!(matches!(
            calibrate(&[], HORIZONS),
            Err(DomainError::InsufficientData(_))
        ));
        assert!(matches!(
            calibrate(&uniform_trades(1), HORIZONS),
            Err(DomainError::InsufficientData(_))
        ));
    }

    #[test]
    fn zero_duration_is_insufficient_data() {
        let trades = vec![
            Trade::new(5_000, dec!(100), dec!(1), Side::Buy).unwrap(),
            Trade::new(5_000, dec!(101), dec!(1), Side::Sell).unwrap(),
        ];
        assert!(matches!(
            calibrate(&trades, HORIZONS),
            Err(DomainError::InsufficientData(_))
        ));
    }

    #[test]
    fn non_positive_horizon_is_invalid_threshold() {
        let trades = uniform_trades(10);
        assert_eq!(
            calibrate(&trades, [300_000, 0, 900_000, 60_000]),
            Err(DomainError::InvalidThreshold("0".to_string()))
        );
        assert!(matches!(
            calibrate(&trades, [300_000, 3_600_000, -1, 60_000]),
            Err(DomainError::InvalidThreshold(_))
        ));
    }

    #[test]
    fn feed_thresholds_round_trip_through_json() {
        let thresholds = calibrate(&uniform_trades(3_601), HORIZONS).unwrap();
        let json = serde_json::to_string(&thresholds).unwrap();
        assert_eq!(
            serde_json::from_str::<FeedThresholds>(&json).unwrap(),
            thresholds
        );
    }
}
