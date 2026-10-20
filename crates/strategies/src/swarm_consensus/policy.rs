//! The consensus policy (Stage B) of the swarm consensus strategy: the original level machine
//! with hysteresis on two axes, the signed target, risk per level, and its HPO search space
//! (strategy spec `docs/strategy/03-swarm-consensus.md` §10 and §14.2).

use domain::{Decision, TargetPosition};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use super::sample::ConsensusSample;
use crate::strategy::HyperparameterSpace;

/// Highest level of one axis (and the highest absolute target level).
const TOP_LEVEL: u8 = 3;

/// Reasons a [`PolicyConfig`] is rejected (strategy spec §10.1).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyConfigError {
    /// `threshold_base` must be strictly positive.
    #[error("threshold_base must be > 0, got {0}")]
    NonPositiveThresholdBase(Decimal),
    /// `threshold_step` must be strictly positive.
    #[error("threshold_step must be > 0, got {0}")]
    NonPositiveThresholdStep(Decimal),
    /// The level-3 threshold `threshold_base + 2·threshold_step` must not exceed 1.
    #[error("level-3 threshold (threshold_base + 2*threshold_step) must be <= 1, got {0}")]
    TopThresholdAboveOne(Decimal),
    /// `hysteresis` must not be negative.
    #[error("hysteresis must be >= 0, got {0}")]
    NegativeHysteresis(Decimal),
    /// `base_risk` must be strictly positive.
    #[error("base_risk must be > 0, got {0}")]
    NonPositiveBaseRisk(Decimal),
    /// `risk_increment` must be strictly positive.
    #[error("risk_increment must be > 0, got {0}")]
    NonPositiveRiskIncrement(Decimal),
    /// The level-3 risk `base_risk + 2·risk_increment` must not exceed 1.
    #[error("level-3 risk (base_risk + 2*risk_increment) must be <= 1, got {0}")]
    TopRiskAboveOne(Decimal),
    /// `stop_atr_multiplier` must be strictly positive.
    #[error("stop_atr_multiplier must be > 0, got {0}")]
    NonPositiveStopMultiplier(Decimal),
}

/// Parameters of the consensus policy, searched by HPO (strategy spec §14.2).
///
/// Thresholds and hysteresis are fractions of the swarm size `N`; risks are fractions of equity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Level-1 threshold `L1`.
    pub threshold_base: Decimal,
    /// Distance between consecutive level thresholds (`L2 = L1 + step`, `L3 = L1 + 2·step`).
    pub threshold_step: Decimal,
    /// Extra net vote against a level needed to step down from it (`H`).
    pub hysteresis: Decimal,
    /// Risk fraction of a level-1 position.
    pub base_risk: Decimal,
    /// Extra risk fraction per level above 1.
    pub risk_increment: Decimal,
    /// Stop distance in multiples of the feed-1 ATR (fixed at 3.0 in the search).
    pub stop_atr_multiplier: Decimal,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            threshold_base: dec!(0.50),
            threshold_step: dec!(0.05),
            hysteresis: dec!(0.25),
            base_risk: dec!(0.010),
            risk_increment: dec!(0.005),
            stop_atr_multiplier: dec!(3.0),
        }
    }
}

impl std::fmt::Display for PolicyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "B:{}|S:{}|H:{}|R:{}|Ri:{}|Stop:{}",
            self.threshold_base,
            self.threshold_step,
            self.hysteresis,
            self.base_risk,
            self.risk_increment,
            self.stop_atr_multiplier
        )
    }
}

impl PolicyConfig {
    /// The three level thresholds `(L1, L2, L3)`.
    pub fn thresholds(&self) -> (Decimal, Decimal, Decimal) {
        (
            self.threshold_base,
            self.threshold_base + self.threshold_step,
            self.threshold_base + dec!(2) * self.threshold_step,
        )
    }

    /// Risk fraction of the level-3 position, `base_risk + 2·risk_increment`.
    pub fn top_level_risk(&self) -> Decimal {
        self.risk_fraction(TOP_LEVEL)
    }

