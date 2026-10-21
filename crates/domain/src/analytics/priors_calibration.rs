//! Empirical prior calibration from historical bars and indicators (spec §7.1).
//!
//! Analyzes empirical distributions (quantiles, medians, and candle ratios)
//! over a slice of bars and indicators to derive calibrated prior means for
//! the 11 swarm genome genes.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::feeds::FeedBar;
use super::indicators::{BarInput, FeedIndicators};
use crate::error::DomainError;

/// The empirical means derived from calibrating against historical bars.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmpiricalPriors {
    pub rsi_overbought_mean: Decimal,
    pub rsi_oversold_mean: Decimal,
    pub atr_expansion_mean: Decimal,
    pub atr_contraction_mean: Decimal,
    pub vwap_deviation_mean: Decimal,
    pub alma_slope_mean: Decimal,
    pub pinbar_long_ratio_mean: Decimal,
    pub pinbar_short_ratio_mean: Decimal,
    pub engulfing_ratio_mean: Decimal,
    pub bos_margin_mean: Decimal,
    pub insidebar_ratio_mean: Decimal,
}

impl Default for EmpiricalPriors {
    fn default() -> Self {
        Self {
            rsi_overbought_mean: Decimal::from(70),
            rsi_oversold_mean: Decimal::from(30),
            atr_expansion_mean: Decimal::ONE,
            atr_contraction_mean: Decimal::new(5, 1),
            vwap_deviation_mean: Decimal::ONE,
            alma_slope_mean: Decimal::new(7, 2),
            pinbar_long_ratio_mean: Decimal::new(28, 1),
            pinbar_short_ratio_mean: Decimal::new(3, 1),
            engulfing_ratio_mean: Decimal::new(115, 2),
            bos_margin_mean: Decimal::ZERO,
            insidebar_ratio_mean: Decimal::new(7, 1),
        }
    }
}

/// Helper to get a percentile value from a sorted slice.
fn percentile(sorted: &[Decimal], pct: usize) -> Decimal {
    if sorted.is_empty() {
        return Decimal::ZERO;
    }
    let idx = (sorted.len() * pct) / 100;
    sorted[idx.min(sorted.len() - 1)]
}

/// Helper to calculate the median from a sorted slice.
fn median(sorted: &[Decimal]) -> Decimal {
    percentile(sorted, 50)
}

