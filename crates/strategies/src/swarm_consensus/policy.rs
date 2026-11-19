//! The consensus policy (Stage B) of the swarm consensus strategy: a classifier that turns the
//! net vote of a sample into a triple-barrier entry, and its HPO search space (strategy spec
//! `docs/strategy/03-swarm-consensus.md` §10 and §14.2).

use domain::{OrderIntent, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use super::genome::SwarmRng;
use super::sample::{ConsensusSample, FeedWeights, FeedWeightsError};
use crate::strategy::HyperparameterSpace;

/// Largest stop distance a configuration may ask for, in ATR multiples.
const MAX_STOP_ATR_MULTIPLIER: Decimal = dec!(2.00);
/// Smallest take profit a configuration may ask for, in multiples of its stop.
const MIN_PAYOFF: Decimal = dec!(1.5);
/// Shortest and longest time barrier, in seconds (2 h and 24 h).
const DECAY_HORIZON_SECONDS: std::ops::RangeInclusive<u64> = 7200..=86400;

/// Reasons a [`PolicyConfig`] is rejected (strategy spec §10.1).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyConfigError {
    /// `threshold_base` must be strictly positive.
    #[error("threshold_base must be > 0, got {0}")]
    NonPositiveThresholdBase(Decimal),
    /// `threshold_base` is a fraction of the swarm, so it must not exceed 1.
    #[error("threshold_base must be <= 1, got {0}")]
    ThresholdAboveOne(Decimal),
    /// `base_risk` must be strictly positive.
    #[error("base_risk must be > 0, got {0}")]
    NonPositiveBaseRisk(Decimal),
    /// `risk_increment` must not be negative.
    #[error("risk_increment must be >= 0, got {0}")]
    NegativeRiskIncrement(Decimal),
    /// The largest risk `base_risk + risk_increment` must not exceed 1.
    #[error("the largest risk (base_risk + risk_increment) must be <= 1, got {0}")]
    MaxRiskAboveOne(Decimal),
    /// `stop_atr_multiplier` must be strictly positive.
    #[error("stop_atr_multiplier must be > 0, got {0}")]
    NonPositiveStopMultiplier(Decimal),
    /// `stop_atr_multiplier` must not exceed 2.00 ATR.
    #[error("stop ATR multiplier must be <= 2.00, got {0}")]
    ExcessiveStop(Decimal),
    /// `take_profit_atr_multiplier` must be strictly positive.
    #[error("take_profit_atr_multiplier must be > 0, got {0}")]
    NonPositiveTakeProfitMultiplier(Decimal),
    /// `take_profit_atr_multiplier` must be at least 1.5x `stop_atr_multiplier`.
    #[error("take profit multiplier {0} must be >= 1.5x stop multiplier {1}")]
    InvertedPayoff(Decimal, Decimal),
    /// `breakeven_atr_multiplier` must be strictly positive when set.
    #[error("breakeven_atr_multiplier must be > 0, got {0}")]
    NonPositiveBreakevenMultiplier(Decimal),
    /// `breakeven_atr_multiplier` must be strictly less than `take_profit_atr_multiplier`.
    #[error("breakeven multiplier {0} must be < take profit multiplier {1}")]
    BreakevenAboveTakeProfit(Decimal, Decimal),
    /// `trailing_atr_multiplier` must be strictly positive when set.
    #[error("trailing_atr_multiplier must be > 0, got {0}")]
    NonPositiveTrailingMultiplier(Decimal),
    /// `decay_horizon_seconds` must lie between 7200 s (2 h) and 86400 s (24 h).
    #[error("decay horizon must be between 7200s (2h) and 86400s (24h), got {0}")]
    InvalidDecayHorizon(u64),
    /// `feed_weights` is invalid.
    #[error("invalid feed_weights: {0}")]
    InvalidFeedWeights(#[from] FeedWeightsError),
    /// `macro_slope_min` must be non-negative.
    #[error("macro_slope_min must be >= 0, got {0}")]
    NegativeMacroSlopeMin(Decimal),
    /// `short_threshold_delta` must be non-negative.
    #[error("short_threshold_delta must be >= 0, got {0}")]
    NegativeShortThresholdDelta(Decimal),
    /// The configuration was written for the level machine this policy no longer has.
    #[error(
        "this configuration was searched for the removed target-level policy \
         (classifier_mode = false); it cannot be run, search a new one"
    )]
    RemovedTargetMode,
}

/// Parameters of the consensus policy, searched by HPO (strategy spec §14.2).
///
/// The threshold is a fraction of the swarm size `N`; risks are fractions of equity; distances
/// are multiples of the ATR of the stop reference feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StoredPolicyConfig")]
pub struct PolicyConfig {
    /// Net vote a side needs to enter.
    pub threshold_base: Decimal,
    /// Risk fraction of an entry whose net vote just reaches the threshold.
    pub base_risk: Decimal,
    /// Extra risk fraction of an entry with a unanimous swarm; in between it grows linearly.
    pub risk_increment: Decimal,
    /// Stop distance in multiples of the ATR.
    pub stop_atr_multiplier: Decimal,
    /// Take-profit distance in multiples of the ATR.
    pub take_profit_atr_multiplier: Decimal,
    /// Optional breakeven ATR multiplier (favorable excursion to move SL to entry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakeven_atr_multiplier: Option<Decimal>,
    /// Optional trailing step ATR multiplier (distance per trailing ratchet step).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailing_atr_multiplier: Option<Decimal>,
    /// Time barrier: seconds after the entry at which the position is closed.
    pub decay_horizon_seconds: u64,
    /// Optional feed weights for heterogeneous consensus voting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feed_weights: Option<FeedWeights>,
    /// Whether to require trend alignment with the higher-timeframe feeds. Long entries require
    /// `macro_trend_slope >= macro_slope_min`, short entries `macro_trend_slope <=
    /// -macro_slope_min`, and the two-hour slope must not point the other way.
    pub macro_trend_filter: bool,
    /// Minimum higher-timeframe ALMA slope required to enter. When `|macro_trend_slope| <
    /// macro_slope_min`, the market is in choppy consolidation and entries are withheld.
    pub macro_slope_min: Decimal,
    /// Additional net vote a short entry needs (`theta_short = theta_long + delta`).
    pub short_threshold_delta: Decimal,
}

/// [`PolicyConfig`] as files store it, including files written before the target-level policy
/// was removed: their `classifier_mode` flag is checked, and the fields that only that policy
/// read (`threshold_step`, `hysteresis`, `bracket_priority`) are ignored.
#[derive(Deserialize)]
struct StoredPolicyConfig {
    threshold_base: Decimal,
    base_risk: Decimal,
    risk_increment: Decimal,
    stop_atr_multiplier: Decimal,
    take_profit_atr_multiplier: Decimal,
    #[serde(default)]
    breakeven_atr_multiplier: Option<Decimal>,
    #[serde(default)]
    trailing_atr_multiplier: Option<Decimal>,
    decay_horizon_seconds: u64,
    #[serde(default)]
    feed_weights: Option<FeedWeights>,
    #[serde(default)]
    macro_trend_filter: bool,
    #[serde(default)]
    macro_slope_min: Decimal,
    #[serde(default)]
    short_threshold_delta: Decimal,
    #[serde(default)]
    classifier_mode: Option<bool>,
}