    /// Risk fraction for an absolute level: `base_risk + (level - 1)·risk_increment` for
    /// `level >= 1`, and 0 for level 0 (strategy spec §10.4). Levels above 3 count as 3.
    pub fn risk_fraction(&self, abs_level: u8) -> Decimal {
        match abs_level.min(TOP_LEVEL) {
            0 => Decimal::ZERO,
            level => self.base_risk + Decimal::from(level - 1) * self.risk_increment,
        }
    }

    /// Checks the rules of strategy spec §10.1.
    pub fn validate(&self) -> Result<(), PolicyConfigError> {
        if self.threshold_base <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveThresholdBase(
                self.threshold_base,
            ));
        }
        if self.threshold_step <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveThresholdStep(
                self.threshold_step,
            ));
        }
        let (_, _, l3) = self.thresholds();
        if l3 > Decimal::ONE {
            return Err(PolicyConfigError::TopThresholdAboveOne(l3));
        }
        if self.hysteresis < Decimal::ZERO {
            return Err(PolicyConfigError::NegativeHysteresis(self.hysteresis));
        }
        if self.base_risk <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveBaseRisk(self.base_risk));
        }
        if self.risk_increment <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveRiskIncrement(
                self.risk_increment,
            ));
        }
        let top_risk = self.top_level_risk();
        if top_risk > Decimal::ONE {
            return Err(PolicyConfigError::TopRiskAboveOne(top_risk));
        }
        if self.stop_atr_multiplier <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveStopMultiplier(
                self.stop_atr_multiplier,
            ));
        }
        Ok(())
    }
}

/// One axis of the original level machine (`CRiskManager::GetTradeParams`, strategy spec
/// §10.2), reproduced exactly: moving up is immediate (a jump from 0 to 3 is allowed); moving
/// down needs `h` extra net vote against and may cascade over several levels in one update.
///
/// A `current` above 3 is treated as 3.
pub fn axis_update(
    current: u8,
    net: Decimal,
    l1: Decimal,
    l2: Decimal,
    l3: Decimal,
    h: Decimal,
) -> u8 {
    let mut t = current.min(TOP_LEVEL);
    if net >= l3 {
        t = 3;
    } else if net >= l2 && t < 2 {
        t = 2;
    } else if net >= l1 && t < 1 {
        t = 1;
    }
    if t == 3 && net < l3 - h {
        t = 2;
    }
    if t == 2 && net < l2 - h {
        t = 1;
    }
    if t == 1 && net < l1 - h {
        t = 0;
    }
    t
}

/// The consensus policy: two level machines (long and short) and the signed target they imply
/// (strategy spec §10.3–10.6).
#[derive(Debug, Clone)]
pub struct ConsensusPolicy {
    config: PolicyConfig,
    long_level: u8,
    short_level: u8,
    last_level: i8,
}

