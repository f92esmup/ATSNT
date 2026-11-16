//! Technical indicators updated once per closed bar.
//!
//! Every indicator is updated once per closed bar of its feed, incrementally where the math
//! allows it, and entirely in fixed-point [`Decimal`] arithmetic:
//! - [`WilderRsi`]: Wilder's relative strength index.
//! - [`WilderAtr`]: Wilder's average true range, in price units.
//! - [`AtrRatio`]: the current ATR over the mean of its recent values, adimensional.
//! - [`AlmaSlope`]: the Arnaud Legoux moving-average slope in ATR units per bar.
//! - [`VwapDeviation`]: the distance of the close from the rolling VWAP in ATR units.
//!
//! Every rolling window is a fixed-capacity [`VecDeque`] ring buffer allocated once at
//! construction, so an update allocates nothing.

use std::collections::VecDeque;

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;

use crate::error::DomainError;

const HUNDRED: Decimal = Decimal::ONE_HUNDRED;

/// The fields of a closed bar that the indicators read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarInput {
    /// Highest trade price of the bar.
    pub high: Decimal,
    /// Lowest trade price of the bar.
    pub low: Decimal,
    /// Last trade price of the bar.
    pub close: Decimal,
    /// Traded base quantity of the bar.
    pub volume: Decimal,
}

impl From<&super::feeds::FeedBar> for BarInput {
    fn from(bar: &super::feeds::FeedBar) -> Self {
        Self {
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        }
    }
}

/// Rejects lookbacks below 2: a period of 1 degenerates every indicator here into its input.
fn validate_period(period: usize) -> Result<(), DomainError> {
    if period < 2 {
        return Err(DomainError::InvalidWindowSize(period));
    }
    Ok(())
}

/// Wilder's running average: the simple mean of the first `period` inputs as the seed, then
/// `avg = (avg · (period - 1) + x) / period`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WilderAverage {
    period: usize,
    seed_sum: Decimal,
    seed_count: usize,
    value: Option<Decimal>,
}

impl WilderAverage {
    fn new(period: usize) -> Self {
        Self {
            period,
            seed_sum: Decimal::ZERO,
            seed_count: 0,
            value: None,
        }
    }

    fn update(&mut self, x: Decimal) -> Option<Decimal> {
        let p = Decimal::from(self.period);
        match self.value {
            Some(avg) => self.value = Some((avg * (p - Decimal::ONE) + x) / p),
            None => {
                self.seed_sum += x;
                self.seed_count += 1;
                if self.seed_count == self.period {
                    self.value = Some(self.seed_sum / p);
                }
            }
        }
        self.value
    }

    fn reset(&mut self) {
        *self = Self::new(self.period);
    }
}

/// Wilder's relative strength index over close-to-close changes.
///
/// Ready after `period + 1` closes; the range is 0 to 100, and it is 100 whenever the average
/// loss is 0 (a flat series included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WilderRsi {
    prev_close: Option<Decimal>,
    gains: WilderAverage,
    losses: WilderAverage,
}

impl WilderRsi {
    /// Creates an RSI with the given period (must be >= 2).
    pub fn new(period: usize) -> Result<Self, DomainError> {
        validate_period(period)?;
        Ok(Self {
            prev_close: None,
            gains: WilderAverage::new(period),
            losses: WilderAverage::new(period),
        })
    }

    /// True once the RSI produces values.
    pub fn is_ready(&self) -> bool {
        self.gains.value.is_some()
    }

    /// Ingests the close of a bar and returns the RSI once ready.
    pub fn update(&mut self, close: Decimal) -> Option<Decimal> {
        let prev = self.prev_close.replace(close)?;
        let change = close - prev;
        let gain = self.gains.update(change.max(Decimal::ZERO));
        let loss = self.losses.update((-change).max(Decimal::ZERO));
        let (gain, loss) = (gain?, loss?);
        if loss.is_zero() {
            return Some(HUNDRED);
        }
        // 100 - 100 / (1 + G/L) rewritten as 100·G / (G + L): equal in exact arithmetic, with one
        // rounding fewer in `Decimal` (and invariant to a scaling of the prices).
        Some(HUNDRED * gain / (gain + loss))
    }

    /// Restores the fresh state.
    pub fn reset(&mut self) {
        self.prev_close = None;
        self.gains.reset();
        self.losses.reset();
    }
}