impl TryFrom<StoredPolicyConfig> for PolicyConfig {
    type Error = PolicyConfigError;

    fn try_from(stored: StoredPolicyConfig) -> Result<Self, Self::Error> {
        if stored.classifier_mode == Some(false) {
            return Err(PolicyConfigError::RemovedTargetMode);
        }
        Ok(Self {
            threshold_base: stored.threshold_base,
            base_risk: stored.base_risk,
            risk_increment: stored.risk_increment,
            stop_atr_multiplier: stored.stop_atr_multiplier,
            take_profit_atr_multiplier: stored.take_profit_atr_multiplier,
            breakeven_atr_multiplier: stored.breakeven_atr_multiplier,
            trailing_atr_multiplier: stored.trailing_atr_multiplier,
            decay_horizon_seconds: stored.decay_horizon_seconds,
            feed_weights: stored.feed_weights,
            macro_trend_filter: stored.macro_trend_filter,
            macro_slope_min: stored.macro_slope_min,
            short_threshold_delta: stored.short_threshold_delta,
        })
    }
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            threshold_base: dec!(0.45),
            base_risk: dec!(0.010),
            risk_increment: dec!(0.010),
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: dec!(2.0),
            breakeven_atr_multiplier: Some(dec!(0.75)),
            trailing_atr_multiplier: Some(dec!(0.50)),
            decay_horizon_seconds: 14400,
            feed_weights: None,
            macro_trend_filter: false,
            macro_slope_min: Decimal::ZERO,
            short_threshold_delta: Decimal::ZERO,
        }
    }
}

impl std::fmt::Display for PolicyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "B:{}|R:{}|Ri:{}|Stop:{}|TP:{}",
            self.threshold_base,
            self.base_risk,
            self.risk_increment,
            self.stop_atr_multiplier,
            self.take_profit_atr_multiplier
        )?;
        if let Some(be) = self.breakeven_atr_multiplier {
            write!(f, "|BE:{be}")?;
        }
        if let Some(tr) = self.trailing_atr_multiplier {
            write!(f, "|Trail:{tr}")?;
        }
        write!(f, "|Decay:{}s", self.decay_horizon_seconds)?;
        if let Some(ref weights) = self.feed_weights {
            write!(f, "|W:{weights}")?;
        }
        if self.macro_trend_filter {
            write!(f, "|Macro")?;
            if self.macro_slope_min > Decimal::ZERO {
                write!(f, ":{}", self.macro_slope_min)?;
            }
        }
        if self.short_threshold_delta > Decimal::ZERO {
            write!(f, "|ShortDelta:{}", self.short_threshold_delta)?;
        }
        Ok(())
    }
}

impl PolicyConfig {
    /// The largest risk fraction an entry may ask for: that of a unanimous swarm.
    pub fn max_risk(&self) -> Decimal {
        self.base_risk + self.risk_increment
    }

    /// Risk fraction of an entry decided at net vote `conviction` against threshold `theta`:
    /// `base_risk` at the threshold, rising linearly to [`PolicyConfig::max_risk`] at a
    /// unanimous swarm (strategy spec §10.4).
    pub fn risk_fraction(&self, conviction: Decimal, theta: Decimal) -> Decimal {
        let span = Decimal::ONE - theta;
        let share = if span > Decimal::ZERO {
            ((conviction - theta) / span)
                .max(Decimal::ZERO)
                .min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };
        self.base_risk + share * self.risk_increment
    }

    /// Checks the rules of strategy spec §10.1.
    pub fn validate(&self) -> Result<(), PolicyConfigError> {
        if self.threshold_base <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveThresholdBase(
                self.threshold_base,
            ));
        }
        if self.threshold_base > Decimal::ONE {
            return Err(PolicyConfigError::ThresholdAboveOne(self.threshold_base));
        }
        if self.base_risk <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveBaseRisk(self.base_risk));
        }
        if self.risk_increment < Decimal::ZERO {
            return Err(PolicyConfigError::NegativeRiskIncrement(
                self.risk_increment,
            ));
        }
        if self.max_risk() > Decimal::ONE {
            return Err(PolicyConfigError::MaxRiskAboveOne(self.max_risk()));
        }
        let (stop, take_profit) = (self.stop_atr_multiplier, self.take_profit_atr_multiplier);
        if stop <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveStopMultiplier(stop));
        }
        if stop > MAX_STOP_ATR_MULTIPLIER {
            return Err(PolicyConfigError::ExcessiveStop(stop));
        }
        if take_profit <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveTakeProfitMultiplier(
                take_profit,
            ));
        }
        if take_profit < MIN_PAYOFF * stop {
            return Err(PolicyConfigError::InvertedPayoff(take_profit, stop));
        }
        if let Some(be) = self.breakeven_atr_multiplier {
            if be <= Decimal::ZERO {
                return Err(PolicyConfigError::NonPositiveBreakevenMultiplier(be));
            }
            if be >= take_profit {
                return Err(PolicyConfigError::BreakevenAboveTakeProfit(be, take_profit));
            }
        }
        if let Some(tr) = self.trailing_atr_multiplier {
            if tr <= Decimal::ZERO {
                return Err(PolicyConfigError::NonPositiveTrailingMultiplier(tr));
            }
        }
        if !DECAY_HORIZON_SECONDS.contains(&self.decay_horizon_seconds) {
            return Err(PolicyConfigError::InvalidDecayHorizon(
                self.decay_horizon_seconds,
            ));
        }
        if let Some(ref weights) = self.feed_weights {
            weights.validate()?;
        }
        if self.macro_slope_min < Decimal::ZERO {
            return Err(PolicyConfigError::NegativeMacroSlopeMin(
                self.macro_slope_min,
            ));
        }
        if self.short_threshold_delta < Decimal::ZERO {
            return Err(PolicyConfigError::NegativeShortThresholdDelta(
                self.short_threshold_delta,
            ));
        }
        Ok(())
    }
}

/// The consensus policy: enters the side whose net vote clears its threshold while the other
/// side's does not, once per signal (strategy spec §10.3–10.6).
#[derive(Debug, Clone)]
pub struct ConsensusPolicy {
    config: PolicyConfig,
    /// Side of the signal already acted on; cleared when no side clears its threshold.
    in_signal_side: Option<Side>,
}