/// Calibrates empirical priors from a slice of bars.
///
/// Requires at least 64 bars to warm up indicators and accumulate representative samples.
pub fn calibrate_empirical_priors(bars: &[FeedBar]) -> Result<EmpiricalPriors, DomainError> {
    if bars.len() < 64 {
        return Err(DomainError::InsufficientData(format!(
            "prior calibration requires at least 64 bars, got {}",
            bars.len()
        )));
    }

    let mut indicators = FeedIndicators::new()?;
    let mut rsis = Vec::with_capacity(bars.len());
    let mut atr_ratios = Vec::with_capacity(bars.len());
    let mut vwap_devs = Vec::with_capacity(bars.len());
    let mut alma_slopes = Vec::with_capacity(bars.len());

    let mut pinbar_long = Vec::with_capacity(bars.len());
    let mut pinbar_short = Vec::with_capacity(bars.len());
    let mut engulfing = Vec::with_capacity(bars.len());
    let mut insidebar = Vec::with_capacity(bars.len());

    let mut prev_bar: Option<&FeedBar> = None;

    for bar in bars {
        let input = BarInput::from(bar);
        if let Some(snapshot) = indicators.update(&input) {
            rsis.push(snapshot.rsi);
            atr_ratios.push(snapshot.atr_ratio);
            vwap_devs.push(snapshot.vwap_dev);
            alma_slopes.push(snapshot.alma_slope.abs());
        }

        // Candle shape analysis
        let body = (bar.close - bar.open).abs();
        let upper_wick = bar.high - bar.open.max(bar.close);
        let lower_wick = bar.open.min(bar.close) - bar.low;

        if body > Decimal::ZERO {
            let rejection_wick = lower_wick.max(upper_wick);
            let opposite_wick = lower_wick.min(upper_wick);
            pinbar_long.push(rejection_wick / body);
            pinbar_short.push(opposite_wick / body);
        }

        if let Some(prev) = prev_bar {
            let prev_body = (prev.close - prev.open).abs();
            if prev_body > Decimal::ZERO && body > prev_body {
                engulfing.push(body / prev_body);
            }

            let prev_range = prev.high - prev.low;
            let curr_range = bar.high - bar.low;
            if prev_range > Decimal::ZERO && curr_range < prev_range {
                insidebar.push(curr_range / prev_range);
            }
        }

        prev_bar = Some(bar);
    }

    rsis.sort();
    atr_ratios.sort();
    vwap_devs.sort();
    alma_slopes.sort();
    pinbar_long.sort();
    pinbar_short.sort();
    engulfing.sort();
    insidebar.sort();

    let defaults = EmpiricalPriors::default();

    // RSI: 90th percentile for overbought, 10th for oversold
    let rsi_overbought = if !rsis.is_empty() {
        percentile(&rsis, 90).clamp(Decimal::from(60), Decimal::from(80))
    } else {
        defaults.rsi_overbought_mean
    };

    let rsi_oversold = if !rsis.is_empty() {
        percentile(&rsis, 10).clamp(Decimal::from(20), Decimal::from(40))
    } else {
        defaults.rsi_oversold_mean
    };

    // ATR Ratio: 75th percentile for expansion, 25th for contraction
    let atr_expansion = if !atr_ratios.is_empty() {
        percentile(&atr_ratios, 75).clamp(Decimal::new(8, 1), Decimal::new(25, 1))
    } else {
        defaults.atr_expansion_mean
    };

    let atr_contraction = if !atr_ratios.is_empty() {
        let raw = percentile(&atr_ratios, 25).clamp(Decimal::new(2, 1), Decimal::new(7, 1));
        raw.min(atr_expansion - Decimal::new(1, 1))
    } else {
        defaults.atr_contraction_mean
    };

    // VWAP Deviation: 75th percentile
    let vwap_deviation = if !vwap_devs.is_empty() {
        percentile(&vwap_devs, 75).clamp(Decimal::new(5, 1), Decimal::from(3))
    } else {
        defaults.vwap_deviation_mean
    };

    // ALMA Slope: median absolute slope
    let alma_slope = if !alma_slopes.is_empty() {
        median(&alma_slopes).clamp(Decimal::new(1, 2), Decimal::new(3, 1))
    } else {
        defaults.alma_slope_mean
    };

    // Pin bar: median rejection and opposite ratios
    let pinbar_long_ratio = if !pinbar_long.is_empty() {
        median(&pinbar_long).clamp(Decimal::new(18, 1), Decimal::from(5))
    } else {
        defaults.pinbar_long_ratio_mean
    };

    let pinbar_short_ratio = if !pinbar_short.is_empty() {
        median(&pinbar_short).clamp(Decimal::new(1, 1), Decimal::new(6, 1))
    } else {
        defaults.pinbar_short_ratio_mean
    };

    // Engulfing: median body ratio
    let engulfing_ratio = if !engulfing.is_empty() {
        median(&engulfing).clamp(Decimal::new(105, 2), Decimal::from(2))
    } else {
        defaults.engulfing_ratio_mean
    };

    // Inside bar: median range ratio
    let insidebar_ratio = if !insidebar.is_empty() {
        median(&insidebar).clamp(Decimal::new(4, 1), Decimal::new(9, 1))
    } else {
        defaults.insidebar_ratio_mean
    };

    Ok(EmpiricalPriors {
        rsi_overbought_mean: rsi_overbought.round_dp(2),
        rsi_oversold_mean: rsi_oversold.round_dp(2),
        atr_expansion_mean: atr_expansion.round_dp(2),
        atr_contraction_mean: atr_contraction.round_dp(2),
        vwap_deviation_mean: vwap_deviation.round_dp(2),
        alma_slope_mean: alma_slope.round_dp(4),
        pinbar_long_ratio_mean: pinbar_long_ratio.round_dp(2),
        pinbar_short_ratio_mean: pinbar_short_ratio.round_dp(2),
        engulfing_ratio_mean: engulfing_ratio.round_dp(2),
        bos_margin_mean: Decimal::ZERO,
        insidebar_ratio_mean: insidebar_ratio.round_dp(2),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn synthetic_bar(open: Decimal, high: Decimal, low: Decimal, close: Decimal) -> FeedBar {
        FeedBar {
            start_time: 0,
            end_time: 3_600_000,
            open,
            high,
            low,
            close,
            volume: dec!(100),
            trade_count: 50,
        }
    }

    #[test]
    fn calibration_requires_at_least_64_bars() {
        let bars = vec![synthetic_bar(dec!(100), dec!(105), dec!(95), dec!(102)); 63];
        assert!(calibrate_empirical_priors(&bars).is_err());
    }

    #[test]
    fn calibration_on_synthetic_bars_extracts_consistent_priors() {
        let mut bars = Vec::new();
        for i in 0..100 {
            let base = dec!(100) + Decimal::from(i % 10);
            bars.push(synthetic_bar(
                base,
                base + dec!(4),
                base - dec!(2),
                base + dec!(1),
            ));
        }

        let priors = calibrate_empirical_priors(&bars).expect("calibrates successfully");
        assert!(priors.rsi_overbought_mean >= dec!(60));
        assert!(priors.rsi_oversold_mean <= dec!(40));
        assert!(priors.atr_contraction_mean < priors.atr_expansion_mean);
        assert!(priors.pinbar_long_ratio_mean >= dec!(1.8));
    }
}