/// True range of `bar` against the previous close: `max(H - L, |H - C_prev|, |L - C_prev|)`.
fn true_range(bar: &BarInput, prev_close: Decimal) -> Decimal {
    (bar.high - bar.low)
        .max((bar.high - prev_close).abs())
        .max((bar.low - prev_close).abs())
}

/// Wilder's average true range, in price units.
///
/// The first bar has no previous close and yields no true range, so the ATR is ready after
/// `period + 1` bars. The seed is the simple mean of the first `period` true ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WilderAtr {
    prev_close: Option<Decimal>,
    average: WilderAverage,
}

impl WilderAtr {
    /// Creates an ATR with the given period (must be >= 2).
    pub fn new(period: usize) -> Result<Self, DomainError> {
        validate_period(period)?;
        Ok(Self {
            prev_close: None,
            average: WilderAverage::new(period),
        })
    }

    /// True once the ATR produces values.
    pub fn is_ready(&self) -> bool {
        self.average.value.is_some()
    }

    /// The latest ATR, if ready.
    pub fn value(&self) -> Option<Decimal> {
        self.average.value
    }

    /// Ingests a closed bar and returns the ATR once ready.
    pub fn update(&mut self, bar: &BarInput) -> Option<Decimal> {
        let prev = self.prev_close.replace(bar.close)?;
        self.average.update(true_range(bar, prev))
    }

    /// Restores the fresh state.
    pub fn reset(&mut self) {
        self.prev_close = None;
        self.average.reset();
    }
}

/// The current ATR divided by the mean of the last `window` ATR values, the current one
/// included. 1 means "typical volatility for this feed".
///
/// It consumes ATR values (from [`WilderAtr`]) rather than bars, so it composes with one ATR
/// instead of duplicating it. It is undefined until `window` ATR values exist, and when the
/// mean ATR is 0 (a flat series).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtrRatio {
    window_size: usize,
    window: VecDeque<Decimal>,
}

impl AtrRatio {
    /// Creates an ATR ratio over the given number of ATR values (must be >= 2).
    pub fn new(window_size: usize) -> Result<Self, DomainError> {
        validate_period(window_size)?;
        Ok(Self {
            window_size,
            window: VecDeque::with_capacity(window_size),
        })
    }

    /// True once `window` ATR values have been ingested.
    pub fn is_ready(&self) -> bool {
        self.window.len() == self.window_size
    }

    /// Ingests the latest ATR and returns the ratio once ready.
    pub fn update(&mut self, atr: Decimal) -> Option<Decimal> {
        if self.window.len() == self.window_size {
            self.window.pop_front();
        }
        self.window.push_back(atr);
        if !self.is_ready() {
            return None;
        }
        // The sum is recomputed over the window (O(window)) instead of kept as a running sum:
        // ATR values carry all 28 significant digits, and a running add/subtract would drift.
        let sum: Decimal = self.window.iter().sum();
        let mean = sum / Decimal::from(self.window_size);
        if mean <= Decimal::ZERO {
            return None;
        }
        Some(atr / mean)
    }

    /// Restores the fresh state.
    pub fn reset(&mut self) {
        self.window.clear();
    }
}

/// Slope of the Arnaud Legoux moving average in ATR units per bar:
/// `(ALMA_t - ALMA_{t-1}) / ATR_t`.
///
/// With `m = offset · (period - 1)` and `s = period / sigma`, the weights are
/// `w_i = exp(-(i - m)² / (2 s²))` for `i = 0..period`, where `i = 0` is the **oldest** close.
/// They are computed once at construction. `ALMA_t = Σ w_i · C_i / Σ w_i`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlmaSlope {
    period: usize,
    weights: Vec<Decimal>,
    weight_sum: Decimal,
    closes: VecDeque<Decimal>,
    prev_alma: Option<Decimal>,
    /// Number of ALMA values computed so far, saturating at 2 (a slope needs two).
    alma_count: u8,
}

