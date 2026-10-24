//! The consensus policy (Stage B) of the swarm consensus strategy: the original level machine
//! with hysteresis on two axes, the signed target, risk per level, and its HPO search space
//! (strategy spec `docs/strategy/03-swarm-consensus.md` §10 and §14.2).

use domain::{Decision, TargetPosition};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use super::sample::{ConsensusSample, FeedWeights, FeedWeightsError};
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
    /// `risk_increment` must not be negative in classifier mode.
    #[error("risk_increment must be >= 0, got {0}")]
    NegativeRiskIncrement(Decimal),
    /// The level-3 risk `base_risk + 2·risk_increment` must not exceed 1.
    #[error("level-3 risk (base_risk + 2*risk_increment) must be <= 1, got {0}")]
    TopRiskAboveOne(Decimal),
    /// `stop_atr_multiplier` must be strictly positive.
    #[error("stop_atr_multiplier must be > 0, got {0}")]
    NonPositiveStopMultiplier(Decimal),
    /// `take_profit_atr_multiplier` must be strictly positive when set.
    #[error("take_profit_atr_multiplier must be > 0, got {0}")]
    NonPositiveTakeProfitMultiplier(Decimal),
    /// `decay_horizon_seconds` must be strictly positive when set.
    #[error("decay_horizon_seconds must be > 0, got {0}")]
    NonPositiveDecayHorizon(u64),
    /// `breakeven_atr_multiplier` must be strictly positive when set.
    #[error("breakeven_atr_multiplier must be > 0, got {0}")]
    NonPositiveBreakevenMultiplier(Decimal),
    /// `trailing_atr_multiplier` must be strictly positive when set.
    #[error("trailing_atr_multiplier must be > 0, got {0}")]
    NonPositiveTrailingMultiplier(Decimal),
    /// In classifier mode, `breakeven_atr_multiplier` must be strictly less than `take_profit_atr_multiplier`.
    #[error("breakeven multiplier {0} must be < take profit multiplier {1}")]
    BreakevenAboveTakeProfit(Decimal, Decimal),
    /// In classifier mode, `take_profit_atr_multiplier` must be at least 1.5x `stop_atr_multiplier`.
    #[error("take profit multiplier {0} must be >= 1.5x stop multiplier {1}")]
    InvertedPayoff(Decimal, Decimal),
    /// In classifier mode, `take_profit_atr_multiplier` must be specified.
    #[error("take profit multiplier must be specified in classifier mode")]
    MissingTakeProfitInClassifierMode,
    /// In classifier mode, `decay_horizon_seconds` must be specified and between 7200s (2h) and 86400s (24h).
    #[error(
        "decay horizon must be between 7200s (2h) and 86400s (24h) in classifier mode, got {0:?}"
    )]
    InvalidDecayHorizonInClassifierMode(Option<u64>),
    /// In classifier mode, `stop_atr_multiplier` must not exceed 2.00 ATR.
    #[error("stop ATR multiplier must be <= 2.00 in classifier mode, got {0}")]
    ExcessiveStopInClassifierMode(Decimal),
    /// `feed_weights` is invalid.
    #[error("invalid feed_weights: {0}")]
    InvalidFeedWeights(#[from] FeedWeightsError),
    /// `macro_slope_min` must be non-negative.
    #[error("macro_slope_min must be >= 0, got {0}")]
    NegativeMacroSlopeMin(Decimal),
    /// `short_threshold_delta` must be non-negative.
    #[error("short_threshold_delta must be >= 0, got {0}")]
    NegativeShortThresholdDelta(Decimal),
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
    /// Stop distance in multiples of the feed-1 ATR.
    pub stop_atr_multiplier: Decimal,
    /// Optional take-profit distance in multiples of the feed-1 ATR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_profit_atr_multiplier: Option<Decimal>,
    /// Optional breakeven ATR multiplier (favorable excursion to move SL to entry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakeven_atr_multiplier: Option<Decimal>,
    /// Optional trailing step ATR multiplier (distance per trailing ratchet step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing_atr_multiplier: Option<Decimal>,
    /// Optional max holding horizon in seconds before forced flat exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decay_horizon_seconds: Option<u64>,
    /// Optional feed weights for heterogeneous consensus voting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_weights: Option<FeedWeights>,
    /// Whether open positions with active bracket exits (TP/SL) prioritize holding until TP/SL or
    /// opposite-side flip, rather than market-closing on neutral consensus decay (k = 0).
    #[serde(default = "default_bracket_priority")]
    pub bracket_priority: bool,
    /// Whether to run in Classifier Mode (emitting triple-barrier `Decision::Entry(OrderIntent)`)
    /// instead of the legacy multi-level target state machine (`Decision::Target`).
    #[serde(default)]
    pub classifier_mode: bool,
    /// Whether to require trend alignment with the higher-timeframe feed (Feed 2, 1h).
    /// Long entries require `macro_trend_slope >= macro_slope_min`, short entries require `macro_trend_slope <= -macro_slope_min`.
    #[serde(default)]
    pub macro_trend_filter: bool,
    /// Minimum higher-timeframe ALMA slope required to enter. When `|macro_trend_slope| < macro_slope_min`,
    /// the market is in choppy consolidation and entries are withheld.
    #[serde(default)]
    pub macro_slope_min: Decimal,
    /// Additional consensus threshold required for short entries (`theta_short = theta_long + short_threshold_delta`).
    #[serde(default)]
    pub short_threshold_delta: Decimal,
}

fn default_bracket_priority() -> bool {
    true
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            threshold_base: dec!(0.45),
            threshold_step: dec!(0.075),
            hysteresis: dec!(0.55),
            base_risk: dec!(0.010),
            risk_increment: dec!(0.005),
            stop_atr_multiplier: dec!(3.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            breakeven_atr_multiplier: None,
            trailing_atr_multiplier: None,
            decay_horizon_seconds: None,
            feed_weights: None,
            bracket_priority: true,
            classifier_mode: false,
            macro_trend_filter: false,
            macro_slope_min: Decimal::ZERO,
            short_threshold_delta: Decimal::ZERO,
        }
    }
}