impl ConsensusPolicy {
    /// Builds a policy with no signal from a validated configuration.
    pub fn new(config: PolicyConfig) -> Result<Self, PolicyConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            in_signal_side: None,
        })
    }

    /// The configuration in use.
    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    /// Side of the signal the policy last entered and that still holds, if any.
    pub fn signal_side(&self) -> Option<Side> {
        self.in_signal_side
    }

    /// The side the sample's votes select, with its net vote and threshold. `None` when no
    /// side clears its threshold, both do, or the macro trend filter withholds the entry.
    fn candidate(
        &self,
        sample: &ConsensusSample,
        allow_short: bool,
    ) -> Option<(Side, Decimal, Decimal)> {
        let (long_net, short_net) = match &self.config.feed_weights {
            Some(weights) => (
                sample.weighted_long_net(weights),
                sample.weighted_short_net(weights),
            ),
            None => (sample.long_net(), sample.short_net()),
        };
        let theta_long = self.config.threshold_base;
        let theta_short = self.config.threshold_base + self.config.short_threshold_delta;
        let filter = self.config.macro_trend_filter;
        let slope_min = self.config.macro_slope_min;

        if long_net >= theta_long && short_net < theta_long {
            let against_trend =
                sample.macro_trend_slope < slope_min || sample.macro_trend_slope_2h < Decimal::ZERO;
            (!(filter && against_trend)).then_some((Side::Buy, long_net, theta_long))
        } else if allow_short && short_net >= theta_short && long_net < theta_short {
            let against_trend = sample.macro_trend_slope > -slope_min
                || sample.macro_trend_slope_2h > Decimal::ZERO;
            (!(filter && against_trend)).then_some((Side::Sell, short_net, theta_short))
        } else {
            None
        }
    }

    /// Feeds one sample and returns a triple-barrier entry when a side's signal starts
    /// (strategy spec §10.6).
    ///
    /// A signal is acted on once: while the same side keeps clearing its threshold nothing more
    /// is emitted, and the signal ends when no side is selected. A sample without a positive
    /// ATR or price carries no valid barrier distances and is skipped whole.
    pub fn on_sample(
        &mut self,
        sample: &ConsensusSample,
        allow_short: bool,
    ) -> Option<OrderIntent> {
        if sample.atr_stop_reference <= Decimal::ZERO || sample.price <= Decimal::ZERO {
            return None;
        }
        let Some((side, conviction, theta)) = self.candidate(sample, allow_short) else {
            self.in_signal_side = None;
            return None;
        };
        if self.in_signal_side == Some(side) {
            return None;
        }

        let atr = sample.atr_stop_reference;
        let stop_distance = self.config.stop_atr_multiplier * atr;
        let take_profit_distance = self.config.take_profit_atr_multiplier * atr;
        let (stop_loss, take_profit) = match side {
            Side::Buy => (
                sample.price - stop_distance,
                sample.price + take_profit_distance,
            ),
            Side::Sell => (
                sample.price + stop_distance,
                sample.price - take_profit_distance,
            ),
        };
        let horizon_ms = i64::try_from(self.config.decay_horizon_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1000))?;

        let intent = OrderIntent::with_risk_and_exits(
            sample.timestamp,
            side,
            sample.price,
            stop_loss,
            take_profit,
            horizon_ms,
            Some(self.config.risk_fraction(conviction, theta)),
            self.config.trailing_atr_multiplier.map(|m| m * atr),
            self.config.breakeven_atr_multiplier.map(|m| m * atr),
        )
        .ok()?;

        self.in_signal_side = Some(side);
        Some(intent)
    }

    /// Returns to the initial state, keeping the configuration.
    pub fn reset(&mut self) {
        self.in_signal_side = None;
    }
}

fn default_max_risk() -> Decimal {
    dec!(0.05)
}

fn standard_feed_weights_options() -> Vec<Option<FeedWeights>> {
    let mut profiles: Vec<Option<FeedWeights>> = vec![None];
    profiles.extend(FeedWeights::standard_profiles().into_iter().map(Some));
    profiles
}

fn unweighted() -> Vec<Option<FeedWeights>> {
    vec![None]
}

fn no_filter() -> Vec<bool> {
    vec![false]
}

fn zero() -> Vec<Decimal> {
    vec![Decimal::ZERO]
}

fn unset() -> Vec<Option<Decimal>> {
    vec![None]
}

/// HPO search space of [`PolicyConfig`] (strategy spec §14.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmPolicySpace {
    /// Grid of `threshold_base`.
    pub threshold_bases: Vec<Decimal>,
    /// Grid of `base_risk`.
    pub base_risks: Vec<Decimal>,
    /// Grid of `risk_increment`.
    pub risk_increments: Vec<Decimal>,
    /// Values of `stop_atr_multiplier`.
    pub stop_atr_multipliers: Vec<Decimal>,
    /// Values of `take_profit_atr_multiplier`.
    pub take_profit_atr_multipliers: Vec<Decimal>,
    /// Values of `breakeven_atr_multiplier` (`None` for no breakeven).
    #[serde(default = "unset")]
    pub breakeven_atr_multipliers: Vec<Option<Decimal>>,
    /// Values of `trailing_atr_multiplier` (`None` for no trailing ratchet).
    #[serde(default = "unset")]
    pub trailing_atr_multipliers: Vec<Option<Decimal>>,
    /// Values of `feed_weights` (`None` for equal weights).
    #[serde(default = "unweighted")]
    pub feed_weights_options: Vec<Option<FeedWeights>>,
    /// Values of `macro_trend_filter`.
    #[serde(default = "no_filter")]
    pub macro_trend_filter_options: Vec<bool>,
    /// Values of `macro_slope_min`.
    #[serde(default = "zero")]
    pub macro_slope_min_options: Vec<Decimal>,
    /// Values of `short_threshold_delta`.
    #[serde(default = "zero")]
    pub short_threshold_delta_options: Vec<Decimal>,
    /// Values of `decay_horizon_seconds`.
    pub decay_horizon_options: Vec<u64>,
    /// Account policy cap: candidates whose largest risk exceeds it are never generated.
    #[serde(default = "default_max_risk", alias = "max_level_risk")]
    pub max_risk: Decimal,
}

impl Default for SwarmPolicySpace {
    fn default() -> Self {
        Self {
            threshold_bases: vec![dec!(0.475), dec!(0.50), dec!(0.525), dec!(0.55)],
            base_risks: vec![dec!(0.010)],
            risk_increments: vec![dec!(0.0)],
            stop_atr_multipliers: vec![dec!(1.0), dec!(1.25), dec!(1.50)],
            take_profit_atr_multipliers: vec![dec!(3.5), dec!(4.0), dec!(4.5)],
            breakeven_atr_multipliers: vec![Some(dec!(1.0)), Some(dec!(1.25)), Some(dec!(1.50))],
            trailing_atr_multipliers: vec![Some(dec!(1.0)), Some(dec!(1.25)), Some(dec!(1.50))],
            feed_weights_options: standard_feed_weights_options(),
            macro_trend_filter_options: vec![true],
            macro_slope_min_options: vec![dec!(0.02), dec!(0.04), dec!(0.06)],
            short_threshold_delta_options: vec![dec!(0.0)],
            decay_horizon_options: vec![86400],
            max_risk: default_max_risk(),
        }
    }
}