impl AlmaSlope {
    /// Creates an ALMA slope (`period` >= 2, `0 <= offset <= 1`, `sigma > 0`).
    pub fn new(period: usize, offset: Decimal, sigma: Decimal) -> Result<Self, DomainError> {
        validate_period(period)?;
        if offset < Decimal::ZERO || offset > Decimal::ONE {
            return Err(DomainError::InvalidThreshold(format!(
                "ALMA offset must lie in [0, 1], got: {offset}"
            )));
        }
        if sigma <= Decimal::ZERO {
            return Err(DomainError::InvalidThreshold(format!(
                "ALMA sigma must be strictly positive, got: {sigma}"
            )));
        }
        let p = Decimal::from(period);
        let m = offset * (p - Decimal::ONE);
        let s = p / sigma;
        let two_s2 = Decimal::TWO * s * s;
        let mut weights = Vec::with_capacity(period);
        for i in 0..period {
            let d = Decimal::from(i) - m;
            let w = (-(d * d) / two_s2).checked_exp().ok_or_else(|| {
                DomainError::InvalidThreshold(format!("ALMA weight {i} overflows"))
            })?;
            weights.push(w);
        }
        let weight_sum: Decimal = weights.iter().sum();
        if weight_sum <= Decimal::ZERO {
            return Err(DomainError::InvalidThreshold(format!(
                "ALMA weights vanish for sigma {sigma}"
            )));
        }
        Ok(Self {
            period,
            weights,
            weight_sum,
            closes: VecDeque::with_capacity(period),
            prev_alma: None,
            alma_count: 0,
        })
    }

    /// The precomputed (unnormalised) weights, the oldest close first.
    pub fn weights(&self) -> &[Decimal] {
        &self.weights
    }

    /// True once two ALMA values exist, so a slope is defined whenever the ATR is positive.
    pub fn is_ready(&self) -> bool {
        self.alma_count >= 2
    }

    /// Ingests a close with the current ATR of the same bar and returns the slope once two
    /// ALMA values exist and the ATR is positive.
    pub fn update(&mut self, close: Decimal, atr: Option<Decimal>) -> Option<Decimal> {
        if self.closes.len() == self.period {
            self.closes.pop_front();
        }
        self.closes.push_back(close);
        if self.closes.len() < self.period {
            return None;
        }
        let weighted: Decimal = self
            .weights
            .iter()
            .zip(self.closes.iter())
            .map(|(w, c)| *w * *c)
            .sum();
        let alma = weighted / self.weight_sum;
        let prev = self.prev_alma.replace(alma);
        self.alma_count = self.alma_count.saturating_add(1).min(2);
        let (prev, atr) = (prev?, atr?);
        if atr <= Decimal::ZERO {
            return None;
        }
        Some((alma - prev) / atr)
    }

    /// Restores the fresh state.
    pub fn reset(&mut self) {
        self.closes.clear();
        self.prev_alma = None;
        self.alma_count = 0;
    }
}

/// Distance of the close from the rolling VWAP in ATR units: `|C_t - VWAP| / ATR_t`, with
/// `VWAP = Σ P_i · volume_i / Σ volume_i` over the last `window` bars and the typical price
/// `P_i = (H_i + L_i + C_i) / 3`.
///
/// Undefined until `window` bars exist, when their volume sum is 0, or without a positive ATR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VwapDeviation {
    window_size: usize,
    /// `(H + L + C, volume)` per bar: the division by 3 is applied once to the VWAP.
    window: VecDeque<(Decimal, Decimal)>,
}

impl VwapDeviation {
    /// Creates a VWAP deviation over the given number of bars (must be >= 2).
    pub fn new(window_size: usize) -> Result<Self, DomainError> {
        validate_period(window_size)?;
        Ok(Self {
            window_size,
            window: VecDeque::with_capacity(window_size),
        })
    }

    /// True once `window` bars have been ingested.
    pub fn is_ready(&self) -> bool {
        self.window.len() == self.window_size
    }

    /// Ingests a closed bar with the current ATR of the same bar and returns the deviation once
    /// defined.
    pub fn update(&mut self, bar: &BarInput, atr: Option<Decimal>) -> Option<Decimal> {
        if self.window.len() == self.window_size {
            self.window.pop_front();
        }
        self.window
            .push_back((bar.high + bar.low + bar.close, bar.volume));
        if !self.is_ready() {
            return None;
        }
        // Recomputed over the window (O(window)) so no running sum drifts.
        let (price_volume, volume) = self
            .window
            .iter()
            .fold((Decimal::ZERO, Decimal::ZERO), |(pv, v), (hlc, vol)| {
                (pv + *hlc * *vol, v + *vol)
            });
        let atr = atr?;
        if volume <= Decimal::ZERO || atr <= Decimal::ZERO {
            return None;
        }
        let vwap = price_volume / (Decimal::from(3) * volume);
        Some((bar.close - vwap).abs() / atr)
    }

