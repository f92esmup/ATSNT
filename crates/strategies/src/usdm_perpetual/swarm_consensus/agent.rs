//! The macro and micro stages of an agent (strategy spec §8.1–8.2).
//!
//! The macro stage reads the feed's indicator snapshot and produces a set of regime states; the
//! micro stage reads the feed's closed-bar window and produces a set of `(pattern, direction)`.
//! Every detector runs and none short-circuits another: both stages return **sets**.

use domain::{FeedBar, IndicatorSnapshot};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::genome::Genome;

/// A market-regime state of the macro stage (§8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MacroState {
    /// `alma_slope > alma_slope_gene`.
    Bullish,
    /// `alma_slope < -alma_slope_gene`.
    Bearish,
    /// `RSI > rsi_overbought`.
    Overbought,
    /// `RSI < rsi_oversold`.
    Oversold,
    /// `atr_ratio > atr_expansion`.
    High,
    /// `atr_ratio < atr_contraction`.
    Low,
    /// `vwap_dev > vwap_deviation`.
    Stretched,
}

impl MacroState {
    /// Every state, in declaration order.
    pub const ALL: [MacroState; 7] = [
        MacroState::Bullish,
        MacroState::Bearish,
        MacroState::Overbought,
        MacroState::Oversold,
        MacroState::High,
        MacroState::Low,
        MacroState::Stretched,
    ];

    /// The state under the mirror transformation of §8.3: `Bullish ↔ Bearish`,
    /// `Overbought ↔ Oversold`; `High`, `Low`, and `Stretched` are fixed.
    pub fn mirror(self) -> Self {
        match self {
            MacroState::Bullish => MacroState::Bearish,
            MacroState::Bearish => MacroState::Bullish,
            MacroState::Overbought => MacroState::Oversold,
            MacroState::Oversold => MacroState::Overbought,
            other => other,
        }
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// The set of macro states that hold for one evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MacroSet(u8);

impl MacroSet {
    /// The set of the given states.
    pub fn of(states: &[MacroState]) -> Self {
        let mut set = Self::default();
        for state in states {
            set.insert(*state);
        }
        set
    }

    /// Adds a state.
    pub fn insert(&mut self, state: MacroState) {
        self.0 |= state.bit();
    }

    /// Whether the state holds.
    pub fn contains(&self, state: MacroState) -> bool {
        self.0 & state.bit() != 0
    }

    /// The set under the mirror transformation (§8.3).
    pub fn mirror(&self) -> Self {
        let mut set = Self::default();
        for state in MacroState::ALL {
            if self.contains(state) {
                set.insert(state.mirror());
            }
        }
        set
    }

    /// The states in declaration order.
    pub fn states(&self) -> Vec<MacroState> {
        MacroState::ALL
            .into_iter()
            .filter(|s| self.contains(*s))
            .collect()
    }
}

/// Computes the macro states of one agent with strict comparisons (§8.1).
pub fn macro_states(genome: &Genome, indicators: &IndicatorSnapshot) -> MacroSet {
    let mut set = MacroSet::default();
    if indicators.alma_slope > genome.alma_slope {
        set.insert(MacroState::Bullish);
    }
    if indicators.alma_slope < -genome.alma_slope {
        set.insert(MacroState::Bearish);
    }
    if indicators.rsi > genome.rsi_overbought {
        set.insert(MacroState::Overbought);
    }
    if indicators.rsi < genome.rsi_oversold {
        set.insert(MacroState::Oversold);
    }
    if indicators.atr_ratio > genome.atr_expansion {
        set.insert(MacroState::High);
    }
    if indicators.atr_ratio < genome.atr_contraction {
        set.insert(MacroState::Low);
    }
    if indicators.vwap_dev > genome.vwap_deviation {
        set.insert(MacroState::Stretched);
    }
    set
}

/// A price pattern of the micro stage (§8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pattern {
    /// Pin bar (rejection wick).
    PinBar,
    /// Engulfing of the previous body.
    Engulfing,
    /// Inside bar of the previous range.
    InsideBar,
    /// Break of structure over the whole window.
    BreakOfStructure,
}

impl Pattern {
    /// Every pattern, in declaration order.
    pub const ALL: [Pattern; 4] = [
        Pattern::PinBar,
        Pattern::Engulfing,
        Pattern::InsideBar,
        Pattern::BreakOfStructure,
    ];
}

/// The direction attached to a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Bullish.
    Up,
    /// Bearish.
    Down,
    /// Direction-free (inside bar).
    Neutral,
}

