//! The per-feed indicator bundle of the swarm (strategy spec §6.5): the five domain indicators
//! with the strategy's periods, updated once per closed bar of a feed.

use domain::{AlmaSlope, AtrRatio, BarInput, DomainError, VwapDeviation, WilderAtr, WilderRsi};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// RSI period of the strategy (§6.1).
pub const RSI_PERIOD: usize = 14;
/// ATR period of the strategy (§6.2).
pub const ATR_PERIOD: usize = 14;
/// Number of ATR values averaged by the ATR ratio of the strategy (§6.2).
pub const ATR_RATIO_WINDOW: usize = 50;
/// ALMA period of the strategy (§6.3).
pub const ALMA_PERIOD: usize = 9;
/// VWAP window of the strategy, in bars (§6.4).
pub const VWAP_WINDOW: usize = 50;

/// ALMA offset of the strategy (§6.3): 0.85.
fn alma_offset() -> Decimal {
    Decimal::new(85, 2)
}

/// ALMA sigma of the strategy (§6.3): 6.
fn alma_sigma() -> Decimal {
    Decimal::from(6)
}

/// The five indicator values of one feed after a closed bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndicatorSnapshot {
    /// Wilder RSI, 0 to 100.
    pub rsi: Decimal,
    /// Wilder ATR, in price units.
    pub atr: Decimal,
    /// ATR over its recent mean, adimensional.
    pub atr_ratio: Decimal,
    /// ALMA slope, in ATR units per bar.
    pub alma_slope: Decimal,
    /// Distance of the close from the VWAP, in ATR units.
    pub vwap_dev: Decimal,
}

/// The indicators of one feed with the strategy periods: RSI 14, ATR 14, ATR ratio 50,
/// ALMA (9, 0.85, 6), and VWAP 50 (§6.5).
///
/// The feed is warm after 64 closed bars (15 for the ATR plus 49 more for the ATR ratio).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedIndicators {
    rsi: WilderRsi,
    atr: WilderAtr,
    atr_ratio: AtrRatio,
    alma: AlmaSlope,
    vwap: VwapDeviation,
}

impl FeedIndicators {
    /// Creates the bundle with the strategy periods.
    pub fn new() -> Result<Self, DomainError> {
        Ok(Self {
            rsi: WilderRsi::new(RSI_PERIOD)?,
            atr: WilderAtr::new(ATR_PERIOD)?,
            atr_ratio: AtrRatio::new(ATR_RATIO_WINDOW)?,
            alma: AlmaSlope::new(ALMA_PERIOD, alma_offset(), alma_sigma())?,
            vwap: VwapDeviation::new(VWAP_WINDOW)?,
        })
    }

    /// True once every indicator has enough bars. A warm feed can still yield no snapshot on a
    /// bar whose ATR, mean ATR, or window volume is 0.
    pub fn is_warm(&self) -> bool {
        self.rsi.is_ready()
            && self.atr_ratio.is_ready()
            && self.alma.is_ready()
            && self.vwap.is_ready()
    }

    /// Ingests a closed bar into every indicator and returns the snapshot only when every value
    /// is defined.
    pub fn update(&mut self, bar: &BarInput) -> Option<IndicatorSnapshot> {
        let atr = self.atr.update(bar);
        let rsi = self.rsi.update(bar.close);
        let atr_ratio = atr.and_then(|value| self.atr_ratio.update(value));
        let alma_slope = self.alma.update(bar.close, atr);
        let vwap_dev = self.vwap.update(bar, atr);
        Some(IndicatorSnapshot {
            rsi: rsi?,
            atr: atr?,
            atr_ratio: atr_ratio?,
            alma_slope: alma_slope?,
            vwap_dev: vwap_dev?,
        })
    }

    /// Restores the fresh state.
    pub fn reset(&mut self) {
        self.rsi.reset();
        self.atr.reset();
        self.atr_ratio.reset();
        self.alma.reset();
        self.vwap.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// A deterministic 80-bar series with positive ranges and varying volume.
    fn series(n: usize) -> Vec<BarInput> {
        (0..n)
            .map(|i| {
                let close = Decimal::from(100 + (i * 7 % 11)) - Decimal::from(5);
                BarInput {
                    high: close + Decimal::ONE + Decimal::from(i % 3),
                    low: close - Decimal::ONE - Decimal::from(i % 2),
                    close,
                    volume: Decimal::from(1 + i % 4),
                }
            })
            .collect()
    }

    fn scaled(bars: &[BarInput], k: Decimal) -> Vec<BarInput> {
        bars.iter()
            .map(|b| BarInput {
                high: b.high * k,
                low: b.low * k,
                close: b.close * k,
                volume: b.volume,
            })
            .collect()
    }

    #[test]
    fn bundle_is_warm_exactly_at_bar_64() {
        let mut ind = FeedIndicators::new().unwrap();
        for (i, b) in series(80).iter().enumerate() {
            let snapshot = ind.update(b);
            if i < 63 {
                assert_eq!(snapshot, None, "bar {}", i + 1);
                assert!(!ind.is_warm(), "bar {}", i + 1);
            } else {
                assert!(snapshot.is_some(), "bar {}", i + 1);
                assert!(ind.is_warm(), "bar {}", i + 1);
            }
        }
    }

    #[test]
    fn adimensional_values_are_invariant_to_a_price_scaling() {
        let bars = series(80);
        let mut base = FeedIndicators::new().unwrap();
        let mut tenfold = FeedIndicators::new().unwrap();
        let mut compared = 0;
        for (a, b) in bars.iter().zip(scaled(&bars, dec!(10)).iter()) {
            match (base.update(a), tenfold.update(b)) {
                (Some(x), Some(y)) => {
                    // Exact equality is not guaranteed: `Decimal` divisions round at 28
                    // significant digits, so values are compared at 18 decimal places.
                    let r = |v: Decimal| v.round_dp(18);
                    assert_eq!(r(x.rsi), r(y.rsi));
                    assert_eq!(r(x.atr_ratio), r(y.atr_ratio));
                    assert_eq!(r(x.alma_slope), r(y.alma_slope));
                    assert_eq!(r(x.vwap_dev), r(y.vwap_dev));
                    assert_eq!(r(x.atr * dec!(10)), r(y.atr));
                    compared += 1;
                }
                (None, None) => {}
                other => panic!("readiness differs: {other:?}"),
            }
        }
        assert_eq!(compared, 80 - 63);
    }

    #[test]
    fn reset_equals_fresh() {
        let bars = series(70);
        let mut ind = FeedIndicators::new().unwrap();
        for b in &bars {
            ind.update(b);
        }
        ind.reset();
        assert_eq!(ind, FeedIndicators::new().unwrap());
        // After a reset the bundle reproduces the fresh output bar for bar.
        let mut fresh = FeedIndicators::new().unwrap();
        for b in &bars {
            assert_eq!(ind.update(b), fresh.update(b));
        }
    }
}