    /// Restores the fresh state.
    pub fn reset(&mut self) {
        self.window.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn bar(high: Decimal, low: Decimal, close: Decimal) -> BarInput {
        BarInput {
            high,
            low,
            close,
            volume: Decimal::ONE,
        }
    }

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

    #[test]
    fn constructors_reject_degenerate_periods() {
        for p in [0, 1] {
            assert_eq!(WilderRsi::new(p), Err(DomainError::InvalidWindowSize(p)));
            assert_eq!(WilderAtr::new(p), Err(DomainError::InvalidWindowSize(p)));
            assert_eq!(AtrRatio::new(p), Err(DomainError::InvalidWindowSize(p)));
            assert_eq!(
                VwapDeviation::new(p),
                Err(DomainError::InvalidWindowSize(p))
            );
            assert_eq!(
                AlmaSlope::new(p, dec!(0.85), dec!(6)),
                Err(DomainError::InvalidWindowSize(p))
            );
        }
        assert!(matches!(
            AlmaSlope::new(9, dec!(1.5), dec!(6)),
            Err(DomainError::InvalidThreshold(_))
        ));
        assert!(matches!(
            AlmaSlope::new(9, dec!(0.85), Decimal::ZERO),
            Err(DomainError::InvalidThreshold(_))
        ));
    }

    #[test]
    fn rsi_matches_hand_computed_wilder_values() {
        // Period 3. Closes 10, 13, 10, 16, 12, 10 give the changes +3, -3, +6, -4, -2.
        // Close 4 seeds: G = (3 + 0 + 6) / 3 = 3, L = (0 + 3 + 0) / 3 = 1,
        //   RSI = 100 - 100 / (1 + 3/1) = 75.
        // Close 5 (-4): G = (3·2 + 0) / 3 = 2, L = (1·2 + 4) / 3 = 2, RSI = 100 - 100/2 = 50.
        // Close 6 (-2): G = (2·2 + 0) / 3 = 4/3, L = (2·2 + 2) / 3 = 2,
        //   RSI = 100 - 100 / (1 + (4/3)/2) = 100 - 100 / (5/3) = 40.
        let mut rsi = WilderRsi::new(3).unwrap();
        let out: Vec<_> = [10, 13, 10, 16, 12, 10]
            .into_iter()
            .map(|c| rsi.update(Decimal::from(c)))
            .collect();
        assert_eq!(&out[..3], &[None, None, None]);
        assert_eq!(out[3], Some(dec!(75)));
        assert_eq!(out[4], Some(dec!(50)));
        // 4/3 is not exact in Decimal: compared at 20 decimal places.
        assert_eq!(out[5].map(|v| v.round_dp(20)), Some(dec!(40)));
    }

    #[test]
    fn rsi_14_seeds_with_the_simple_mean_then_smooths() {
        // 15 closes from 100 alternating +3 and -1 (7 gains of 3, 7 losses of 1):
        //   G = 21/14 = 1.5, L = 7/14 = 0.5, RSI = 100 - 100 / (1 + 3) = 75 at close 15.
        // Close 16 (+3): G = (1.5·13 + 3)/14 = 22.5/14, L = (0.5·13 + 0)/14 = 6.5/14,
        //   RSI = 100·G/(G+L) = 100·22.5/29 = 2250/29 = 77.58620689655172413793...
        let mut rsi = WilderRsi::new(14).unwrap();
        let mut close = dec!(100);
        let mut out = vec![rsi.update(close)];
        for i in 0..15 {
            close += if i % 2 == 0 { dec!(3) } else { dec!(-1) };
            out.push(rsi.update(close));
        }
        assert!(out[..14].iter().all(Option::is_none));
        assert_eq!(out[14], Some(dec!(75)));
        assert_eq!(
            out[15].map(|v| v.round_dp(16)),
            Some(dec!(77.5862068965517241))
        );
    }

    #[test]
    fn rsi_is_100_without_losses() {
        let mut rsi = WilderRsi::new(14).unwrap();
        let out: Vec<_> = (0..30)
            .map(|i| rsi.update(Decimal::from(100 + i)))
            .collect();
        assert!(out[..14].iter().all(Option::is_none));
        assert!(out[14..].iter().all(|v| *v == Some(dec!(100))));
    }

    #[test]
    fn atr_matches_hand_computed_wilder_values_across_gaps() {
        // Period 3, bars (H, L, C):
        //   b1 (11, 9, 10): no previous close, no TR.
        //   b2 (12, 10, 11): TR = max(2, |12-10| = 2, |10-10| = 0) = 2.
        //   b3 (14, 11, 13): TR = max(3, |14-11| = 3, |11-11| = 0) = 3.
        //   b4 (13, 12, 12): TR = max(1, |13-13| = 0, |12-13| = 1) = 1; seed ATR = 6/3 = 2.
        //   b5 (20, 18, 19), gap up from 12: TR = max(2, |20-12| = 8, |18-12| = 6) = 8;
        //     ATR = (2·2 + 8)/3 = 4.
        //   b6 (19, 18, 18.5): TR = max(1, |19-19| = 0, |18-19| = 1) = 1; ATR = (4·2 + 1)/3 = 3.
        //   b7 (15, 14, 14.5), gap down from 18.5: TR = max(1, 3.5, |14-18.5| = 4.5) = 4.5;
        //     ATR = (3·2 + 4.5)/3 = 3.5.
        let bars = [
            bar(dec!(11), dec!(9), dec!(10)),
            bar(dec!(12), dec!(10), dec!(11)),
            bar(dec!(14), dec!(11), dec!(13)),
            bar(dec!(13), dec!(12), dec!(12)),
            bar(dec!(20), dec!(18), dec!(19)),
            bar(dec!(19), dec!(18), dec!(18.5)),
            bar(dec!(15), dec!(14), dec!(14.5)),
        ];
        let mut atr = WilderAtr::new(3).unwrap();
        let out: Vec<_> = bars.iter().map(|b| atr.update(b)).collect();
        assert_eq!(
            out,
            vec![
                None,
                None,
                None,
                Some(dec!(2)),
                Some(dec!(4)),
                Some(dec!(3)),
                Some(dec!(3.5))
            ]
        );
        assert_eq!(atr.value(), Some(dec!(3.5)));
    }

    #[test]
    fn true_range_uses_the_previous_close_across_gaps() {
        let gap_up = bar(dec!(110), dec!(108), dec!(109));
        assert_eq!(true_range(&gap_up, dec!(100)), dec!(10));
        let gap_down = bar(dec!(92), dec!(90), dec!(91));
        assert_eq!(true_range(&gap_down, dec!(100)), dec!(10));
        assert_eq!(true_range(&gap_down, dec!(91)), dec!(2));
    }

    /// Bars with closes rising by 1 and a range of 2 around the close: every TR is 2.
    fn constant_range(n: usize) -> Vec<BarInput> {
        (0..n)
            .map(|i| {
                let c = Decimal::from(100 + i);
                bar(c + Decimal::ONE, c - Decimal::ONE, c)
            })
            .collect()
    }

    #[test]
    fn atr_ratio_is_one_on_a_constant_range_series() {
        let mut atr = WilderAtr::new(14).unwrap();
        let mut ratio = AtrRatio::new(50).unwrap();
        let out: Vec<_> = constant_range(80)
            .iter()
            .map(|b| atr.update(b).and_then(|a| ratio.update(a)))
            .collect();
        // Ready at bar 15 + 49 = 64 (index 63).
        assert!(out[..63].iter().all(Option::is_none));
        assert!(out[63..].iter().all(|v| *v == Some(Decimal::ONE)));
    }

    #[test]
    fn atr_ratio_is_atr_over_the_window_mean() {
        // Window 3: ATRs 1, 2, 3 → 3 / 2 = 1.5; then 2, 3, 7 → 7 / 4 = 1.75.
        let mut ratio = AtrRatio::new(3).unwrap();
        assert_eq!(ratio.update(dec!(1)), None);
        assert_eq!(ratio.update(dec!(2)), None);
        assert_eq!(ratio.update(dec!(3)), Some(dec!(1.5)));
        assert_eq!(ratio.update(dec!(7)), Some(dec!(1.75)));
    }

    #[test]
    fn alma_weights_match_hand_values() {
        // m = 0.85·8 = 6.8, s = 9/6 = 1.5, 2s² = 4.5, w_i = exp(-(i - 6.8)²/4.5):
        //   i=0: exp(-46.24/4.5), i=1: exp(-33.64/4.5), i=2: exp(-23.04/4.5),
        //   i=3: exp(-14.44/4.5), i=4: exp(-7.84/4.5),  i=5: exp(-3.24/4.5),
        //   i=6: exp(-0.64/4.5),  i=7: exp(-0.04/4.5),  i=8: exp(-1.44/4.5).
        // Reference values evaluated at 60 significant digits, rounded to 12 decimal places.
        let alma = AlmaSlope::new(9, dec!(0.85), dec!(6)).unwrap();
        let expected = [
            dec!(0.000034465368),
            dec!(0.000566770807),
            dec!(0.005976022895),
            dec!(0.040401478874),
            dec!(0.175130788345),
            dec!(0.486752255960),
            dec!(0.867428473183),
            dec!(0.991150500488),
            dec!(0.726149037074),
        ];
        let got: Vec<_> = alma.weights().iter().map(|w| w.round_dp(12)).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn alma_slope_is_one_close_step_per_atr_on_a_linear_series() {
        // Closes 0..=8 give ALMA = Σ w_i·i / Σ w_i = 6.442764782256 (12 dp, same reference).
        // Each further close shifts every C_i by 1, so ALMA rises by exactly 1 per bar and the
        // slope is 1 / ATR = 1/2 = 0.5.
        let mut alma = AlmaSlope::new(9, dec!(0.85), dec!(6)).unwrap();
        for c in 0..9 {
            assert_eq!(alma.update(Decimal::from(c), Some(dec!(2))), None);
        }
        assert_eq!(
            alma.prev_alma.map(|v| v.round_dp(12)),
            Some(dec!(6.442764782256))
        );
        assert!(!alma.is_ready());
        let slope = alma.update(dec!(9), Some(dec!(2)));
        assert!(alma.is_ready());
        assert_eq!(slope.map(|v| v.round_dp(20)), Some(dec!(0.5)));
        assert_eq!(alma.update(dec!(10), None), None);
        assert_eq!(alma.update(dec!(11), Some(Decimal::ZERO)), None);
    }

    #[test]
    fn vwap_deviation_is_zero_when_the_close_is_the_vwap() {
        // Window 3, (H, L, C, volume): (101, 99, 100, 1), (111, 109, 110, 1), (106, 104, 105, 2).
        // Σ (H+L+C)·v = 300 + 330 + 630 = 1260, Σ v = 4, VWAP = 1260 / (3·4) = 105 = C.
        let mut vwap = VwapDeviation::new(3).unwrap();
        let mk = |h, l, c, v| BarInput {
            high: Decimal::from(h),
            low: Decimal::from(l),
            close: Decimal::from(c),
            volume: Decimal::from(v),
        };
        assert_eq!(vwap.update(&mk(101, 99, 100, 1), Some(dec!(2))), None);
        assert_eq!(vwap.update(&mk(111, 109, 110, 1), Some(dec!(2))), None);
        assert_eq!(
            vwap.update(&mk(106, 104, 105, 2), Some(dec!(2))),
            Some(Decimal::ZERO)
        );
        // Next window (111,109,110,1), (106,104,105,2), (121,119,120,1):
        // Σ = 330 + 630 + 360 = 1320, Σ v = 4, VWAP = 110, |120 - 110| / 2 = 5.
        assert_eq!(
            vwap.update(&mk(121, 119, 120, 1), Some(dec!(2))),
            Some(dec!(5))
        );
        // Zero volume or no positive ATR: undefined.
        assert_eq!(vwap.update(&mk(121, 119, 120, 1), None), None);
        let mut empty = VwapDeviation::new(2).unwrap();
        empty.update(&mk(101, 99, 100, 0), Some(dec!(2)));
        assert_eq!(empty.update(&mk(101, 99, 100, 0), Some(dec!(2))), None);
    }

    #[test]
    fn reset_equals_fresh() {
        let bars = series(70);
        let mut rsi = WilderRsi::new(3).unwrap();
        let mut atr = WilderAtr::new(3).unwrap();
        let mut ratio = AtrRatio::new(3).unwrap();
        let mut alma = AlmaSlope::new(3, dec!(0.85), dec!(6)).unwrap();
        let mut vwap = VwapDeviation::new(3).unwrap();
        for b in &bars {
            rsi.update(b.close);
            let a = atr.update(b);
            if let Some(a) = a {
                ratio.update(a);
            }
            alma.update(b.close, a);
            vwap.update(b, a);
        }
        rsi.reset();
        atr.reset();
        ratio.reset();
        alma.reset();
        vwap.reset();
        assert_eq!(rsi, WilderRsi::new(3).unwrap());
        assert_eq!(atr, WilderAtr::new(3).unwrap());
        assert_eq!(ratio, AtrRatio::new(3).unwrap());
        assert_eq!(alma, AlmaSlope::new(3, dec!(0.85), dec!(6)).unwrap());
        assert_eq!(vwap, VwapDeviation::new(3).unwrap());
    }
}