impl Direction {
    /// `Up ↔ Down`; `Neutral` is fixed (§8.3).
    pub fn mirror(self) -> Self {
        match self {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
            Direction::Neutral => Direction::Neutral,
        }
    }
}

/// The set of `(pattern, direction)` found on the window. An empty set is noise (§8.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MicroSet(u16);

impl MicroSet {
    fn bit(pattern: Pattern, direction: Direction) -> u16 {
        1 << ((pattern as u16) * 3 + direction as u16)
    }

    /// The set of the given pairs.
    pub fn of(pairs: &[(Pattern, Direction)]) -> Self {
        let mut set = Self::default();
        for (pattern, direction) in pairs {
            set.insert(*pattern, *direction);
        }
        set
    }

    /// Adds a pair.
    pub fn insert(&mut self, pattern: Pattern, direction: Direction) {
        self.0 |= Self::bit(pattern, direction);
    }

    /// Whether the pair was found.
    pub fn contains(&self, pattern: Pattern, direction: Direction) -> bool {
        self.0 & Self::bit(pattern, direction) != 0
    }

    /// The set under the mirror transformation (§8.3): directions `Up ↔ Down`.
    pub fn mirror(&self) -> Self {
        let mut set = Self::default();
        for pattern in Pattern::ALL {
            for direction in [Direction::Up, Direction::Down, Direction::Neutral] {
                if self.contains(pattern, direction) {
                    set.insert(pattern, direction.mirror());
                }
            }
        }
        set
    }