impl SwarmPolicySpace {
    /// Whether a configuration is valid and within the risk cap.
    pub fn admits(&self, config: &PolicyConfig) -> bool {
        config.validate().is_ok() && config.max_risk() <= self.max_risk
    }

    /// Enables exploration of standard feed weighting and pruning profiles.
    pub fn with_standard_feed_profiles(mut self) -> Self {
        self.feed_weights_options = standard_feed_weights_options();
        self
    }

    /// Every configuration of the grid, in grid order, admitted or not.
    fn grid(&self) -> Vec<PolicyConfig> {
        let mut configs = vec![];
        for &threshold_base in &self.threshold_bases {
            for &base_risk in &self.base_risks {
                for &risk_increment in &self.risk_increments {
                    for &stop_atr_multiplier in &self.stop_atr_multipliers {
                        configs.push(PolicyConfig {
                            threshold_base,
                            base_risk,
                            risk_increment,
                            stop_atr_multiplier,
                            ..PolicyConfig::default()
                        });
                    }
                }
            }
        }
        let configs = expand(configs, &self.take_profit_atr_multipliers, |c, &v| {
            c.take_profit_atr_multiplier = v
        });
        let configs = expand(configs, &self.breakeven_atr_multipliers, |c, &v| {
            c.breakeven_atr_multiplier = v
        });
        let configs = expand(configs, &self.trailing_atr_multipliers, |c, &v| {
            c.trailing_atr_multiplier = v
        });
        let configs = expand(configs, &self.macro_trend_filter_options, |c, &v| {
            c.macro_trend_filter = v
        });
        let configs = expand(configs, &self.macro_slope_min_options, |c, &v| {
            c.macro_slope_min = v
        });
        let configs = expand(configs, &self.short_threshold_delta_options, |c, &v| {
            c.short_threshold_delta = v
        });
        let configs = expand(configs, &self.feed_weights_options, |c, v| {
            c.feed_weights = v.clone()
        });
        expand(configs, &self.decay_horizon_options, |c, &v| {
            c.decay_horizon_seconds = v
        })
    }
}

/// Every configuration of `configs` with every value of `values` set by `set`: for each
/// configuration in order, the values in order (a nested loop with `values` innermost).
fn expand<T>(
    configs: Vec<PolicyConfig>,
    values: &[T],
    set: impl Fn(&mut PolicyConfig, &T),
) -> Vec<PolicyConfig> {
    let mut expanded = Vec::with_capacity(configs.len() * values.len());
    for config in configs {
        for value in values {
            let mut next = config.clone();
            set(&mut next, value);
            expanded.push(next);
        }
    }
    expanded
}

impl HyperparameterSpace for SwarmPolicySpace {
    type Config = PolicyConfig;

    /// The full grid, minus invalid candidates and those above `max_risk`; when that is more
    /// than `max_candidates`, a deterministic subsample (seed 42).
    fn generate_candidates(&self, max_candidates: usize) -> Vec<PolicyConfig> {
        let mut configs: Vec<PolicyConfig> = self
            .grid()
            .into_iter()
            .filter(|config| self.admits(config))
            .collect();

        if configs.len() > max_candidates {
            use rand::seq::SliceRandom;
            use rand::SeedableRng;
            let mut rng = SwarmRng::seed_from_u64(42);
            configs.shuffle(&mut rng);
            configs.truncate(max_candidates);
        }

        configs
    }

    /// One step down and one step up the grid for each of the searched parameters, the
    /// others held, keeping only admitted ones. A value off the grid steps
    /// to the nearest grid values around it.
    fn perturb_neighbors(&self, base: &PolicyConfig) -> Vec<PolicyConfig> {
        let mut neighbors = Vec::with_capacity(18);
        let mut push = |candidate: PolicyConfig| {
            if self.admits(&candidate) {
                neighbors.push(candidate);
            }
        };
        let around = |(below, above): (Option<Decimal>, Option<Decimal>)| {
            [below, above].into_iter().flatten()
        };
        for threshold_base in around(adjacent_grid_values(
            &self.threshold_bases,
            base.threshold_base,
        )) {
            push(PolicyConfig {
                threshold_base,
                ..base.clone()
            });
        }
        for base_risk in around(adjacent_grid_values(&self.base_risks, base.base_risk)) {
            push(PolicyConfig {
                base_risk,
                ..base.clone()
            });
        }
        for risk_increment in around(adjacent_grid_values(
            &self.risk_increments,
            base.risk_increment,
        )) {
            push(PolicyConfig {
                risk_increment,
                ..base.clone()
            });
        }
        for stop_atr_multiplier in around(adjacent_grid_values(
            &self.stop_atr_multipliers,
            base.stop_atr_multiplier,
        )) {
            push(PolicyConfig {
                stop_atr_multiplier,
                ..base.clone()
            });
        }
        for take_profit_atr_multiplier in around(adjacent_grid_values(
            &self.take_profit_atr_multipliers,
            base.take_profit_atr_multiplier,
        )) {
            push(PolicyConfig {
                take_profit_atr_multiplier,
                ..base.clone()
            });
        }
        if self.breakeven_atr_multipliers.len() > 1 {
            let (below, above) = adjacent_option_grid_values(
                &self.breakeven_atr_multipliers,
                base.breakeven_atr_multiplier,
            );
            for breakeven_atr_multiplier in [below, above].into_iter().flatten() {
                push(PolicyConfig {
                    breakeven_atr_multiplier,
                    ..base.clone()
                });
            }
        }
        if self.trailing_atr_multipliers.len() > 1 {
            let (below, above) = adjacent_option_grid_values(
                &self.trailing_atr_multipliers,
                base.trailing_atr_multiplier,
            );
            for trailing_atr_multiplier in [below, above].into_iter().flatten() {
                push(PolicyConfig {
                    trailing_atr_multiplier,
                    ..base.clone()
                });
            }
        }
        if self.decay_horizon_options.len() > 1 {
            let horizon = base.decay_horizon_seconds;
            let below = self
                .decay_horizon_options
                .iter()
                .copied()
                .filter(|v| *v < horizon)
                .max();
            let above = self
                .decay_horizon_options
                .iter()
                .copied()
                .filter(|v| *v > horizon)
                .min();
            for decay_horizon_seconds in [below, above].into_iter().flatten() {
                push(PolicyConfig {
                    decay_horizon_seconds,
                    ..base.clone()
                });
            }
        }
        if self.feed_weights_options.len() > 1 {
            let index = self
                .feed_weights_options
                .iter()
                .position(|option| option == &base.feed_weights);
            let below = index
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| self.feed_weights_options.get(i))
                .cloned();
            let above = index
                .and_then(|i| self.feed_weights_options.get(i + 1))
                .cloned();
            for feed_weights in [below, above].into_iter().flatten() {
                push(PolicyConfig {
                    feed_weights,
                    ..base.clone()
                });
            }
        }
        if self.macro_trend_filter_options.len() > 1 {
            push(PolicyConfig {
                macro_trend_filter: !base.macro_trend_filter,
                ..base.clone()
            });
        }
        if self.macro_slope_min_options.len() > 1 {
            for macro_slope_min in around(adjacent_grid_values(
                &self.macro_slope_min_options,
                base.macro_slope_min,
            )) {
                push(PolicyConfig {
                    macro_slope_min,
                    ..base.clone()
                });
            }
        }
        if self.short_threshold_delta_options.len() > 1 {
            for short_threshold_delta in around(adjacent_grid_values(
                &self.short_threshold_delta_options,
                base.short_threshold_delta,
            )) {
                push(PolicyConfig {
                    short_threshold_delta,
                    ..base.clone()
                });
            }
        }
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

/// Nearest grid values strictly below and strictly above `value` for an option grid where None
/// is considered the lowest element and Some(v) are ordered by value.
fn adjacent_option_grid_values<T: Copy + Ord>(
    grid: &[Option<T>],
    value: Option<T>,
) -> (Option<Option<T>>, Option<Option<T>>) {
    let below = match value {
        None => None,
        Some(v) => {
            let lower_somes = grid.iter().copied().flatten().filter(|x| *x < v).max();
            match lower_somes {
                Some(s) => Some(Some(s)),
                None if grid.contains(&None) => Some(None),
                None => None,
            }
        }
    };
    let above = match value {
        None => grid.iter().copied().flatten().min().map(Some),
        Some(v) => grid
            .iter()
            .copied()
            .flatten()
            .filter(|x| *x > v)
            .min()
            .map(Some),
    };
    (below, above)
}

#[cfg(test)]
mod tests {
    use super::super::genome::FEED_COUNT;
    use super::super::sample::FeedVote;
    use super::*;