impl ConsensusPolicy {
    /// Builds a flat policy from a validated configuration.
    pub fn new(config: PolicyConfig) -> Result<Self, PolicyConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            long_level: 0,
            short_level: 0,
            last_level: 0,
        })
    }

    /// The configuration in use.
    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    /// Current level of the long axis (0..=3).
    pub fn long_level(&self) -> u8 {
        self.long_level
    }

    /// Current level of the short axis (0..=3).
    pub fn short_level(&self) -> u8 {
        self.short_level
    }

    /// Signed level of the last emitted target (0 before any emission).
    pub fn last_level(&self) -> i8 {
        self.last_level
    }

    /// Feeds one sample and returns a [`Decision::Target`] only when the signed target level
    /// changes, including a return to 0 (strategy spec §10.6).
    ///
    /// Both axes always advance with the sample's net votes, so their hysteresis state stays
    /// faithful to the vote history. If a non-zero target cannot be expressed because the
    /// sample's `atr_stop_reference` is not positive (no valid stop distance), nothing is
    /// emitted and the last emitted level is kept: the change is emitted by the next sample
    /// that carries a positive ATR, if the target still differs then.
    pub fn on_sample(&mut self, sample: &ConsensusSample, allow_short: bool) -> Option<Decision> {
        let (l1, l2, l3) = self.config.thresholds();
        let h = self.config.hysteresis;
        self.long_level = axis_update(self.long_level, sample.long_net(), l1, l2, l3, h);
        self.short_level = if allow_short {
            axis_update(self.short_level, sample.short_net(), l1, l2, l3, h)
        } else {
            0
        };
        // Both levels are at most 3, so the difference always fits an `i8`.
        let k = if self.long_level > 0 && self.short_level > 0 {
            0
        } else {
            self.long_level as i8 - self.short_level as i8
        };
        if k == self.last_level {
            return None;
        }
        let (risk, stop) = if k == 0 {
            (Decimal::ZERO, Decimal::ZERO)
        } else {
            (
                self.config.risk_fraction(k.unsigned_abs()),
                self.config.stop_atr_multiplier * sample.atr_stop_reference,
            )
        };
        // Invalid only for a non-zero level with a non-positive ATR (the config is validated).
        let target = TargetPosition::new(sample.timestamp, k, risk, stop).ok()?;
        self.last_level = k;
        Some(Decision::Target(target))
    }

    /// Returns to the flat initial state, keeping the configuration.
    pub fn reset(&mut self) {
        self.long_level = 0;
        self.short_level = 0;
        self.last_level = 0;
    }
}

fn default_max_level_risk() -> Decimal {
    dec!(0.05)
}

/// HPO search space of [`PolicyConfig`] (strategy spec §14.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmPolicySpace {
    /// Grid of `threshold_base`.
    pub threshold_bases: Vec<Decimal>,
    /// Grid of `threshold_step`.
    pub threshold_steps: Vec<Decimal>,
    /// Grid of `hysteresis`.
    pub hysteresis_values: Vec<Decimal>,
    /// Grid of `base_risk`.
    pub base_risks: Vec<Decimal>,
    /// Grid of `risk_increment`.
    pub risk_increments: Vec<Decimal>,
    /// Values of `stop_atr_multiplier` (a single fixed value; not perturbed).
    pub stop_atr_multipliers: Vec<Decimal>,
    /// Account policy cap: candidates whose level-3 risk exceeds it are never generated.
    #[serde(default = "default_max_level_risk")]
    pub max_level_risk: Decimal,
}

impl Default for SwarmPolicySpace {
    fn default() -> Self {
        Self {
            threshold_bases: vec![
                dec!(0.25),
                dec!(0.30),
                dec!(0.35),
                dec!(0.40),
                dec!(0.45),
                dec!(0.50),
                dec!(0.55),
            ],
            threshold_steps: vec![dec!(0.05), dec!(0.10), dec!(0.15)],
            hysteresis_values: vec![dec!(0.10), dec!(0.15), dec!(0.20), dec!(0.25), dec!(0.30)],
            base_risks: vec![dec!(0.005), dec!(0.010), dec!(0.015), dec!(0.020)],
            risk_increments: vec![dec!(0.005), dec!(0.010), dec!(0.015), dec!(0.020)],
            stop_atr_multipliers: vec![dec!(3.0)],
            max_level_risk: default_max_level_risk(),
        }
    }
}

impl SwarmPolicySpace {
    /// Whether a configuration is valid and within the level-3 risk cap.
    pub fn admits(&self, config: &PolicyConfig) -> bool {
        config.validate().is_ok() && config.top_level_risk() <= self.max_level_risk
    }
}

impl HyperparameterSpace for SwarmPolicySpace {
    type Config = PolicyConfig;