    /// True when no pattern was found (noise).
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

/// Runs every micro detector on `window` (index 0 = last closed bar) with the feed's ATR in
/// price units (§8.2).
pub fn micro_patterns(genome: &Genome, window: &[FeedBar], atr: Decimal) -> MicroSet {
    let mut set = MicroSet::default();
    let Some(bar0) = window.first() else {
        return set;
    };
    let (o0, h0, l0, c0) = (bar0.open, bar0.high, bar0.low, bar0.close);
    let body0 = (c0 - o0).abs();

    // Pin bar up, with the C10 guard: positive range and a positive lower wick.
    if c0 >= o0
        && h0 > l0
        && o0 - l0 > Decimal::ZERO
        && o0 - l0 >= genome.pinbar_long_ratio * body0
        && h0 - c0 <= genome.pinbar_short_ratio * body0
    {
        set.insert(Pattern::PinBar, Direction::Up);
    }
    // Pin bar down, the exact mirror of the up case: positive range and a positive upper wick,
    // so a gravestone doji is a bearish pin bar as a dragonfly doji is a bullish one.
    if c0 <= o0
        && h0 > l0
        && h0 - o0 > Decimal::ZERO
        && h0 - o0 >= genome.pinbar_long_ratio * body0
        && c0 - l0 <= genome.pinbar_short_ratio * body0
    {
        set.insert(Pattern::PinBar, Direction::Down);
    }

    if let Some(bar1) = window.get(1) {
        let (o1, h1, l1, c1) = (bar1.open, bar1.high, bar1.low, bar1.close);
        if c0 > o0
            && c1 < o1
            && o0 <= c1
            && c0 >= o1
            && (c0 - o0) > genome.engulfing_ratio * (o1 - c1)
        {
            set.insert(Pattern::Engulfing, Direction::Up);
        }
        if c0 < o0
            && c1 > o1
            && o0 >= c1
            && c0 <= o1
            && (o0 - c0) > genome.engulfing_ratio * (c1 - o1)
        {
            set.insert(Pattern::Engulfing, Direction::Down);
        }
        if h0 < h1 && l0 > l1 && (h0 - l0) <= genome.insidebar_ratio * (h1 - l1) {
            set.insert(Pattern::InsideBar, Direction::Neutral);
        }

        // Break of structure against every earlier bar of the window; margin in ATR units.
        let margin = genome.bos_margin * atr;
        let earlier = &window[1..];
        if earlier.iter().all(|bar| bar.high < c0 - margin) {
            set.insert(Pattern::BreakOfStructure, Direction::Up);
        }
        if earlier.iter().all(|bar| bar.low > c0 + margin) {
            set.insert(Pattern::BreakOfStructure, Direction::Down);
        }
    }
    set
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// A fixed genome with round thresholds, shared by the rule tests.
    pub(crate) fn reference_genome() -> Genome {
        Genome {
            rsi_overbought: dec!(70),
            rsi_oversold: dec!(30),
            atr_expansion: dec!(1.2),
            atr_contraction: dec!(0.6),
            vwap_deviation: dec!(1.0),
            alma_slope: dec!(0.07),
            pinbar_long_ratio: dec!(2.0),
            pinbar_short_ratio: dec!(0.5),
            engulfing_ratio: dec!(1.1),
            bos_margin: dec!(0.1),
            insidebar_ratio: dec!(0.8),
        }
    }

    /// A neutral snapshot: no macro state holds for the reference genome.
    pub(crate) fn neutral_snapshot() -> IndicatorSnapshot {
        IndicatorSnapshot {
            rsi: dec!(50),
            atr: dec!(1),
            atr_ratio: dec!(1.0),
            alma_slope: dec!(0),
            vwap_dev: dec!(0.5),
        }
    }

    pub(crate) fn bar(open: Decimal, high: Decimal, low: Decimal, close: Decimal) -> FeedBar {
        FeedBar {
            start_time: 0,
            end_time: 0,
            open,
            high,
            low,
            close,
            volume: Decimal::ONE,
            trade_count: 1,
        }
    }

    fn states(snapshot: IndicatorSnapshot) -> MacroSet {
        macro_states(&reference_genome(), &snapshot)
    }

    #[test]
    fn the_neutral_snapshot_has_no_macro_state() {
        assert_eq!(states(neutral_snapshot()), MacroSet::default());
    }

    #[test]
    fn each_macro_state_has_a_positive_a_negative_and_a_strict_threshold_case() {
        // (state, setter, firing value, non-firing value, threshold value)
        type Setter = fn(&mut IndicatorSnapshot, Decimal);
        let cases: [(MacroState, Setter, Decimal, Decimal, Decimal); 7] = [
            (
                MacroState::Bullish,
                |s, v| s.alma_slope = v,
                dec!(0.08),
                dec!(0.06),
                dec!(0.07),
            ),
            (
                MacroState::Bearish,
                |s, v| s.alma_slope = v,
                dec!(-0.08),
                dec!(-0.06),
                dec!(-0.07),
            ),
            (
                MacroState::Overbought,
                |s, v| s.rsi = v,
                dec!(71),
                dec!(69),
                dec!(70),
            ),
            (
                MacroState::Oversold,
                |s, v| s.rsi = v,
                dec!(29),
                dec!(31),
                dec!(30),
            ),
            (
                MacroState::High,
                |s, v| s.atr_ratio = v,
                dec!(1.3),
                dec!(1.1),
                dec!(1.2),
            ),
            (
                MacroState::Low,
                |s, v| s.atr_ratio = v,
                dec!(0.5),
                dec!(0.7),
                dec!(0.6),
            ),
            (
                MacroState::Stretched,
                |s, v| s.vwap_dev = v,
                dec!(1.1),
                dec!(0.9),
                dec!(1.0),
            ),
        ];
        for (state, set, yes, no, equal) in cases {
            for (value, expected) in [(yes, true), (no, false), (equal, false)] {
                let mut snapshot = neutral_snapshot();
                set(&mut snapshot, value);
                assert_eq!(
                    states(snapshot).contains(state),
                    expected,
                    "{state:?} at {value}"
                );
            }
            let mut snapshot = neutral_snapshot();
            set(&mut snapshot, yes);
            assert_eq!(
                states(snapshot),
                MacroSet::of(&[state]),
                "{state:?} is alone"
            );
        }
    }

    fn micro(window: &[FeedBar]) -> MicroSet {
        micro_patterns(&reference_genome(), window, dec!(2))
    }

    #[test]
    fn pin_bar_up_positive_and_negative() {
        // body 0.5, lower wick 1.0 = 2 · body, upper wick 0.1 <= 0.25.
        let yes = bar(dec!(10), dec!(10.6), dec!(9), dec!(10.5));
        assert!(micro(&[yes]).contains(Pattern::PinBar, Direction::Up));
        let short_wick = bar(dec!(10), dec!(10.6), dec!(9.5), dec!(10.5));
        assert!(!micro(&[short_wick]).contains(Pattern::PinBar, Direction::Up));
        let long_upper = bar(dec!(10), dec!(11), dec!(9), dec!(10.5));
        assert!(!micro(&[long_upper]).contains(Pattern::PinBar, Direction::Up));
    }

    #[test]
    fn a_flat_bar_is_no_pin_bar() {
        let flat = bar(dec!(10), dec!(10), dec!(10), dec!(10));
        let set = micro(&[flat]);
        assert!(!set.contains(Pattern::PinBar, Direction::Up));
        assert!(!set.contains(Pattern::PinBar, Direction::Down));
        assert!(set.is_empty());
        // A zero-body bar whose only wick is above has no positive lower wick either.
        let upper_only = bar(dec!(10), dec!(11), dec!(10), dec!(10));
        assert!(!micro(&[upper_only]).contains(Pattern::PinBar, Direction::Up));
    }

    #[test]
    fn pin_bar_down_positive_and_negative() {
        let yes = bar(dec!(10.5), dec!(11.5), dec!(9.9), dec!(10));
        assert!(micro(&[yes]).contains(Pattern::PinBar, Direction::Down));
        let short_wick = bar(dec!(10.5), dec!(11), dec!(9.9), dec!(10));
        assert!(!micro(&[short_wick]).contains(Pattern::PinBar, Direction::Down));
    }

    #[test]
    fn dojis_are_pin_bars_symmetrically() {
        // Dragonfly doji: O = C = H above a long lower wick.
        let dragonfly = bar(dec!(10), dec!(10), dec!(9), dec!(10));
        let set = micro(&[dragonfly]);
        assert!(set.contains(Pattern::PinBar, Direction::Up));
        assert!(!set.contains(Pattern::PinBar, Direction::Down));
        // Its mirror, the gravestone doji: O = C = L below a long upper wick.
        let gravestone = bar(dec!(10), dec!(11), dec!(10), dec!(10));
        let set = micro(&[gravestone]);
        assert!(set.contains(Pattern::PinBar, Direction::Down));
        assert!(!set.contains(Pattern::PinBar, Direction::Up));
    }

    #[test]
    fn engulfing_up_positive_negative_and_strict_ratio() {
        let prev = bar(dec!(10.4), dec!(10.5), dec!(9.9), dec!(10.0));
        let yes = bar(dec!(9.9), dec!(10.7), dec!(9.8), dec!(10.6));
        assert!(micro(&[yes.clone(), prev.clone()]).contains(Pattern::Engulfing, Direction::Up));
        let bullish_prev = bar(dec!(10.0), dec!(10.5), dec!(9.9), dec!(10.4));
        assert!(!micro(&[yes, bullish_prev]).contains(Pattern::Engulfing, Direction::Up));
        // Body exactly 1.1 · 0.4 = 0.44 does not fire.
        let equal = bar(dec!(10.0), dec!(10.5), dec!(9.9), dec!(10.44));
        assert!(!micro(&[equal, prev]).contains(Pattern::Engulfing, Direction::Up));
    }

    #[test]
    fn engulfing_down_positive_and_negative() {
        let prev = bar(dec!(10.0), dec!(10.5), dec!(9.9), dec!(10.4));
        let yes = bar(dec!(10.5), dec!(10.6), dec!(9.8), dec!(9.9));
        assert!(micro(&[yes.clone(), prev]).contains(Pattern::Engulfing, Direction::Down));
        let bearish_prev = bar(dec!(10.4), dec!(10.5), dec!(9.9), dec!(10.0));
        assert!(!micro(&[yes, bearish_prev]).contains(Pattern::Engulfing, Direction::Down));
    }

    #[test]
    fn inside_bar_positive_negative_and_strict_edges() {
        let prev = bar(dec!(10), dec!(11), dec!(9), dec!(10));
        let yes = bar(dec!(10), dec!(10.5), dec!(9.5), dec!(10));
        assert!(micro(&[yes, prev.clone()]).contains(Pattern::InsideBar, Direction::Neutral));
        let too_wide = bar(dec!(10), dec!(10.9), dec!(9.1), dec!(10));
        assert!(!micro(&[too_wide, prev.clone()]).contains(Pattern::InsideBar, Direction::Neutral));
        let same_high = bar(dec!(10), dec!(11), dec!(9.5), dec!(10));
        assert!(!micro(&[same_high, prev]).contains(Pattern::InsideBar, Direction::Neutral));
    }

    #[test]
    fn break_of_structure_up_needs_an_earlier_bar_and_uses_its_atr_margin() {
        let bar0 = bar(dec!(11), dec!(12.1), dec!(10.9), dec!(12));
        let earlier = bar(dec!(10), dec!(11), dec!(9), dec!(10.5));
        let near = bar(dec!(10), dec!(11.7), dec!(9), dec!(10.5));
        assert!(
            !micro(std::slice::from_ref(&bar0)).contains(Pattern::BreakOfStructure, Direction::Up)
        );
        // ATR 2, margin 0.1 ATR = 0.2: every high must be < 11.8.
        let window = [bar0.clone(), earlier.clone(), near.clone()];
        assert!(micro(&window).contains(Pattern::BreakOfStructure, Direction::Up));
        // ATR 4 widens the margin to 0.4: 11.7 is no longer below 11.6.
        let wide = micro_patterns(&reference_genome(), &window, dec!(4));
        assert!(!wide.contains(Pattern::BreakOfStructure, Direction::Up));
        // Every earlier bar counts, not only bar 1.
        let blocker = bar(dec!(10), dec!(11.85), dec!(9), dec!(10.5));
        let window = [bar0, earlier, blocker];
        assert!(!micro(&window).contains(Pattern::BreakOfStructure, Direction::Up));
    }

    #[test]
    fn break_of_structure_down_positive_and_negative() {
        let bar0 = bar(dec!(9), dec!(9.1), dec!(7.9), dec!(8));
        let earlier = bar(dec!(10), dec!(11), dec!(9), dec!(10.5));
        assert!(micro(&[bar0.clone(), earlier.clone()])
            .contains(Pattern::BreakOfStructure, Direction::Down));
        let blocker = bar(dec!(10), dec!(11), dec!(8.15), dec!(10.5));
        assert!(
            !micro(&[bar0, earlier, blocker]).contains(Pattern::BreakOfStructure, Direction::Down)
        );
    }

    #[test]
    fn an_empty_window_has_no_pattern() {
        assert!(micro(&[]).is_empty());
    }

    #[test]
    fn mirrors_are_involutions() {
        for state in MacroState::ALL {
            assert_eq!(state.mirror().mirror(), state);
        }
        for direction in [Direction::Up, Direction::Down, Direction::Neutral] {
            assert_eq!(direction.mirror().mirror(), direction);
        }
    }
}