    const ATR: Decimal = dec!(500);
    const PRICE: Decimal = dec!(40000);

    fn sample(timestamp: i64, long_net: i64, short_net: i64, atr: Decimal) -> ConsensusSample {
        // 100 agents, so a net of `x` votes is exactly x/100.
        let (long_up, long_down) = split(long_net);
        let (short_up, short_down) = split(short_net);
        ConsensusSample {
            timestamp,
            agents: 100,
            agents_per_feed: 6,
            long_up,
            long_down,
            short_up,
            short_down,
            waits: 0,
            conflicts: 0,
            feed_long: [FeedVote::default(); FEED_COUNT],
            feed_short: [FeedVote::default(); FEED_COUNT],
            atr_stop_reference: atr,
            macro_trend_slope: Decimal::ZERO,
            macro_trend_slope_2h: Decimal::ZERO,
            price: PRICE,
        }
    }

    fn split(net: i64) -> (u32, u32) {
        if net >= 0 {
            (net as u32, 0)
        } else {
            (0, (-net) as u32)
        }
    }

    fn policy(config: PolicyConfig) -> ConsensusPolicy {
        ConsensusPolicy::new(config).expect("valid config")
    }

    #[test]
    fn a_long_enters_when_its_net_vote_reaches_the_threshold() {
        let mut p = policy(PolicyConfig::default());
        assert_eq!(p.on_sample(&sample(1_000, 44, 0, ATR), true), None);

        let intent = p.on_sample(&sample(2_000, 45, 0, ATR), true).unwrap();

        assert_eq!(intent.side, Side::Buy);
        assert_eq!(intent.price, PRICE);
        // Default: stop 1.0 ATR, take profit 2.0 ATR, time barrier 14 400 s.
        assert_eq!(intent.stop_loss, PRICE - ATR);
        assert_eq!(intent.take_profit, PRICE + dec!(2) * ATR);
        assert_eq!(intent.deadline_ms, 2_000 + 14_400_000);
        assert_eq!(intent.breakeven_atr, Some(dec!(0.75) * ATR));
        assert_eq!(intent.trailing_atr, Some(dec!(0.50) * ATR));
        assert_eq!(p.signal_side(), Some(Side::Buy));
    }

    #[test]
    fn a_short_mirrors_the_barriers_and_needs_permission() {
        let mut long_only = policy(PolicyConfig::default());
        assert_eq!(long_only.on_sample(&sample(1_000, 0, 60, ATR), false), None);

        let mut both = policy(PolicyConfig::default());
        let intent = both.on_sample(&sample(1_000, 0, 60, ATR), true).unwrap();

        assert_eq!(intent.side, Side::Sell);
        assert_eq!(intent.stop_loss, PRICE + ATR);
        assert_eq!(intent.take_profit, PRICE - dec!(2) * ATR);
    }

    #[test]
    fn a_signal_is_acted_on_once_and_rearms_after_it_ends() {
        let mut p = policy(PolicyConfig::default());
        assert!(p.on_sample(&sample(1, 50, 0, ATR), true).is_some());
        // The same side keeps clearing the threshold: nothing more.
        assert_eq!(p.on_sample(&sample(2, 70, 0, ATR), true), None);
        // No side selected: the signal ends.
        assert_eq!(p.on_sample(&sample(3, 10, 0, ATR), true), None);
        assert_eq!(p.signal_side(), None);
        assert!(p.on_sample(&sample(4, 50, 0, ATR), true).is_some());
    }

    #[test]
    fn a_flip_of_side_enters_the_other_side_at_once() {
        let mut p = policy(PolicyConfig::default());
        assert!(p.on_sample(&sample(1, 50, 0, ATR), true).is_some());
        let flipped = p.on_sample(&sample(2, 0, 50, ATR), true).unwrap();
        assert_eq!(flipped.side, Side::Sell);
    }

    #[test]
    fn both_sides_clearing_the_threshold_stand_aside() {
        let mut p = policy(PolicyConfig::default());
        assert_eq!(p.on_sample(&sample(1, 55, 55, ATR), true), None);
        assert_eq!(p.signal_side(), None);
    }

    #[test]
    fn risk_grows_linearly_from_the_threshold_to_a_unanimous_swarm() {
        let config = PolicyConfig::default();
        let risk = |net: i64| {
            policy(config.clone())
                .on_sample(&sample(1, net, 0, ATR), true)
                .unwrap()
                .risk_fraction
                .unwrap()
        };

        assert_eq!(risk(45), dec!(0.010));
        assert_eq!(risk(100), dec!(0.020));
        // Half-way between 0.45 and 1: 0.725 of the swarm.
        assert_eq!(config.risk_fraction(dec!(0.725), dec!(0.45)), dec!(0.015));
        assert_eq!(config.max_risk(), dec!(0.020));
        // A threshold of 1 leaves no span: the base risk.
        assert_eq!(
            config.risk_fraction(Decimal::ONE, Decimal::ONE),
            dec!(0.010)
        );
    }