    /// The full grid, minus invalid candidates and those above `max_level_risk`; when that is
    /// more than `max_candidates`, a deterministic subsample (seed 42).
    fn generate_candidates(&self, max_candidates: usize) -> Vec<PolicyConfig> {
        let mut configs = Vec::new();
        for &threshold_base in &self.threshold_bases {
            for &threshold_step in &self.threshold_steps {
                for &hysteresis in &self.hysteresis_values {
                    for &base_risk in &self.base_risks {
                        for &risk_increment in &self.risk_increments {
                            for &stop_atr_multiplier in &self.stop_atr_multipliers {
                                let config = PolicyConfig {
                                    threshold_base,
                                    threshold_step,
                                    hysteresis,
                                    base_risk,
                                    risk_increment,
                                    stop_atr_multiplier,
                                };
                                if self.admits(&config) {
                                    configs.push(config);
                                }
                            }
                        }
                    }
                }
            }
        }

        if configs.len() > max_candidates {
            use rand::seq::SliceRandom;
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);
            configs.shuffle(&mut rng);
            configs.truncate(max_candidates);
        }

        configs
    }

    /// One step down and one step up the grid for each of the five searched parameters, the
    /// others held: up to 10 neighbours, keeping only admitted ones. A value off the grid steps
    /// to the nearest grid values around it.
    fn perturb_neighbors(&self, base: &PolicyConfig) -> Vec<PolicyConfig> {
        let mut neighbors = Vec::with_capacity(10);
        let push = |neighbors: &mut Vec<PolicyConfig>,
                    (below, above): (Option<Decimal>, Option<Decimal>),
                    set: &dyn Fn(Decimal) -> PolicyConfig| {
            for value in [below, above].into_iter().flatten() {
                let candidate = set(value);
                if self.admits(&candidate) {
                    neighbors.push(candidate);
                }
            }
        };
        push(
            &mut neighbors,
            adjacent_grid_values(&self.threshold_bases, base.threshold_base),
            &|threshold_base| PolicyConfig {
                threshold_base,
                ..base.clone()
            },
        );
        push(
            &mut neighbors,
            adjacent_grid_values(&self.threshold_steps, base.threshold_step),
            &|threshold_step| PolicyConfig {
                threshold_step,
                ..base.clone()
            },
        );
        push(
            &mut neighbors,
            adjacent_grid_values(&self.hysteresis_values, base.hysteresis),
            &|hysteresis| PolicyConfig {
                hysteresis,
                ..base.clone()
            },
        );
        push(
            &mut neighbors,
            adjacent_grid_values(&self.base_risks, base.base_risk),
            &|base_risk| PolicyConfig {
                base_risk,
                ..base.clone()
            },
        );
        push(
            &mut neighbors,
            adjacent_grid_values(&self.risk_increments, base.risk_increment),
            &|risk_increment| PolicyConfig {
                risk_increment,
                ..base.clone()
            },
        );
        neighbors
    }
}