impl PolicyConfig {
    /// Recommended defaults for Classifier Mode with Triple Barrier protection.
    pub fn classifier_default() -> Self {
        Self {
            threshold_base: dec!(0.45),
            threshold_step: dec!(0.075),
            hysteresis: dec!(0.55),
            base_risk: dec!(0.010),
            risk_increment: dec!(0.010),
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            breakeven_atr_multiplier: Some(dec!(0.75)),
            trailing_atr_multiplier: Some(dec!(0.50)),
            decay_horizon_seconds: Some(14400),
            feed_weights: None,
            bracket_priority: true,
            classifier_mode: true,
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
            "B:{}|S:{}|H:{}|R:{}|Ri:{}|Stop:{}",
            self.threshold_base,
            self.threshold_step,
            self.hysteresis,
            self.base_risk,
            self.risk_increment,
            self.stop_atr_multiplier
        )?;
        if let Some(tp) = self.take_profit_atr_multiplier {
            write!(f, "|TP:{tp}")?;
        }
        if let Some(be) = self.breakeven_atr_multiplier {
            write!(f, "|BE:{be}")?;
        }
        if let Some(tr) = self.trailing_atr_multiplier {
            write!(f, "|Trail:{tr}")?;
        }
        if let Some(decay) = self.decay_horizon_seconds {
            write!(f, "|Decay:{decay}s")?;
        }
        if let Some(ref weights) = self.feed_weights {
            write!(f, "|W:{weights}")?;
        }
        if self.bracket_priority {
            write!(f, "|BP")?;
        }
        if self.classifier_mode {
            write!(f, "|Classifier")?;
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
        if !self.classifier_mode && self.risk_increment <= Decimal::ZERO {
            return Err(PolicyConfigError::NonPositiveRiskIncrement(
                self.risk_increment,
            ));
        }
        if self.classifier_mode && self.risk_increment < Decimal::ZERO {
            return Err(PolicyConfigError::NegativeRiskIncrement(
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
        if let Some(tp) = self.take_profit_atr_multiplier {
            if tp <= Decimal::ZERO {
                return Err(PolicyConfigError::NonPositiveTakeProfitMultiplier(tp));
            }
        }
        if let Some(be) = self.breakeven_atr_multiplier {
            if be <= Decimal::ZERO {
                return Err(PolicyConfigError::NonPositiveBreakevenMultiplier(be));
            }
        }
        if let Some(tr) = self.trailing_atr_multiplier {
            if tr <= Decimal::ZERO {
                return Err(PolicyConfigError::NonPositiveTrailingMultiplier(tr));
            }
        }
        if let Some(decay) = self.decay_horizon_seconds {
            if decay == 0 {
                return Err(PolicyConfigError::NonPositiveDecayHorizon(decay));
            }
        }
        if let Some(ref weights) = self.feed_weights {
            weights.validate()?;
        }
        if self.classifier_mode {
            if self.stop_atr_multiplier > dec!(2.00) {
                return Err(PolicyConfigError::ExcessiveStopInClassifierMode(
                    self.stop_atr_multiplier,
                ));
            }
            match self.take_profit_atr_multiplier {
                Some(tp) => {
                    if tp < dec!(1.5) * self.stop_atr_multiplier {
                        return Err(PolicyConfigError::InvertedPayoff(
                            tp,
                            self.stop_atr_multiplier,
                        ));
                    }
                }
                None => {
                    return Err(PolicyConfigError::MissingTakeProfitInClassifierMode);
                }
            }
            if let (Some(be), Some(tp)) = (
                self.breakeven_atr_multiplier,
                self.take_profit_atr_multiplier,
            ) {
                if be >= tp {
                    return Err(PolicyConfigError::BreakevenAboveTakeProfit(be, tp));
                }
            }
            match self.decay_horizon_seconds {
                Some(s) if (7200..=86400).contains(&s) => {}
                other => {
                    return Err(PolicyConfigError::InvalidDecayHorizonInClassifierMode(
                        other,
                    ));
                }
            }
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
    level_entry_timestamp: Option<i64>,
    in_signal_side: Option<domain::Side>,
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
            level_entry_timestamp: None,
            in_signal_side: None,
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
        if self.config.classifier_mode {
            if sample.atr_stop_reference <= Decimal::ZERO || sample.price <= Decimal::ZERO {
                return None;
            }

            let (long_net, short_net) = match &self.config.feed_weights {
                Some(weights) => (
                    sample.weighted_long_net(weights),
                    sample.weighted_short_net(weights),
                ),
                None => (sample.long_net(), sample.short_net()),
            };

            let theta_long = self.config.threshold_base;
            let theta_short = self.config.threshold_base + self.config.short_threshold_delta;

            let (candidate_side, conviction, theta) =
                if long_net >= theta_long && short_net < theta_long {
                    if self.config.macro_trend_filter
                        && sample.macro_trend_slope < self.config.macro_slope_min
                    {
                        (None, Decimal::ZERO, theta_long)
                    } else {
                        (Some(domain::Side::Buy), long_net, theta_long)
                    }
                } else if allow_short && short_net >= theta_short && long_net < theta_short {
                    if self.config.macro_trend_filter
                        && sample.macro_trend_slope > -self.config.macro_slope_min
                    {
                        (None, Decimal::ZERO, theta_short)
                    } else {
                        (Some(domain::Side::Sell), short_net, theta_short)
                    }
                } else {
                    (None, Decimal::ZERO, theta_long)
                };

            let Some(side) = candidate_side else {
                self.in_signal_side = None;
                self.last_level = 0;
                self.long_level = 0;
                self.short_level = 0;
                return None;
            };

            if self.in_signal_side == Some(side) {
                return None;
            }

            let denom = Decimal::ONE - theta;
            let fraction = if denom > Decimal::ZERO {
                ((conviction - theta) / denom)
                    .max(Decimal::ZERO)
                    .min(Decimal::ONE)
            } else {
                Decimal::ZERO
            };
            let calculated_risk = self.config.base_risk + fraction * self.config.risk_increment;
            let top_risk = self.config.top_level_risk();
            let risk = calculated_risk.min(top_risk);

            let stop_distance = self.config.stop_atr_multiplier * sample.atr_stop_reference;
            let tp_distance = match self.config.take_profit_atr_multiplier {
                Some(tp_mult) => tp_mult * sample.atr_stop_reference,
                None => dec!(1.5) * stop_distance,
            };

            let (stop_loss, take_profit) = match side {
                domain::Side::Buy => (sample.price - stop_distance, sample.price + tp_distance),
                domain::Side::Sell => (sample.price + stop_distance, sample.price - tp_distance),
            };

            let deadline_ms = self
                .config
                .decay_horizon_seconds
                .map(|s| sample.timestamp + (s as i64) * 1000);

            let breakeven_atr = self
                .config
                .breakeven_atr_multiplier
                .map(|m| m * sample.atr_stop_reference);
            let trailing_atr = self
                .config
                .trailing_atr_multiplier
                .map(|m| m * sample.atr_stop_reference);

            let intent = domain::OrderIntent::with_risk_and_exits(
                sample.timestamp,
                side,
                sample.price,
                stop_loss,
                take_profit,
                0,
                Some(risk),
                trailing_atr,
                breakeven_atr,
                deadline_ms,
            )
            .ok()?;

            self.in_signal_side = Some(side);
            self.last_level = match side {
                domain::Side::Buy => 1,
                domain::Side::Sell => -1,
            };
            self.long_level = if side == domain::Side::Buy { 1 } else { 0 };
            self.short_level = if side == domain::Side::Sell { 1 } else { 0 };
            self.level_entry_timestamp = Some(sample.timestamp);

            return Some(Decision::Entry(intent));
        }

        let (l1_long, l2_long, l3_long) = self.config.thresholds();
        let l1_short = l1_long + self.config.short_threshold_delta;
        let l2_short = l2_long + self.config.short_threshold_delta;
        let l3_short = l3_long + self.config.short_threshold_delta;
        let h = self.config.hysteresis;
        let (long_net, short_net) = match &self.config.feed_weights {
            Some(weights) => (
                sample.weighted_long_net(weights),
                sample.weighted_short_net(weights),
            ),
            None => (sample.long_net(), sample.short_net()),
        };
        self.long_level = axis_update(self.long_level, long_net, l1_long, l2_long, l3_long, h);
        self.short_level = if allow_short {
            axis_update(self.short_level, short_net, l1_short, l2_short, l3_short, h)
        } else {
            0
        };
        if self.config.macro_trend_filter {
            if sample.macro_trend_slope < self.config.macro_slope_min {
                self.long_level = 0;
            }
            if sample.macro_trend_slope > -self.config.macro_slope_min {
                self.short_level = 0;
            }
        }
        // Both levels are at most 3, so the difference always fits an `i8`.
        let mut k = if self.long_level > 0 && self.short_level > 0 {
            0
        } else {
            self.long_level as i8 - self.short_level as i8
        };

        // Temporal decay / max holding horizon check:
        if self.last_level != 0 {
            if let (Some(horizon_s), Some(entry_ts)) = (
                self.config.decay_horizon_seconds,
                self.level_entry_timestamp,
            ) {
                if sample.timestamp.saturating_sub(entry_ts) >= (horizon_s as i64) * 1000 {
                    // Holding horizon expired without consensus naturally flipping: force flat exit
                    self.long_level = 0;
                    self.short_level = 0;
                    k = 0;
                }
            }
        }

        if k == self.last_level {
            return None;
        }
        let (risk, stop, take_profit) = if k == 0 {
            (Decimal::ZERO, Decimal::ZERO, None)
        } else {
            (
                self.config.risk_fraction(k.unsigned_abs()),
                self.config.stop_atr_multiplier * sample.atr_stop_reference,
                self.config
                    .take_profit_atr_multiplier
                    .map(|m| m * sample.atr_stop_reference),
            )
        };
        // Invalid only for a non-zero level with a non-positive ATR (the config is validated).
        let target =
            TargetPosition::with_exits(sample.timestamp, k, risk, stop, take_profit).ok()?;
        self.last_level = k;
        if k != 0 {
            self.level_entry_timestamp = Some(sample.timestamp);
        } else {
            self.level_entry_timestamp = None;
        }
        Some(Decision::Target(target))
    }

    /// Returns to the flat initial state, keeping the configuration.
    pub fn reset(&mut self) {
        self.long_level = 0;
        self.short_level = 0;
        self.last_level = 0;
        self.level_entry_timestamp = None;
        self.in_signal_side = None;
    }
}

fn default_max_level_risk() -> Decimal {
    dec!(0.05)
}

fn default_take_profit_multipliers() -> Vec<Option<Decimal>> {
    vec![
        Some(dec!(1.25)),
        Some(dec!(1.5)),
        Some(dec!(1.75)),
        Some(dec!(2.0)),
        Some(dec!(2.25)),
        Some(dec!(2.5)),
        Some(dec!(3.0)),
        Some(dec!(3.5)),
        Some(dec!(4.0)),
    ]
}

fn default_breakeven_multipliers() -> Vec<Option<Decimal>> {
    vec![None]
}

fn default_trailing_multipliers() -> Vec<Option<Decimal>> {
    vec![None]
}

fn default_feed_weights_options() -> Vec<Option<FeedWeights>> {
    vec![None]
}

fn standard_feed_weights_options() -> Vec<Option<FeedWeights>> {
    let mut profiles: Vec<Option<FeedWeights>> = vec![None];
    profiles.extend(FeedWeights::standard_profiles().into_iter().map(Some));
    profiles
}

fn default_bracket_priority_options() -> Vec<bool> {
    vec![true]
}

fn default_macro_trend_filter_options() -> Vec<bool> {
    vec![false]
}

fn default_macro_slope_min_options() -> Vec<Decimal> {
    vec![Decimal::ZERO]
}

fn default_short_threshold_delta_options() -> Vec<Decimal> {
    vec![Decimal::ZERO]
}

fn default_decay_horizon_options() -> Vec<Option<u64>> {
    vec![None]
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
    /// Values of `stop_atr_multiplier`.
    pub stop_atr_multipliers: Vec<Decimal>,
    /// Values of `take_profit_atr_multiplier` (None for no take profit, or multiple of ATR).
    #[serde(default = "default_take_profit_multipliers")]
    pub take_profit_atr_multipliers: Vec<Option<Decimal>>,
    /// Values of `breakeven_atr_multiplier` (None for no breakeven, or multiple of ATR).
    #[serde(default = "default_breakeven_multipliers")]
    pub breakeven_atr_multipliers: Vec<Option<Decimal>>,
    /// Values of `trailing_atr_multiplier` (None for no trailing ratchet, or multiple of ATR).
    #[serde(default = "default_trailing_multipliers")]
    pub trailing_atr_multipliers: Vec<Option<Decimal>>,
    /// Values of `feed_weights` (None for equal/unweighted, or weighted profile).
    #[serde(default = "default_feed_weights_options")]
    pub feed_weights_options: Vec<Option<FeedWeights>>,
    /// Values of `bracket_priority`.
    #[serde(default = "default_bracket_priority_options")]
    pub bracket_priority_options: Vec<bool>,
    /// Values of `macro_trend_filter`.
    #[serde(default = "default_macro_trend_filter_options")]
    pub macro_trend_filter_options: Vec<bool>,
    /// Values of `macro_slope_min`.
    #[serde(default = "default_macro_slope_min_options")]
    pub macro_slope_min_options: Vec<Decimal>,
    /// Values of `short_threshold_delta`.
    #[serde(default = "default_short_threshold_delta_options")]
    pub short_threshold_delta_options: Vec<Decimal>,
    /// Optional decay / time barrier horizons in seconds.
    #[serde(default = "default_decay_horizon_options")]
    pub decay_horizon_options: Vec<Option<u64>>,
    /// Account policy cap: candidates whose level-3 risk exceeds it are never generated.
    #[serde(default = "default_max_level_risk")]
    pub max_level_risk: Decimal,
    /// Whether this space optimizes Classifier Mode policies.
    #[serde(default)]
    pub classifier_mode: bool,
}

impl Default for SwarmPolicySpace {
    fn default() -> Self {
        Self {
            threshold_bases: vec![
                dec!(0.40),
                dec!(0.425),
                dec!(0.45),
                dec!(0.475),
                dec!(0.50),
                dec!(0.525),
                dec!(0.55),
                dec!(0.575),
                dec!(0.60),
                dec!(0.65),
            ],
            threshold_steps: vec![dec!(0.05), dec!(0.075), dec!(0.10), dec!(0.125), dec!(0.15)],
            hysteresis_values: vec![
                dec!(0.35),
                dec!(0.45),
                dec!(0.55),
                dec!(0.65),
                dec!(0.75),
                dec!(0.85),
                dec!(0.95),
                dec!(1.05),
                dec!(1.15),
            ],
            base_risks: vec![
                dec!(0.005),
                dec!(0.0075),
                dec!(0.010),
                dec!(0.0125),
                dec!(0.015),
                dec!(0.020),
            ],
            risk_increments: vec![
                dec!(0.005),
                dec!(0.0075),
                dec!(0.010),
                dec!(0.0125),
                dec!(0.015),
                dec!(0.020),
            ],
            stop_atr_multipliers: vec![
                dec!(1.5),
                dec!(1.75),
                dec!(2.0),
                dec!(2.25),
                dec!(2.5),
                dec!(2.75),
                dec!(3.0),
                dec!(3.5),
                dec!(4.0),
            ],
            take_profit_atr_multipliers: default_take_profit_multipliers(),
            breakeven_atr_multipliers: default_breakeven_multipliers(),
            trailing_atr_multipliers: default_trailing_multipliers(),
            feed_weights_options: default_feed_weights_options(),
            bracket_priority_options: default_bracket_priority_options(),
            macro_trend_filter_options: default_macro_trend_filter_options(),
            macro_slope_min_options: default_macro_slope_min_options(),
            short_threshold_delta_options: default_short_threshold_delta_options(),
            decay_horizon_options: default_decay_horizon_options(),
            max_level_risk: default_max_level_risk(),
            classifier_mode: false,
        }
    }
}

impl SwarmPolicySpace {
    /// Search space for Classifier Mode with Triple Barrier protection.
    pub fn classifier_space() -> Self {
        Self {
            threshold_bases: vec![dec!(0.50), dec!(0.525), dec!(0.55)],
            threshold_steps: vec![dec!(0.075)],
            hysteresis_values: vec![dec!(0.55)],
            base_risks: vec![dec!(0.010)],
            risk_increments: vec![dec!(0.0)],
            stop_atr_multipliers: vec![dec!(1.0), dec!(1.25), dec!(1.50)],
            take_profit_atr_multipliers: vec![
                Some(dec!(2.5)),
                Some(dec!(3.0)),
                Some(dec!(3.5)),
                Some(dec!(4.0)),
            ],
            breakeven_atr_multipliers: vec![Some(dec!(1.0)), Some(dec!(1.25)), Some(dec!(1.50))],
            trailing_atr_multipliers: vec![Some(dec!(0.75)), Some(dec!(1.0)), Some(dec!(1.25))],
            decay_horizon_options: vec![Some(43200), Some(86400)],
            feed_weights_options: standard_feed_weights_options(),
            bracket_priority_options: default_bracket_priority_options(),
            macro_trend_filter_options: vec![true],
            macro_slope_min_options: vec![dec!(0.0), dec!(0.02), dec!(0.04)],
            short_threshold_delta_options: vec![dec!(0.0), dec!(0.025), dec!(0.05)],
            max_level_risk: default_max_level_risk(),
            classifier_mode: true,
        }
    }

    /// Whether a configuration is valid and within the level-3 risk cap and search space constraints.
    pub fn admits(&self, config: &PolicyConfig) -> bool {
        if config.validate().is_err() || config.top_level_risk() > self.max_level_risk {
            return false;
        }
        if self.classifier_mode || config.classifier_mode {
            if !config.classifier_mode {
                return false;
            }
            if config.stop_atr_multiplier > dec!(2.00) {
                return false;
            }
            match config.take_profit_atr_multiplier {
                Some(tp) if tp >= dec!(1.5) * config.stop_atr_multiplier => {}
                _ => return false,
            }
            if let (Some(be), Some(tp)) = (
                config.breakeven_atr_multiplier,
                config.take_profit_atr_multiplier,
            ) {
                if be >= tp {
                    return false;
                }
            }
            match config.decay_horizon_seconds {
                Some(s) if (7200..=86400).contains(&s) => {}
                _ => return false,
            }
        }
        true
    }

    /// Enables exploration of standard feed weighting and pruning profiles.
    pub fn with_standard_feed_profiles(mut self) -> Self {
        let mut profiles: Vec<Option<FeedWeights>> = vec![None];
        profiles.extend(FeedWeights::standard_profiles().into_iter().map(Some));
        self.feed_weights_options = profiles;
        self
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
                                for &take_profit_atr_multiplier in &self.take_profit_atr_multipliers
                                {
                                    for &breakeven_atr_multiplier in &self.breakeven_atr_multipliers
                                    {
                                        for &trailing_atr_multiplier in
                                            &self.trailing_atr_multipliers
                                        {
                                            for &macro_trend_filter in
                                                &self.macro_trend_filter_options
                                            {
                                                for &macro_slope_min in
                                                    &self.macro_slope_min_options
                                                {
                                                    for &short_threshold_delta in
                                                        &self.short_threshold_delta_options
                                                    {
                                                        for feed_weights in
                                                            &self.feed_weights_options
                                                        {
                                                            for &bracket_priority in
                                                                &self.bracket_priority_options
                                                            {
                                                                for &decay_horizon_seconds in
                                                                    &self.decay_horizon_options
                                                                {
                                                                    let config = PolicyConfig {
                                                                        threshold_base,
                                                                        threshold_step,
                                                                        hysteresis,
                                                                        base_risk,
                                                                        risk_increment,
                                                                        stop_atr_multiplier,
                                                                        take_profit_atr_multiplier,
                                                                        breakeven_atr_multiplier,
                                                                        trailing_atr_multiplier,
                                                                        decay_horizon_seconds,
                                                                        feed_weights: feed_weights
                                                                            .clone(),
                                                                        bracket_priority,
                                                                        classifier_mode: self
                                                                            .classifier_mode,
                                                                        macro_trend_filter,
                                                                        macro_slope_min,
                                                                        short_threshold_delta,
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
                                        }
                                    }
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

    /// One step down and one step up the grid for each of the searched parameters, the
    /// others held, keeping only admitted ones. A value off the grid steps
    /// to the nearest grid values around it.
    fn perturb_neighbors(&self, base: &PolicyConfig) -> Vec<PolicyConfig> {
        let mut neighbors = Vec::with_capacity(18);
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
        push(
            &mut neighbors,
            adjacent_grid_values(&self.stop_atr_multipliers, base.stop_atr_multiplier),
            &|stop_atr_multiplier| PolicyConfig {
                stop_atr_multiplier,
                ..base.clone()
            },
        );
        let (tp_below, tp_above) = adjacent_option_grid_values(
            &self.take_profit_atr_multipliers,
            base.take_profit_atr_multiplier,
        );
        for value in [tp_below, tp_above].into_iter().flatten() {
            let candidate = PolicyConfig {
                take_profit_atr_multiplier: value,
                ..base.clone()
            };
            if self.admits(&candidate) {
                neighbors.push(candidate);
            }
        }
        if self.breakeven_atr_multipliers.len() > 1 {
            let (be_below, be_above) = adjacent_option_grid_values(
                &self.breakeven_atr_multipliers,
                base.breakeven_atr_multiplier,
            );
            for value in [be_below, be_above].into_iter().flatten() {
                let candidate = PolicyConfig {
                    breakeven_atr_multiplier: value,
                    ..base.clone()
                };
                if self.admits(&candidate) {
                    neighbors.push(candidate);
                }
            }
        }
        if self.trailing_atr_multipliers.len() > 1 {
            let (tr_below, tr_above) = adjacent_option_grid_values(
                &self.trailing_atr_multipliers,
                base.trailing_atr_multiplier,
            );
            for value in [tr_below, tr_above].into_iter().flatten() {
                let candidate = PolicyConfig {
                    trailing_atr_multiplier: value,
                    ..base.clone()
                };
                if self.admits(&candidate) {
                    neighbors.push(candidate);
                }
            }
        }
        if self.decay_horizon_options.len() > 1 {
            let (decay_below, decay_above) = adjacent_option_grid_values(
                &self.decay_horizon_options,
                base.decay_horizon_seconds,
            );
            for value in [decay_below, decay_above].into_iter().flatten() {
                let candidate = PolicyConfig {
                    decay_horizon_seconds: value,
                    ..base.clone()
                };
                if self.admits(&candidate) {
                    neighbors.push(candidate);
                }
            }
        }
        if self.feed_weights_options.len() > 1 {
            let idx = self
                .feed_weights_options
                .iter()
                .position(|opt| opt == &base.feed_weights);
            let below = idx
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| self.feed_weights_options.get(i))
                .cloned();
            let above = idx
                .and_then(|i| self.feed_weights_options.get(i + 1))
                .cloned();
            for val in [below, above].into_iter().flatten() {
                let candidate = PolicyConfig {
                    feed_weights: val,
                    ..base.clone()
                };
                if self.admits(&candidate) {
                    neighbors.push(candidate);
                }
            }
        }
        if self.macro_trend_filter_options.len() > 1 {
            let candidate = PolicyConfig {
                macro_trend_filter: !base.macro_trend_filter,
                ..base.clone()
            };
            if self.admits(&candidate) {
                neighbors.push(candidate);
            }
        }
        if self.macro_slope_min_options.len() > 1 {
            push(
                &mut neighbors,
                adjacent_grid_values(&self.macro_slope_min_options, base.macro_slope_min),
                &|macro_slope_min| PolicyConfig {
                    macro_slope_min,
                    ..base.clone()
                },
            );
        }
        if self.short_threshold_delta_options.len() > 1 {
            push(
                &mut neighbors,
                adjacent_grid_values(
                    &self.short_threshold_delta_options,
                    base.short_threshold_delta,
                ),
                &|short_threshold_delta| PolicyConfig {
                    short_threshold_delta,
                    ..base.clone()
                },
            );
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
            "B:0.45|S:0.075|H:0.55|R:0.010|Ri:0.005|Stop:3.0|TP:2.0|BP"
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
            (
                PolicyConfig {
                    take_profit_atr_multiplier: Some(Decimal::ZERO),
                    ..d()
                },
                PolicyConfigError::NonPositiveTakeProfitMultiplier(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    decay_horizon_seconds: Some(0),
                    ..d()
                },
                PolicyConfigError::NonPositiveDecayHorizon(0),
            ),
            (
                PolicyConfig {
                    breakeven_atr_multiplier: Some(Decimal::ZERO),
                    ..d()
                },
                PolicyConfigError::NonPositiveBreakevenMultiplier(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    trailing_atr_multiplier: Some(Decimal::ZERO),
                    ..d()
                },
                PolicyConfigError::NonPositiveTrailingMultiplier(Decimal::ZERO),
            ),
            (
                PolicyConfig {
                    macro_slope_min: dec!(-0.01),
                    ..d()
                },
                PolicyConfigError::NegativeMacroSlopeMin(dec!(-0.01)),
            ),
            (
                PolicyConfig {
                    short_threshold_delta: dec!(-0.01),
                    ..d()
                },
                PolicyConfigError::NegativeShortThresholdDelta(dec!(-0.01)),
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
            && space
                .take_profit_atr_multipliers
                .contains(&c.take_profit_atr_multiplier)
            && space.feed_weights_options.contains(&c.feed_weights)
    }

    #[test]
    fn grid_count_after_the_level_risk_filter_is_pinned() {
        // Risk pairs with base_risk + 2*risk_increment <= 0.05:
        //   increment 0.005, 0.0075, 0.010, 0.0125, 0.015 → all 6 base risks (top <= 0.050);
        //   increment 0.020 → base 0.005, 0.0075, 0.010 only (0.045, 0.0475, 0.050; others exceed).
        // 5 * 6 + 3 = 33 pairs.
        // 10 bases · 5 steps · 9 hysteresis · 33 risk pairs · 9 stops · 9 TPs = 1,202,850 candidates.
        let space = SwarmPolicySpace::default();
        let all = space.generate_candidates(usize::MAX);
        assert_eq!(all.len(), 1_202_850);
        assert!(all.iter().all(|c| c.top_level_risk() <= dec!(0.05)));
        assert!(all.contains(&PolicyConfig::default()));
    }

    #[test]
    fn candidates_are_on_the_grid_valid_and_deterministic() {
        let space = SwarmPolicySpace::default();
        let candidates = space.generate_candidates(1000);
        assert!(candidates
            .iter()
            .all(|c| on_grid(&space, c) && space.admits(c)));
        let a = space.generate_candidates(100);
        let b = space.generate_candidates(100);
        assert_eq!(a.len(), 100);
        assert_eq!(a, b);
        assert!(a.iter().all(|c| candidates.contains(c)));
    }

    #[test]
    fn neighbours_are_on_the_grid_valid_and_adjacent() {
        let space = SwarmPolicySpace::default();
        for c in space.generate_candidates(500) {
            for n in space.perturb_neighbors(&c) {
                assert!(on_grid(&space, &n) && space.admits(&n), "{n} from {c}");
                assert_ne!(n, c);
            }
        }
        // Interior point: 2 neighbours per parameter, except the risk ones the cap removes.
        let base = PolicyConfig {
            threshold_base: dec!(0.45),
            threshold_step: dec!(0.10),
            hysteresis: dec!(0.55),
            base_risk: dec!(0.010),
            risk_increment: dec!(0.010),
            stop_atr_multiplier: dec!(3.0),
            take_profit_atr_multiplier: Some(dec!(3.0)),
            breakeven_atr_multiplier: None,
            trailing_atr_multiplier: None,
            decay_horizon_seconds: None,
            feed_weights: None,
            bracket_priority: true,
            classifier_mode: false,
            macro_trend_filter: false,
            macro_slope_min: Decimal::ZERO,
            short_threshold_delta: Decimal::ZERO,
        };
        let n = space.perturb_neighbors(&base);
        assert_eq!(n.len(), 14);
        assert!(n.contains(&PolicyConfig {
            threshold_base: dec!(0.425),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            threshold_base: dec!(0.475),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            risk_increment: dec!(0.0125),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            stop_atr_multiplier: dec!(2.75),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            stop_atr_multiplier: dec!(3.5),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            take_profit_atr_multiplier: Some(dec!(2.5)),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            take_profit_atr_multiplier: Some(dec!(3.5)),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            hysteresis: dec!(0.45),
            ..base.clone()
        }));
        assert!(n.contains(&PolicyConfig {
            hysteresis: dec!(0.65),
            ..base.clone()
        }));

        let base_none = PolicyConfig {
            take_profit_atr_multiplier: None,
            ..base.clone()
        };
        let n_none = space.perturb_neighbors(&base_none);
        assert_eq!(n_none.len(), 13);
        assert!(n_none.contains(&PolicyConfig {
            take_profit_atr_multiplier: Some(dec!(1.25)),
            ..base_none.clone()
        }));

        // At the risk cap, the step up in either risk parameter is filtered out.
        let capped = PolicyConfig {
            base_risk: dec!(0.010),
            risk_increment: dec!(0.020),
            ..base.clone()
        };
        let n = space.perturb_neighbors(&capped);
        assert_eq!(n.len(), 12);
        assert!(n.iter().all(|c| c.top_level_risk() <= dec!(0.05)));
    }

    #[test]
    fn policy_emits_target_with_take_profit_distance() {
        let config = PolicyConfig {
            take_profit_atr_multiplier: Some(dec!(2.0)),
            ..worked_example_config()
        };
        let mut p = policy(config);
        let Some(Decision::Target(target)) = p.on_sample(&sample(1, 52, 0, dec!(500)), true) else {
            panic!("expected target");
        };
        assert_eq!(target.level(), 1);
        assert_eq!(target.stop_distance(), dec!(1500)); // 3.0 * 500
        assert_eq!(target.take_profit_distance(), Some(dec!(1000))); // 2.0 * 500
    }

    #[test]
    fn policy_forces_flat_exit_after_decay_horizon() {
        let config = PolicyConfig {
            decay_horizon_seconds: Some(3600), // 1 hour
            ..worked_example_config()
        };
        let mut p = policy(config);
        // Level 1 entered at timestamp 10_000
        let out1 = p.on_sample(&sample(10_000, 52, 0, dec!(500)), true);
        assert_eq!(emitted_level(out1), Some(1));

        // Intermediate tick within the horizon: no change emitted
        let out2 = p.on_sample(&sample(10_000 + 1800 * 1000, 52, 0, dec!(500)), true);
        assert_eq!(out2, None);

        // Tick at exactly horizon expiry: forced flat target emitted
        let out3 = p.on_sample(&sample(10_000 + 3600 * 1000, 52, 0, dec!(500)), true);
        assert_eq!(emitted_level(out3), Some(0));
        assert_eq!(p.last_level(), 0);
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

    #[test]
    fn policy_with_feed_weights_and_pruning() {
        // Build a sample with 6 agents per feed (96 total):
        // Feed 0 (time): 6 agents vote long down (-1.0)
        // Feed 4 (tick): 6 agents vote long up (+1.0)
        // Feeds 1..3 and 5..15: 0 votes
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
            atr_stop_reference: dec!(500),
            macro_trend_slope: Decimal::ZERO,
            price: dec!(40000),
        };
        s.feed_long[0] = FeedVote { up: 0, down: 6 };
        s.feed_long[4] = FeedVote { up: 6, down: 0 };

        // 1. Unweighted policy: net vote is 0 -> remains flat (level 0)
        let mut unweighted_policy = ConsensusPolicy::new(PolicyConfig {
            threshold_base: dec!(0.20),
            ..PolicyConfig::default()
        })
        .unwrap();
        assert_eq!(unweighted_policy.on_sample(&s, false), None);
        assert_eq!(unweighted_policy.long_level(), 0);

        // 2. Policy pruning time feeds:
        // Category weights: time=0, tick=1, vol=0, range=0 -> tick feeds get 0.25 each
        let prune_time_weights =
            FeedWeights::from_categories(dec!(0), dec!(1), dec!(0), dec!(0)).unwrap();
        let mut pruned_policy = ConsensusPolicy::new(PolicyConfig {
            threshold_base: dec!(0.20),
            threshold_step: dec!(0.10),
            feed_weights: Some(prune_time_weights),
            ..PolicyConfig::default()
        })
        .unwrap();

        // Feed 4 has +1.0 * 0.25 = +0.25 net vote (between L1=0.20 and L2=0.30) -> triggers level 1!
        let decision = pruned_policy.on_sample(&s, false);
        assert!(decision.is_some());
        assert_eq!(pruned_policy.long_level(), 1);
    }

    #[test]
    fn policy_space_with_feed_weights_exploration() {
        let space = SwarmPolicySpace::default().with_standard_feed_profiles();
        // 6 profiles (None + 5 standard profiles)
        assert_eq!(space.feed_weights_options.len(), 6);

        // Test neighbor transitions on feed weights
        let base = PolicyConfig {
            feed_weights: Some(FeedWeights::equal()),
            ..PolicyConfig::default()
        };
        let neighbors = space.perturb_neighbors(&base);
        // Should have adjacent weight neighbors
        assert!(neighbors.iter().any(|n| n.feed_weights.is_none()));
        assert!(neighbors
            .iter()
            .any(|n| n.feed_weights.is_some() && n.feed_weights != base.feed_weights));
    }

    #[test]
    fn long_entry_triggers_on_threshold() {
        let mut p = ConsensusPolicy::new(PolicyConfig::classifier_default()).unwrap();
        let s_below = sample(1000, 40, 0, dec!(500)); // long_net = 0.40 < 0.45
        assert_eq!(p.on_sample(&s_below, true), None);

        let s_cross = sample(2000, 50, 0, dec!(500)); // long_net = 0.50 >= 0.45
        let decision = p.on_sample(&s_cross, true);
        assert!(decision.is_some());
        let Decision::Entry(intent) = decision.unwrap() else {
            panic!("expected Entry decision");
        };
        assert_eq!(intent.side, domain::Side::Buy);
        assert_eq!(intent.price, dec!(40000));
        assert_eq!(intent.stop_loss, dec!(39500)); // 40000 - 1.0 * 500
        assert_eq!(intent.take_profit, dec!(41000)); // 40000 + 2.0 * 500
        assert_eq!(intent.deadline_ms, Some(2000 + 14400 * 1000));
        assert!(intent.risk_fraction.is_some());

        // While remaining in the active long signal, no duplicate entries are emitted
        let s_still_long = sample(3000, 52, 0, dec!(500));
        assert_eq!(p.on_sample(&s_still_long, true), None);

        // When dropping back to neutral, policy re-arms without emitting
        let s_neutral = sample(4000, 30, 0, dec!(500));
        assert_eq!(p.on_sample(&s_neutral, true), None);

        // Re-crossing threshold triggers a new entry
        let s_recross = sample(5000, 48, 0, dec!(500));
        assert!(matches!(
            p.on_sample(&s_recross, true),
            Some(Decision::Entry(_))
        ));
    }

    #[test]
    fn sizing_increases_with_consensus() {
        let config = PolicyConfig {
            threshold_base: dec!(0.40),
            base_risk: dec!(0.010),
            risk_increment: dec!(0.010),
            classifier_mode: true,
            decay_horizon_seconds: Some(14400),
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            ..PolicyConfig::default()
        };

        // 1. Threshold crossing at theta (0.40): fraction = 0 -> risk = base_risk = 0.010
        let mut p1 = ConsensusPolicy::new(config.clone()).unwrap();
        let Decision::Entry(i1) = p1.on_sample(&sample(1000, 40, 0, dec!(500)), true).unwrap()
        else {
            panic!("expected Entry");
        };
        assert_eq!(i1.risk_fraction, Some(dec!(0.010)));

        // 2. Consensus at 0.70: halfway between 0.40 and 1.00 -> fraction = 0.50 -> risk = 0.015
        let mut p2 = ConsensusPolicy::new(config.clone()).unwrap();
        let Decision::Entry(i2) = p2.on_sample(&sample(1000, 70, 0, dec!(500)), true).unwrap()
        else {
            panic!("expected Entry");
        };
        assert_eq!(i2.risk_fraction, Some(dec!(0.015)));

        // 3. Maximum consensus at 1.00: fraction = 1.00 -> risk = base + inc = 0.020
        let mut p3 = ConsensusPolicy::new(config).unwrap();
        let Decision::Entry(i3) = p3
            .on_sample(&sample(1000, 100, 0, dec!(500)), true)
            .unwrap()
        else {
            panic!("expected Entry");
        };
        assert_eq!(i3.risk_fraction, Some(dec!(0.020)));

        assert!(i1.risk_fraction.unwrap() < i2.risk_fraction.unwrap());
        assert!(i2.risk_fraction.unwrap() < i3.risk_fraction.unwrap());
    }

    #[test]
    fn neutral_consensus_waits() {
        let mut p = ConsensusPolicy::new(PolicyConfig::classifier_default()).unwrap();
        // Sub-threshold neutral
        assert_eq!(p.on_sample(&sample(1000, 20, 10, dec!(500)), true), None);
        // Conflicting consensus (both axes above threshold)
        assert_eq!(p.on_sample(&sample(2000, 50, 50, dec!(500)), true), None);
    }

    #[test]
    fn admits_rejects_inverted_payoff() {
        let space = SwarmPolicySpace::classifier_space();
        let inverted = PolicyConfig {
            classifier_mode: true,
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(1.25)), // 1.25 < 1.5 * 1.0
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        assert!(!space.admits(&inverted));

        let valid = PolicyConfig {
            classifier_mode: true,
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(1.50)), // 1.50 >= 1.5 * 1.0
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        assert!(space.admits(&valid));
    }

    #[test]
    fn admits_rejects_excessive_stop() {
        let space = SwarmPolicySpace::classifier_space();
        let excessive = PolicyConfig {
            classifier_mode: true,
            stop_atr_multiplier: dec!(2.5), // > 2.00 ATR
            take_profit_atr_multiplier: Some(dec!(4.0)),
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        assert!(!space.admits(&excessive));

        let valid = PolicyConfig {
            classifier_mode: true,
            stop_atr_multiplier: dec!(1.75), // <= 2.00 ATR
            take_profit_atr_multiplier: Some(dec!(3.0)),
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        assert!(space.admits(&valid));
    }

    #[test]
    fn admits_rejects_breakeven_above_take_profit() {
        let space = SwarmPolicySpace::classifier_space();
        let invalid = PolicyConfig {
            classifier_mode: true,
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            breakeven_atr_multiplier: Some(dec!(2.5)), // 2.5 >= 2.0
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        assert!(!space.admits(&invalid));

        let valid = PolicyConfig {
            classifier_mode: true,
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            breakeven_atr_multiplier: Some(dec!(0.75)), // 0.75 < 2.0
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        assert!(space.admits(&valid));
    }

    #[test]
    fn classifier_mode_accepts_flat_risk_and_rejects_negative() {
        let flat = PolicyConfig {
            classifier_mode: true,
            risk_increment: Decimal::ZERO,
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        assert!(flat.validate().is_ok());

        let negative = PolicyConfig {
            classifier_mode: true,
            risk_increment: dec!(-0.005),
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        assert_eq!(
            negative.validate(),
            Err(PolicyConfigError::NegativeRiskIncrement(dec!(-0.005)))
        );
    }

    #[test]
    fn classifier_space_candidates_are_valid_and_deterministic() {
        let space = SwarmPolicySpace::classifier_space();
        let candidates = space.generate_candidates(500);
        assert!(!candidates.is_empty());
        assert!(candidates.iter().all(|c| {
            space.admits(c)
                && c.classifier_mode
                && c.stop_atr_multiplier <= dec!(2.00)
                && c.take_profit_atr_multiplier.unwrap() >= dec!(1.5) * c.stop_atr_multiplier
                && c.decay_horizon_seconds.unwrap() <= 86400
        }));

        let c1 = space.generate_candidates(50);
        let c2 = space.generate_candidates(50);
        assert_eq!(c1, c2);
    }

    #[test]
    fn macro_trend_filter_blocks_counter_trend_in_classifier_mode() {
        let config = PolicyConfig {
            classifier_mode: true,
            macro_trend_filter: true,
            threshold_base: dec!(0.50),
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        let mut policy = ConsensusPolicy::new(config).unwrap();

        // 1. Bullish consensus (long_net = 0.60) but bearish macro trend (slope = -0.05) -> blocked!
        let mut s1 = sample(1000, 60, 0, dec!(500));
        s1.macro_trend_slope = dec!(-0.05);
        assert_eq!(policy.on_sample(&s1, true), None);

        // 2. Bullish consensus with bullish macro trend (slope = +0.05) -> permitted!
        let mut s2 = sample(2000, 60, 0, dec!(500));
        s2.macro_trend_slope = dec!(0.05);
        let d2 = policy.on_sample(&s2, true);
        assert!(
            matches!(d2, Some(Decision::Entry(ref intent)) if intent.side == domain::Side::Buy)
        );

        // Reset policy for short tests
        policy.reset();

        // 3. Bearish consensus (short_net = 0.60) but bullish macro trend (slope = +0.05) -> blocked!
        let mut s3 = sample(3000, 0, 60, dec!(500));
        s3.macro_trend_slope = dec!(0.05);
        assert_eq!(policy.on_sample(&s3, true), None);

        // 4. Bearish consensus with bearish macro trend (slope = -0.05) -> permitted!
        let mut s4 = sample(4000, 0, 60, dec!(500));
        s4.macro_trend_slope = dec!(-0.05);
        let d4 = policy.on_sample(&s4, true);
        assert!(
            matches!(d4, Some(Decision::Entry(ref intent)) if intent.side == domain::Side::Sell)
        );
    }

    #[test]
    fn macro_trend_filter_blocks_counter_trend_in_target_mode() {
        let config = PolicyConfig {
            classifier_mode: false,
            macro_trend_filter: true,
            threshold_base: dec!(0.40),
            threshold_step: dec!(0.10),
            ..PolicyConfig::default()
        };
        let mut policy = ConsensusPolicy::new(config).unwrap();

        // Bullish net vote (60), but bearish macro trend -> long level forced to 0
        let mut s1 = sample(1000, 60, 0, dec!(500));
        s1.macro_trend_slope = dec!(-0.10);
        policy.on_sample(&s1, true);
        assert_eq!(policy.long_level(), 0);

        // Bullish net vote (60) with bullish macro trend -> long level active (> 0)
        let mut s2 = sample(2000, 60, 0, dec!(500));
        s2.macro_trend_slope = dec!(0.10);
        policy.on_sample(&s2, true);
        assert!(policy.long_level() > 0);
    }

    #[test]
    fn macro_slope_min_filters_choppy_consolidation() {
        let config = PolicyConfig {
            classifier_mode: true,
            macro_trend_filter: true,
            macro_slope_min: dec!(0.02),
            threshold_base: dec!(0.50),
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        let mut policy = ConsensusPolicy::new(config).unwrap();

        // 1. Bullish consensus (long_net = 0.60) but slope is only +0.01 (< 0.02) -> blocked (choppy)!
        let mut s1 = sample(1000, 60, 0, dec!(500));
        s1.macro_trend_slope = dec!(0.01);
        assert_eq!(policy.on_sample(&s1, true), None);

        // 2. Bullish consensus with strong slope (+0.03 >= 0.02) -> admitted!
        let mut s2 = sample(2000, 60, 0, dec!(500));
        s2.macro_trend_slope = dec!(0.03);
        let d2 = policy.on_sample(&s2, true);
        assert!(
            matches!(d2, Some(Decision::Entry(ref intent)) if intent.side == domain::Side::Buy)
        );

        // Reset
        policy.reset();

        // 3. Bearish consensus (short_net = 0.60) but slope is only -0.01 (> -0.02) -> blocked (choppy)!
        let mut s3 = sample(3000, 0, 60, dec!(500));
        s3.macro_trend_slope = dec!(-0.01);
        assert_eq!(policy.on_sample(&s3, true), None);

        // 4. Bearish consensus with strong slope (-0.03 <= -0.02) -> admitted!
        let mut s4 = sample(4000, 0, 60, dec!(500));
        s4.macro_trend_slope = dec!(-0.03);
        let d4 = policy.on_sample(&s4, true);
        assert!(
            matches!(d4, Some(Decision::Entry(ref intent)) if intent.side == domain::Side::Sell)
        );
    }

    #[test]
    fn short_threshold_delta_enforces_asymmetric_conviction() {
        let config = PolicyConfig {
            classifier_mode: true,
            threshold_base: dec!(0.50),
            short_threshold_delta: dec!(0.05), // theta_short = 0.55
            stop_atr_multiplier: dec!(1.0),
            take_profit_atr_multiplier: Some(dec!(2.0)),
            decay_horizon_seconds: Some(14400),
            ..PolicyConfig::default()
        };
        let mut policy = ConsensusPolicy::new(config).unwrap();

        // 1. Long net vote = 0.52 >= theta_long (0.50) -> Long is admitted
        let s_long = sample(1000, 52, 0, dec!(500));
        let d_long = policy.on_sample(&s_long, true);
        assert!(
            matches!(d_long, Some(Decision::Entry(ref intent)) if intent.side == domain::Side::Buy)
        );

        policy.reset();

        // 2. Short net vote = 0.52: >= theta_long (0.50) BUT < theta_short (0.55) -> Short is blocked!
        let s_short_sub = sample(2000, 0, 52, dec!(500));
        assert_eq!(policy.on_sample(&s_short_sub, true), None);

        // 3. Short net vote = 0.56 >= theta_short (0.55) -> Short is admitted!
        let s_short_ok = sample(3000, 0, 56, dec!(500));
        let d_short = policy.on_sample(&s_short_ok, true);
        assert!(
            matches!(d_short, Some(Decision::Entry(ref intent)) if intent.side == domain::Side::Sell)
        );
    }
}