    #[test]
    fn a_sample_without_atr_or_price_is_skipped_and_keeps_the_signal() {
        let mut p = policy(PolicyConfig::default());
        assert_eq!(p.on_sample(&sample(1, 50, 0, Decimal::ZERO), true), None);
        assert_eq!(p.signal_side(), None);
        assert!(p.on_sample(&sample(2, 50, 0, ATR), true).is_some());

        // Skipped whole: a neutral vote on an unusable sample does not end the signal.
        assert_eq!(p.on_sample(&sample(3, 0, 0, Decimal::ZERO), true), None);
        assert_eq!(p.signal_side(), Some(Side::Buy));
        let mut no_price = sample(4, 0, 0, ATR);
        no_price.price = Decimal::ZERO;
        assert_eq!(p.on_sample(&no_price, true), None);
        assert_eq!(p.signal_side(), Some(Side::Buy));
    }

    #[test]
    fn reset_equals_fresh() {
        let mut p = policy(PolicyConfig::default());
        p.on_sample(&sample(1, 50, 0, ATR), true);
        p.reset();
        assert_eq!(p.signal_side(), None);
        assert!(p.on_sample(&sample(2, 50, 0, ATR), true).is_some());
    }

    #[test]
    fn the_default_config_is_valid_and_displays_compactly() {
        let config = PolicyConfig::default();
        assert_eq!(config.validate(), Ok(()));
        assert_eq!(
            config.to_string(),
            "B:0.45|R:0.010|Ri:0.010|Stop:1.0|TP:2.0|BE:0.75|Trail:0.50|Decay:14400s"
        );
        let filtered = PolicyConfig {
            macro_trend_filter: true,
            macro_slope_min: dec!(0.02),
            short_threshold_delta: dec!(0.05),
            ..config
        };
        assert!(filtered
            .to_string()
            .ends_with("|Macro:0.02|ShortDelta:0.05"));
    }

    #[test]
    fn validation_rejects_each_invalid_field() {
        use PolicyConfigError as E;
        let base = PolicyConfig::default;
        let cases: Vec<(PolicyConfig, E)> = vec![
            (
                PolicyConfig {
                    threshold_base: Decimal::ZERO,
                    ..base()
                },
                E::NonPositiveThresholdBase(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    threshold_base: dec!(1.01),
                    ..base()
                },
                E::ThresholdAboveOne(dec!(1.01)),
            ),
            (
                PolicyConfig {
                    base_risk: Decimal::ZERO,
                    ..base()
                },
                E::NonPositiveBaseRisk(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    risk_increment: dec!(-0.01),
                    ..base()
                },
                E::NegativeRiskIncrement(dec!(-0.01)),
            ),
            (
                PolicyConfig {
                    base_risk: dec!(0.6),
                    risk_increment: dec!(0.5),
                    ..base()
                },
                E::MaxRiskAboveOne(dec!(1.1)),
            ),
            (
                PolicyConfig {
                    stop_atr_multiplier: Decimal::ZERO,
                    ..base()
                },
                E::NonPositiveStopMultiplier(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    stop_atr_multiplier: dec!(2.01),
                    take_profit_atr_multiplier: dec!(5),
                    ..base()
                },
                E::ExcessiveStop(dec!(2.01)),
            ),
            (
                PolicyConfig {
                    take_profit_atr_multiplier: Decimal::ZERO,
                    ..base()
                },
                E::NonPositiveTakeProfitMultiplier(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    take_profit_atr_multiplier: dec!(1.49),
                    ..base()
                },
                E::InvertedPayoff(dec!(1.49), dec!(1.0)),
            ),
            (
                PolicyConfig {
                    breakeven_atr_multiplier: Some(Decimal::ZERO),
                    ..base()
                },
                E::NonPositiveBreakevenMultiplier(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    breakeven_atr_multiplier: Some(dec!(2.0)),
                    ..base()
                },
                E::BreakevenAboveTakeProfit(dec!(2.0), dec!(2.0)),
            ),
            (
                PolicyConfig {
                    trailing_atr_multiplier: Some(Decimal::ZERO),
                    ..base()
                },
                E::NonPositiveTrailingMultiplier(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    decay_horizon_seconds: 7199,
                    ..base()
                },
                E::InvalidDecayHorizon(7199),
            ),
            (
                PolicyConfig {
                    decay_horizon_seconds: 86401,
                    ..base()
                },
                E::InvalidDecayHorizon(86401),
            ),
            (
                PolicyConfig {
                    macro_slope_min: dec!(-0.01),
                    ..base()
                },
                E::NegativeMacroSlopeMin(dec!(-0.01)),
            ),
            (
                PolicyConfig {
                    short_threshold_delta: dec!(-0.01),
                    ..base()
                },
                E::NegativeShortThresholdDelta(dec!(-0.01)),
            ),
        ];
        for (config, expected) in cases {
            assert_eq!(config.validate(), Err(expected.clone()), "{expected}");
            assert!(ConsensusPolicy::new(config).is_err());
        }
        // The boundaries are valid.
        let edge = PolicyConfig {
            threshold_base: Decimal::ONE,
            risk_increment: Decimal::ZERO,
            stop_atr_multiplier: dec!(2.00),
            take_profit_atr_multiplier: dec!(3.00),
            decay_horizon_seconds: 7200,
            ..base()
        };
        assert_eq!(edge.validate(), Ok(()));
    }

    #[test]
    fn a_config_round_trips_through_json() {
        let config = PolicyConfig {
            feed_weights: Some(FeedWeights::equal()),
            macro_trend_filter: true,
            macro_slope_min: dec!(0.04),
            ..PolicyConfig::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<PolicyConfig>(&json).unwrap(), config);
    }