/// Nearest grid values strictly below and strictly above `value` (same rule as the other
/// strategies' spaces).
fn adjacent_grid_values(grid: &[Decimal], value: Decimal) -> (Option<Decimal>, Option<Decimal>) {
    let below = grid.iter().copied().filter(|v| *v < value).max();
    let above = grid.iter().copied().filter(|v| *v > value).min();
    (below, above)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(timestamp: i64, long_net: i64, short_net: i64, atr: Decimal) -> ConsensusSample {
        // 100 agents, so a net of `x` votes is exactly x/100.
        let (long_up, long_down) = split(long_net);
        let (short_up, short_down) = split(short_net);
        ConsensusSample {
            timestamp,
            agents: 100,
            long_up,
            long_down,
            short_up,
            short_down,
            waits: 0,
            conflicts: 0,
            atr_stop_reference: atr,
            price: dec!(40000),
        }
    }

    fn split(net: i64) -> (u32, u32) {
        if net >= 0 {
            (net as u32, 0)
        } else {
            (0, (-net) as u32)
        }
    }

    fn worked_example_config() -> PolicyConfig {
        PolicyConfig {
            threshold_base: dec!(0.50),
            threshold_step: dec!(0.05),
            hysteresis: dec!(0.25),
            ..PolicyConfig::default()
        }
    }

    fn policy(config: PolicyConfig) -> ConsensusPolicy {
        ConsensusPolicy::new(config).expect("valid config")
    }

    fn emitted_level(decision: Option<Decision>) -> Option<i8> {
        decision.map(|d| match d {
            Decision::Target(t) => t.level(),
            Decision::Entry(_) => panic!("the policy only emits targets"),
        })
    }

    #[test]
    fn worked_example_of_section_18() {
        // (long_net, short_net) in votes of 100, then long_level, short_level, k, emitted.
        let rows: [(i64, i64, u8, u8, i8, Option<i8>); 7] = [
            (20, 0, 0, 0, 0, None),
            (52, 0, 1, 0, 1, Some(1)),
            (61, 0, 3, 0, 3, Some(3)),
            (40, 0, 3, 0, 3, None),
            (20, 10, 0, 0, 0, Some(0)),
            (-10, 52, 0, 1, -1, Some(-1)),
            (55, 55, 2, 2, 0, Some(0)),
        ];
        let mut p = policy(worked_example_config());
        for (i, (ln, sn, ll, sl, k, emitted)) in rows.into_iter().enumerate() {
            let out = emitted_level(p.on_sample(&sample(i as i64 + 1, ln, sn, dec!(500)), true));
            assert_eq!(p.long_level(), ll, "row {} long level", i + 1);
            assert_eq!(p.short_level(), sl, "row {} short level", i + 1);
            assert_eq!(p.last_level(), k, "row {} k", i + 1);
            assert_eq!(out, emitted, "row {} emission", i + 1);
        }
    }

    #[test]
    fn axis_jumps_from_zero_to_three() {
        let (l1, l2, l3) = (dec!(0.50), dec!(0.55), dec!(0.60));
        assert_eq!(axis_update(0, dec!(0.60), l1, l2, l3, dec!(0.25)), 3);
        assert_eq!(axis_update(0, dec!(0.99), l1, l2, l3, dec!(0.25)), 3);
    }

    #[test]
    fn axis_cascades_from_three_to_zero_in_one_update() {
        let (l1, l2, l3) = (dec!(0.50), dec!(0.55), dec!(0.60));
        assert_eq!(axis_update(3, dec!(0.20), l1, l2, l3, dec!(0.25)), 0);
        // Only partly: 0.32 < 0.35 (3→2) and < 0.30? no, so it stops at 2.
        assert_eq!(axis_update(3, dec!(0.32), l1, l2, l3, dec!(0.25)), 2);
        // 0.27 < 0.35 and < 0.30, not < 0.25: stops at 1.
        assert_eq!(axis_update(3, dec!(0.27), l1, l2, l3, dec!(0.25)), 1);
    }

    #[test]
    fn ascent_does_not_lower_and_boundaries_are_inclusive() {
        let (l1, l2, l3) = (dec!(0.50), dec!(0.55), dec!(0.60));
        let h = dec!(0.25);
        assert_eq!(axis_update(0, dec!(0.50), l1, l2, l3, h), 1);
        assert_eq!(axis_update(0, dec!(0.55), l1, l2, l3, h), 2);
        // At level 2, a net between L1 and L2 keeps level 2 (no ascent rule lowers it).
        assert_eq!(axis_update(2, dec!(0.51), l1, l2, l3, h), 2);
        // Exactly at L1 - H does not step down (strict `<`).
        assert_eq!(axis_update(1, dec!(0.25), l1, l2, l3, h), 1);
        assert_eq!(axis_update(1, dec!(0.24), l1, l2, l3, h), 0);
    }

    #[test]
    fn hysteresis_above_l1_keeps_level_one_until_the_net_turns_clearly_negative() {
        let config = PolicyConfig {
            threshold_base: dec!(0.25),
            threshold_step: dec!(0.05),
            hysteresis: dec!(0.30),
            ..PolicyConfig::default()
        };
        let mut p = policy(config);
        assert_eq!(
            emitted_level(p.on_sample(&sample(1, 25, 0, dec!(500)), true)),
            Some(1)
        );
        for (t, net) in [(2, 0), (3, -4), (4, -5)] {
            assert_eq!(p.on_sample(&sample(t, net, 0, dec!(500)), true), None);
            assert_eq!(p.long_level(), 1, "net {net}");
        }
        assert_eq!(
            emitted_level(p.on_sample(&sample(5, -6, 0, dec!(500)), true)),
            Some(0)
        );
        assert_eq!(p.long_level(), 0);
    }

    #[test]
    fn long_only_never_emits_a_negative_level() {
        let mut p = policy(worked_example_config());
        let nets = [(0, 90), (-50, 70), (10, 60), (70, 99), (-90, 99), (0, 55)];
        for (t, (ln, sn)) in nets.into_iter().enumerate() {
            let out = emitted_level(p.on_sample(&sample(t as i64, ln, sn, dec!(500)), false));
            assert_eq!(p.short_level(), 0);
            assert!(p.last_level() >= 0);
            if let Some(level) = out {
                assert!(level >= 0, "emitted {level}");
            }
        }
        // The same short votes with shorts allowed do go negative.
        let mut q = policy(worked_example_config());
        assert_eq!(
            emitted_level(q.on_sample(&sample(0, 0, 90, dec!(500)), true)),
            Some(-3)
        );
    }

    #[test]
    fn contradiction_between_axes_stands_aside() {
        let mut p = policy(worked_example_config());
        assert_eq!(
            emitted_level(p.on_sample(&sample(1, 70, 0, dec!(500)), true)),
            Some(3)
        );
        assert_eq!(
            emitted_level(p.on_sample(&sample(2, 70, 50, dec!(500)), true)),
            Some(0)
        );
        assert_eq!((p.long_level(), p.short_level()), (3, 1));
        // Both axes keep their state through the conflict: the short axis steps down and the
        // long one resumes.
        assert_eq!(
            emitted_level(p.on_sample(&sample(3, 70, 0, dec!(500)), true)),
            Some(3)
        );
    }

    #[test]
    fn emits_only_on_change() {
        let mut p = policy(worked_example_config());
        let mut emitted = Vec::new();
        for (t, ln) in [52, 53, 54, 52, 60, 61, 59, 40, 36].into_iter().enumerate() {
            if let Some(level) =
                emitted_level(p.on_sample(&sample(t as i64, ln, 0, dec!(500)), true))
            {
                emitted.push(level);
            }
        }
        // 52→1; 53, 54, 52 stay 1; 60→3; 61, 59, 40, 36 stay 3 (all ≥ L3 - H = 0.35).
        assert_eq!(emitted, vec![1, 3]);
    }

    #[test]
    fn target_carries_risk_and_stop_for_its_level() {
        let mut p = policy(worked_example_config());
        let Some(Decision::Target(t)) = p.on_sample(&sample(7, 52, 0, dec!(400)), true) else {
            panic!("expected a target");
        };
        assert_eq!(t.timestamp(), 7);
        assert_eq!(t.level(), 1);
        assert_eq!(t.risk_fraction(), dec!(0.010));
        assert_eq!(t.stop_distance(), dec!(1200));
        let Some(Decision::Target(t)) = p.on_sample(&sample(8, 0, 61, dec!(250)), true) else {
            panic!("expected a target");
        };
        // Long axis 1 → 0 (0 < 0.25), short axis 0 → 3.
        assert_eq!(t.level(), -3);
        assert_eq!(t.risk_fraction(), dec!(0.020));
        assert_eq!(t.stop_distance(), dec!(750));
        let Some(Decision::Target(t)) = p.on_sample(&sample(9, 0, 0, dec!(250)), true) else {
            panic!("expected a target");
        };
        assert_eq!(t.level(), 0);
        assert_eq!(t.risk_fraction(), Decimal::ZERO);
        assert_eq!(t.stop_distance(), Decimal::ZERO);
    }

    #[test]
    fn risk_fraction_per_level() {
        let c = PolicyConfig {
            base_risk: dec!(0.01),
            risk_increment: dec!(0.02),
            ..PolicyConfig::default()
        };
        assert_eq!(c.risk_fraction(0), Decimal::ZERO);
        assert_eq!(c.risk_fraction(1), dec!(0.01));
        assert_eq!(c.risk_fraction(2), dec!(0.03));
        assert_eq!(c.risk_fraction(3), dec!(0.05));
        assert_eq!(c.top_level_risk(), dec!(0.05));
    }

    #[test]
    fn non_positive_atr_withholds_a_non_zero_target_until_a_valid_one() {
        let mut p = policy(worked_example_config());
        assert_eq!(p.on_sample(&sample(1, 61, 0, Decimal::ZERO), true), None);
        assert_eq!(p.long_level(), 3);
        assert_eq!(p.last_level(), 0);
        assert_eq!(p.on_sample(&sample(2, 61, 0, dec!(-1)), true), None);
        assert_eq!(
            emitted_level(p.on_sample(&sample(3, 61, 0, dec!(100)), true)),
            Some(3)
        );
        // A return to flat needs no ATR.
        assert_eq!(
            emitted_level(p.on_sample(&sample(4, 0, 0, Decimal::ZERO), true)),
            Some(0)
        );
    }

    #[test]
    fn reset_equals_fresh() {
        let mut p = policy(worked_example_config());
        p.on_sample(&sample(1, 61, 0, dec!(100)), true);
        p.reset();
        assert_eq!((p.long_level(), p.short_level(), p.last_level()), (0, 0, 0));
        assert_eq!(
            emitted_level(p.on_sample(&sample(2, 52, 0, dec!(100)), true)),
            Some(1)
        );
    }

    #[test]
    fn default_config_is_valid_and_displays_compactly() {
        let c = PolicyConfig::default();
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(
            c.to_string(),
            "B:0.50|S:0.05|H:0.25|R:0.010|Ri:0.005|Stop:3.0"
        );
        let json = serde_json::to_string(&c).expect("serialise");
        let back: PolicyConfig = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, c);
    }

    #[test]
    fn validation_rejects_each_invalid_field() {
        let d = PolicyConfig::default;
        let cases = [
            (
                PolicyConfig {
                    threshold_base: Decimal::ZERO,
                    ..d()
                },
                PolicyConfigError::NonPositiveThresholdBase(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    threshold_step: dec!(-0.05),
                    ..d()
                },
                PolicyConfigError::NonPositiveThresholdStep(dec!(-0.05)),
            ),
            (
                PolicyConfig {
                    threshold_base: dec!(0.80),
                    threshold_step: dec!(0.15),
                    ..d()
                },
                PolicyConfigError::TopThresholdAboveOne(dec!(1.10)),
            ),
            (
                PolicyConfig {
                    hysteresis: dec!(-0.01),
                    ..d()
                },
                PolicyConfigError::NegativeHysteresis(dec!(-0.01)),
            ),
            (
                PolicyConfig {
                    base_risk: Decimal::ZERO,
                    ..d()
                },
                PolicyConfigError::NonPositiveBaseRisk(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    risk_increment: Decimal::ZERO,
                    ..d()
                },
                PolicyConfigError::NonPositiveRiskIncrement(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    base_risk: dec!(0.5),
                    risk_increment: dec!(0.3),
                    ..d()
                },
                PolicyConfigError::TopRiskAboveOne(dec!(1.1)),
            ),
            (
                PolicyConfig {
                    stop_atr_multiplier: Decimal::ZERO,
                    ..d()
                },
                PolicyConfigError::NonPositiveStopMultiplier(Decimal::ZERO),
            ),
        ];
        for (config, expected) in cases {
            assert_eq!(config.validate(), Err(expected.clone()), "{config}");
            assert_eq!(ConsensusPolicy::new(config).err(), Some(expected));
        }
        // Edge cases that are allowed: L3 exactly 1, H = 0, H > L1.
        for ok in [
            PolicyConfig {
                threshold_base: dec!(0.70),
                threshold_step: dec!(0.15),
                ..d()
            },
            PolicyConfig {
                hysteresis: Decimal::ZERO,
                ..d()
            },
            PolicyConfig {
                threshold_base: dec!(0.25),
                hysteresis: dec!(0.30),
                ..d()
            },
        ] {
            assert_eq!(ok.validate(), Ok(()), "{ok}");
        }
    }

    fn on_grid(space: &SwarmPolicySpace, c: &PolicyConfig) -> bool {
        space.threshold_bases.contains(&c.threshold_base)
            && space.threshold_steps.contains(&c.threshold_step)
            && space.hysteresis_values.contains(&c.hysteresis)
            && space.base_risks.contains(&c.base_risk)
            && space.risk_increments.contains(&c.risk_increment)
            && space.stop_atr_multipliers.contains(&c.stop_atr_multiplier)
    }

    #[test]
    fn grid_count_after_the_level_risk_filter_is_pinned() {
        // Risk pairs with base_risk + 2*risk_increment <= 0.05:
        //   increment 0.005, 0.010, 0.015 → all 4 base risks (top 0.030, 0.040, 0.050);
        //   increment 0.020 → base 0.005, 0.010 only (0.045, 0.050; 0.055 and 0.060 exceed).
        // 4 + 4 + 4 + 2 = 14 pairs; thresholds never exceed L3 = 0.55 + 0.30 = 0.85 ≤ 1.
        // 7 bases · 3 steps · 5 hysteresis · 14 risk pairs · 1 stop = 1,470 of 1,680.
        let space = SwarmPolicySpace::default();
        let all = space.generate_candidates(usize::MAX);
        assert_eq!(all.len(), 1_470);
        assert!(all.iter().all(|c| c.top_level_risk() <= dec!(0.05)));
        assert!(all.contains(&PolicyConfig::default()));
    }

    #[test]
    fn candidates_are_on_the_grid_valid_and_deterministic() {
        let space = SwarmPolicySpace::default();
        let all = space.generate_candidates(usize::MAX);
        assert!(all.iter().all(|c| on_grid(&space, c) && space.admits(c)));
        let a = space.generate_candidates(100);
        let b = space.generate_candidates(100);
        assert_eq!(a.len(), 100);
        assert_eq!(a, b);
        assert!(a.iter().all(|c| all.contains(c)));
        assert_ne!(a[..], all[..100], "the subsample is shuffled, not a prefix");
    }

    #[test]
    fn neighbours_are_on_the_grid_valid_and_adjacent() {
        let space = SwarmPolicySpace::default();
        for c in space.generate_candidates(usize::MAX) {
            for n in space.perturb_neighbors(&c) {
                assert!(on_grid(&space, &n) && space.admits(&n), "{n} from {c}");
                assert_ne!(n, c);
                assert_eq!(n.stop_atr_multiplier, c.stop_atr_multiplier);
            }
        }
        // Interior point: 2 neighbours per parameter, except the risk ones the cap removes.
        let base = PolicyConfig {
            threshold_base: dec!(0.40),
            threshold_step: dec!(0.10),
            hysteresis: dec!(0.20),
            base_risk: dec!(0.010),
            risk_increment: dec!(0.010),
            stop_atr_multiplier: dec!(3.0),
        };
        let n = space.perturb_neighbors(&base);
        assert_eq!(n.len(), 10);
        assert!(n.contains(&PolicyConfig {
            threshold_base: dec!(0.35),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            risk_increment: dec!(0.015),
            ..base.clone()
        }));
        // At the risk cap, the step up in either risk parameter is filtered out.
        let capped = PolicyConfig {
            base_risk: dec!(0.010),
            risk_increment: dec!(0.020),
            ..base.clone()
        };
        let n = space.perturb_neighbors(&capped);
        assert_eq!(n.len(), 8);
        assert!(n.iter().all(|c| c.top_level_risk() <= dec!(0.05)));
    }

    #[test]
    fn space_round_trips_and_defaults_the_cap() {
        let space = SwarmPolicySpace::default();
        let json = serde_json::to_string(&space).expect("serialise");
        assert_eq!(
            serde_json::from_str::<SwarmPolicySpace>(&json).expect("deserialise"),
            space
        );
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("value");
        value
            .as_object_mut()
            .expect("object")
            .remove("max_level_risk");
        let back: SwarmPolicySpace = serde_json::from_value(value).expect("deserialise");
        assert_eq!(back.max_level_risk, dec!(0.05));
    }
}