    /// A configuration file written before the target-level policy was removed.
    fn stored(classifier_mode: bool) -> String {
        format!(
            r#"{{
                "threshold_base": "0.5", "threshold_step": "0.075", "hysteresis": "0.55",
                "base_risk": "0.01", "risk_increment": "0.0", "stop_atr_multiplier": "1.25",
                "take_profit_atr_multiplier": "4.0", "breakeven_atr_multiplier": "1.0",
                "trailing_atr_multiplier": "1.0", "decay_horizon_seconds": 86400,
                "bracket_priority": true, "classifier_mode": {classifier_mode},
                "macro_trend_filter": true, "macro_slope_min": "0.02",
                "short_threshold_delta": "0.0"
            }}"#
        )
    }

    #[test]
    fn an_older_classifier_file_loads_and_its_removed_fields_are_ignored() {
        let config: PolicyConfig = serde_json::from_str(&stored(true)).unwrap();
        assert_eq!(config.threshold_base, dec!(0.5));
        assert_eq!(config.take_profit_atr_multiplier, dec!(4.0));
        assert_eq!(config.decay_horizon_seconds, 86400);
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn a_file_of_the_removed_target_policy_is_refused_with_its_reason() {
        let error = serde_json::from_str::<PolicyConfig>(&stored(false))
            .unwrap_err()
            .to_string();
        assert!(error.contains("removed target-level policy"), "{error}");
    }

    #[test]
    fn feed_weights_replace_the_plain_net_vote() {
        let mut s = ConsensusSample {
            timestamp: 1_000,
            agents: 96,
            agents_per_feed: 6,
            long_up: 6,
            long_down: 6,
            short_up: 0,
            short_down: 0,
            waits: 84,
            conflicts: 0,
            feed_long: [FeedVote::default(); FEED_COUNT],
            feed_short: [FeedVote::default(); FEED_COUNT],
            atr_stop_reference: ATR,
            macro_trend_slope: Decimal::ZERO,
            macro_trend_slope_2h: Decimal::ZERO,
            price: PRICE,
        };
        s.feed_long[0] = FeedVote { up: 0, down: 6 };
        s.feed_long[4] = FeedVote { up: 6, down: 0 };

        // Unweighted, the two feeds cancel: the net vote is 0.
        let mut unweighted = policy(PolicyConfig {
            threshold_base: dec!(0.20),
            ..PolicyConfig::default()
        });
        assert_eq!(unweighted.on_sample(&s, false), None);

        // Only tick feeds count (0.25 each): feed 4 alone gives a net vote of 0.25.
        let ticks_only = FeedWeights::from_categories(dec!(0), dec!(1), dec!(0), dec!(0)).unwrap();
        let mut weighted = policy(PolicyConfig {
            threshold_base: dec!(0.20),
            feed_weights: Some(ticks_only),
            ..PolicyConfig::default()
        });
        assert!(weighted.on_sample(&s, false).is_some());
    }

    fn trending(
        long_net: i64,
        short_net: i64,
        slope_1h: Decimal,
        slope_2h: Decimal,
    ) -> ConsensusSample {
        ConsensusSample {
            macro_trend_slope: slope_1h,
            macro_trend_slope_2h: slope_2h,
            ..sample(1, long_net, short_net, ATR)
        }
    }

    fn filtered(macro_slope_min: Decimal) -> ConsensusPolicy {
        policy(PolicyConfig {
            macro_trend_filter: true,
            macro_slope_min,
            ..PolicyConfig::default()
        })
    }

    #[test]
    fn the_macro_filter_blocks_entries_against_the_trend() {
        let up = dec!(0.05);
        // A long against a falling hourly slope, and a short against a rising one.
        assert_eq!(
            filtered(Decimal::ZERO).on_sample(&trending(60, 0, -up, -up), true),
            None
        );
        assert_eq!(
            filtered(Decimal::ZERO).on_sample(&trending(0, 60, up, up), true),
            None
        );
        // With the trend, both enter.
        assert!(filtered(Decimal::ZERO)
            .on_sample(&trending(60, 0, up, up), true)
            .is_some());
        assert!(filtered(Decimal::ZERO)
            .on_sample(&trending(0, 60, -up, -up), true)
            .is_some());
        // Without the filter the trend is not read.
        assert!(policy(PolicyConfig::default())
            .on_sample(&trending(60, 0, -up, -up), true)
            .is_some());
    }

    #[test]
    fn the_macro_filter_needs_the_two_hour_slope_not_to_disagree() {
        let up = dec!(0.05);
        assert_eq!(
            filtered(Decimal::ZERO).on_sample(&trending(60, 0, up, -up), true),
            None
        );
        assert_eq!(
            filtered(Decimal::ZERO).on_sample(&trending(0, 60, -up, up), true),
            None
        );
        // A flat two-hour slope does not disagree.
        assert!(filtered(Decimal::ZERO)
            .on_sample(&trending(60, 0, up, Decimal::ZERO), true)
            .is_some());
    }

    #[test]
    fn the_minimum_slope_withholds_entries_in_a_choppy_market() {
        let weak = dec!(0.01);
        let strong = dec!(0.03);
        assert_eq!(
            filtered(dec!(0.02)).on_sample(&trending(60, 0, weak, weak), true),
            None
        );
        assert!(filtered(dec!(0.02))
            .on_sample(&trending(60, 0, strong, strong), true)
            .is_some());
        assert_eq!(
            filtered(dec!(0.02)).on_sample(&trending(0, 60, -weak, -weak), true),
            None
        );
        assert!(filtered(dec!(0.02))
            .on_sample(&trending(0, 60, -strong, -strong), true)
            .is_some());
    }

    #[test]
    fn a_blocked_entry_ends_the_signal() {
        let up = dec!(0.05);
        let mut p = filtered(Decimal::ZERO);
        assert!(p.on_sample(&trending(60, 0, up, up), true).is_some());
        assert_eq!(p.on_sample(&trending(60, 0, -up, -up), true), None);
        assert_eq!(p.signal_side(), None);
        assert!(p.on_sample(&trending(60, 0, up, up), true).is_some());
    }

    #[test]
    fn the_short_threshold_delta_asks_shorts_for_more_conviction() {
        let asymmetric = || {
            policy(PolicyConfig {
                short_threshold_delta: dec!(0.10),
                ..PolicyConfig::default()
            })
        };
        // 0.50 clears the long threshold (0.45) but not the short one (0.55).
        assert!(asymmetric()
            .on_sample(&sample(1, 50, 0, ATR), true)
            .is_some());
        assert_eq!(asymmetric().on_sample(&sample(1, 0, 50, ATR), true), None);
        let short = asymmetric()
            .on_sample(&sample(1, 0, 55, ATR), true)
            .unwrap();
        assert_eq!(short.side, Side::Sell);
        // The short's risk is measured from its own threshold.
        assert_eq!(short.risk_fraction, Some(dec!(0.010)));
    }

    fn on_grid(space: &SwarmPolicySpace, c: &PolicyConfig) -> bool {
        space.threshold_bases.contains(&c.threshold_base)
            && space.base_risks.contains(&c.base_risk)
            && space.risk_increments.contains(&c.risk_increment)
            && space.stop_atr_multipliers.contains(&c.stop_atr_multiplier)
            && space
                .take_profit_atr_multipliers
                .contains(&c.take_profit_atr_multiplier)
            && space
                .breakeven_atr_multipliers
                .contains(&c.breakeven_atr_multiplier)
            && space
                .trailing_atr_multipliers
                .contains(&c.trailing_atr_multiplier)
            && space.feed_weights_options.contains(&c.feed_weights)
            && space
                .macro_trend_filter_options
                .contains(&c.macro_trend_filter)
            && space.macro_slope_min_options.contains(&c.macro_slope_min)
            && space
                .short_threshold_delta_options
                .contains(&c.short_threshold_delta)
            && space
                .decay_horizon_options
                .contains(&c.decay_horizon_seconds)
    }

    #[test]
    fn candidates_are_on_the_grid_valid_and_deterministic() {
        let space = SwarmPolicySpace::default();
        let all = space.generate_candidates(usize::MAX);
        // 4 thresholds x 3 stops x 3 take profits x 3 breakevens x 3 trailings x 3 slopes x 6
        // weight profiles, all admitted.
        assert_eq!(all.len(), 5832);
        assert!(all.iter().all(|c| on_grid(&space, c) && space.admits(c)));

        let drawn = space.generate_candidates(40);
        assert_eq!(drawn.len(), 40);
        assert_eq!(drawn, space.generate_candidates(40));
        assert!(drawn.iter().all(|c| all.contains(c)));
    }

    #[test]
    fn the_grid_drops_invalid_candidates_and_those_above_the_risk_cap() {
        let space = SwarmPolicySpace {
            base_risks: vec![dec!(0.01), dec!(0.04)],
            risk_increments: vec![dec!(0.0), dec!(0.02)],
            // 2.0 breaks the 1.5x payoff rule against a stop of 1.5.
            stop_atr_multipliers: vec![dec!(1.0), dec!(1.5)],
            take_profit_atr_multipliers: vec![dec!(2.0)],
            threshold_bases: vec![dec!(0.5)],
            breakeven_atr_multipliers: vec![None],
            trailing_atr_multipliers: vec![None],
            feed_weights_options: vec![None],
            macro_trend_filter_options: vec![false],
            macro_slope_min_options: vec![Decimal::ZERO],
            ..SwarmPolicySpace::default()
        };
        let all = space.generate_candidates(usize::MAX);
        // Of the 8 grid points, the stop of 1.5 drops 4 and the risk 0.04 + 0.02 drops 1.
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|c| c.max_risk() <= dec!(0.05)));
        assert!(all.iter().all(|c| c.stop_atr_multiplier == dec!(1.0)));
    }

    #[test]
    fn neighbours_are_admitted_and_differ_from_the_base_in_one_adjacent_step() {
        let space = SwarmPolicySpace::default();
        let base = space.generate_candidates(usize::MAX)[2916].clone();
        let neighbours = space.perturb_neighbors(&base);

        assert!(!neighbours.is_empty());
        for n in &neighbours {
            assert!(space.admits(n) && on_grid(&space, n), "{n}");
            let differences = [
                n.threshold_base != base.threshold_base,
                n.base_risk != base.base_risk,
                n.risk_increment != base.risk_increment,
                n.stop_atr_multiplier != base.stop_atr_multiplier,
                n.take_profit_atr_multiplier != base.take_profit_atr_multiplier,
                n.breakeven_atr_multiplier != base.breakeven_atr_multiplier,
                n.trailing_atr_multiplier != base.trailing_atr_multiplier,
                n.feed_weights != base.feed_weights,
                n.macro_trend_filter != base.macro_trend_filter,
                n.macro_slope_min != base.macro_slope_min,
                n.short_threshold_delta != base.short_threshold_delta,
                n.decay_horizon_seconds != base.decay_horizon_seconds,
            ];
            assert_eq!(differences.iter().filter(|d| **d).count(), 1, "{n}");
        }
        // The threshold steps to both grid neighbours of 0.525.
        assert_eq!(base.threshold_base, dec!(0.525));
        let thresholds: Vec<Decimal> = neighbours
            .iter()
            .map(|n| n.threshold_base)
            .filter(|t| *t != base.threshold_base)
            .collect();
        assert_eq!(thresholds, [dec!(0.50), dec!(0.55)]);
    }

    #[test]
    fn an_off_grid_value_steps_to_the_grid_values_around_it() {
        let space = SwarmPolicySpace::default();
        let base = PolicyConfig {
            threshold_base: dec!(0.51),
            take_profit_atr_multiplier: dec!(4.0),
            macro_trend_filter: true,
            macro_slope_min: dec!(0.02),
            decay_horizon_seconds: 86400,
            ..PolicyConfig::default()
        };
        let thresholds: Vec<Decimal> = space
            .perturb_neighbors(&base)
            .iter()
            .map(|n| n.threshold_base)
            .filter(|t| *t != base.threshold_base)
            .collect();
        assert_eq!(thresholds, [dec!(0.50), dec!(0.525)]);
    }

    #[test]
    fn the_feed_weight_profiles_are_explored_and_stepped() {
        let space = SwarmPolicySpace::default().with_standard_feed_profiles();
        // No weights plus the five standard profiles.
        assert_eq!(space.feed_weights_options.len(), 6);

        let base = PolicyConfig {
            feed_weights: space.feed_weights_options[1].clone(),
            take_profit_atr_multiplier: dec!(4.0),
            macro_trend_filter: true,
            macro_slope_min: dec!(0.02),
            decay_horizon_seconds: 86400,
            ..PolicyConfig::default()
        };
        let neighbours = space.perturb_neighbors(&base);
        assert!(neighbours.iter().any(|n| n.feed_weights.is_none()));
        assert!(neighbours
            .iter()
            .any(|n| n.feed_weights == space.feed_weights_options[2]));
    }

    #[test]
    fn the_space_round_trips_and_reads_files_of_the_older_layout() {
        let space = SwarmPolicySpace::default();
        let json = serde_json::to_string(&space).unwrap();
        assert_eq!(
            serde_json::from_str::<SwarmPolicySpace>(&json).unwrap(),
            space
        );

        // Optional grids default to a single neutral value; the cap keeps its older name.
        let older: SwarmPolicySpace = serde_json::from_str(
            r#"{
                "threshold_bases": ["0.5"], "threshold_steps": ["0.075"],
                "hysteresis_values": ["0.55"], "base_risks": ["0.01"],
                "risk_increments": ["0.0"], "stop_atr_multipliers": ["1.0"],
                "take_profit_atr_multipliers": ["3.5"], "decay_horizon_options": [86400],
                "max_level_risk": "0.03", "classifier_mode": true
            }"#,
        )
        .unwrap();
        assert_eq!(older.max_risk, dec!(0.03));
        assert_eq!(older.breakeven_atr_multipliers, vec![None]);
        assert_eq!(older.feed_weights_options, vec![None]);
        assert_eq!(older.macro_trend_filter_options, vec![false]);
        assert_eq!(older.generate_candidates(usize::MAX).len(), 1);
    }
}

/// Pins the candidate draw: a budget below the grid size must pick the same candidates after any
/// dependency update.
#[cfg(test)]
mod pinned_candidates {
    use super::*;

    #[test]
    fn a_budget_of_25_draws_the_recorded_candidates_of_the_default_space() {
        let space = SwarmPolicySpace::default();
        let grid = space.generate_candidates(usize::MAX);
        assert_eq!(grid.len(), 5832);
        let drawn: Vec<usize> = space
            .generate_candidates(25)
            .iter()
            .filter_map(|candidate| grid.iter().position(|config| config == candidate))
            .collect();
        assert_eq!(
            drawn,
            [
                4037, 3713, 715, 1483, 3659, 3606, 501, 1273, 4726, 5204, 2767, 2026, 1773, 2544,
                5755, 893, 567, 243, 2428, 4727, 2721, 20, 1750, 4693, 4674
            ]
        );
    }
}
