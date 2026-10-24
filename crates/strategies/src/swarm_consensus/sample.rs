//! The features the swarm ensemble hands to the consensus policy (strategy spec §9.3).

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::genome::FEED_COUNT;

/// Error when configuring feed weights.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum FeedWeightsError {
    #[error("feed weight at index {0} cannot be negative: {1}")]
    NegativeWeight(usize, Decimal),
    #[error("sum of feed weights must equal 1 (within tolerance 0.0001), got {0}")]
    InvalidSum(Decimal),
    #[error("at least one feed must have a positive weight")]
    AllZeroWeights,
}

/// Normalized weights across the 16 bar feeds for consensus aggregation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedWeights {
    /// Normalized weight for each of the 16 feeds (sum = 1).
    pub weights: [Decimal; FEED_COUNT],
}

impl FeedWeights {
    /// Equal democracy across all 16 feeds (1/16 each = 0.0625).
    pub fn equal() -> Self {
        Self {
            weights: [dec!(0.0625); FEED_COUNT],
        }
    }

    /// Creates feed weights from raw values, ensuring they are non-negative and sum to 1.
    pub fn new(weights: [Decimal; FEED_COUNT]) -> Result<Self, FeedWeightsError> {
        let mut has_positive = false;
        let mut sum = Decimal::ZERO;
        for (i, &w) in weights.iter().enumerate() {
            if w < Decimal::ZERO {
                return Err(FeedWeightsError::NegativeWeight(i, w));
            }
            if w > Decimal::ZERO {
                has_positive = true;
            }
            sum += w;
        }
        if !has_positive {
            return Err(FeedWeightsError::AllZeroWeights);
        }
        if (sum - Decimal::ONE).abs() > dec!(0.0001) {
            return Err(FeedWeightsError::InvalidSum(sum));
        }
        Ok(Self { weights })
    }

    /// Validates the weights.
    pub fn validate(&self) -> Result<(), FeedWeightsError> {
        Self::new(self.weights).map(|_| ())
    }

    /// Weights by the 4 feed categories: Time (0..4), Tick (4..8), Volume (8..12), Range (12..16).
    ///
    /// Each feed in a category receives 1/4 of that category's weight.
    pub fn from_categories(
        time: Decimal,
        tick: Decimal,
        volume: Decimal,
        range: Decimal,
    ) -> Result<Self, FeedWeightsError> {
        if time < Decimal::ZERO {
            return Err(FeedWeightsError::NegativeWeight(0, time));
        }
        if tick < Decimal::ZERO {
            return Err(FeedWeightsError::NegativeWeight(4, tick));
        }
        if volume < Decimal::ZERO {
            return Err(FeedWeightsError::NegativeWeight(8, volume));
        }
        if range < Decimal::ZERO {
            return Err(FeedWeightsError::NegativeWeight(12, range));
        }
        let cat_weights = [
            time / dec!(4),
            tick / dec!(4),
            volume / dec!(4),
            range / dec!(4),
        ];
        let mut weights = [Decimal::ZERO; FEED_COUNT];
        for (cat, &w) in cat_weights.iter().enumerate() {
            let start = cat * 4;
            weights[start..start + 4].fill(w);
        }
        Self::new(weights)
    }

    /// Weights by the 4 feed categories, automatically normalized so their sum is exactly 1.
    pub fn from_categories_normalized(
        time: Decimal,
        tick: Decimal,
        volume: Decimal,
        range: Decimal,
    ) -> Result<Self, FeedWeightsError> {
        if time < Decimal::ZERO {
            return Err(FeedWeightsError::NegativeWeight(0, time));
        }
        if tick < Decimal::ZERO {
            return Err(FeedWeightsError::NegativeWeight(4, tick));
        }
        if volume < Decimal::ZERO {
            return Err(FeedWeightsError::NegativeWeight(8, volume));
        }
        if range < Decimal::ZERO {
            return Err(FeedWeightsError::NegativeWeight(12, range));
        }
        let total = time + tick + volume + range;
        if total.is_zero() {
            return Err(FeedWeightsError::AllZeroWeights);
        }

        let cat_weights = [
            (time / total) / dec!(4),
            (tick / total) / dec!(4),
            (volume / total) / dec!(4),
            (range / total) / dec!(4),
        ];

        let mut weights = [Decimal::ZERO; FEED_COUNT];
        for (cat, &w) in cat_weights.iter().enumerate() {
            let start = cat * 4;
            weights[start..start + 4].fill(w);
        }

        // Adjust last non-zero feed so the sum is exact 1.0 without division rounding drift
        let sum: Decimal = weights.iter().copied().sum();
        let drift = Decimal::ONE - sum;
        if !drift.is_zero() {
            if let Some(last_active) = weights.iter().rposition(|w| !w.is_zero()) {
                weights[last_active] += drift;
            }
        }

        Self::new(weights)
    }

    /// Standard curated weighting and pruning profiles.
    pub fn standard_profiles() -> Vec<Self> {
        let mut profiles = Vec::new();
        // 1. Equal democracy
        profiles.push(Self::equal());
        // 2. Prune time (0% time, 33.33% tick, 33.33% volume, 33.34% range)
        if let Ok(p) = Self::from_categories_normalized(dec!(0), dec!(1), dec!(1), dec!(1)) {
            profiles.push(p);
        }
        // 3. Volume and Range focus (10% time, 10% tick, 40% volume, 40% range)
        if let Ok(p) =
            Self::from_categories_normalized(dec!(0.10), dec!(0.10), dec!(0.40), dec!(0.40))
        {
            profiles.push(p);
        }
        // 4. Tick and Volume focus (10% time, 45% tick, 45% volume, 0% range)
        if let Ok(p) = Self::from_categories_normalized(dec!(0.10), dec!(0.45), dec!(0.45), dec!(0))
        {
            profiles.push(p);
        }
        // 5. Fast Microstructure (5% time, 65% tick, 15% volume, 15% range)
        if let Ok(p) =
            Self::from_categories_normalized(dec!(0.05), dec!(0.65), dec!(0.15), dec!(0.15))
        {
            profiles.push(p);
        }
        profiles
    }
}

impl std::fmt::Display for FeedWeights {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let time: Decimal = self.weights[0..4].iter().copied().sum();
        let tick: Decimal = self.weights[4..8].iter().copied().sum();
        let vol: Decimal = self.weights[8..12].iter().copied().sum();
        let range: Decimal = self.weights[12..16].iter().copied().sum();
        write!(f, "T:{time}|Tk:{tick}|V:{vol}|R:{range}")
    }
}

/// Vote counts for a single bar feed on one directional axis (long or short).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FeedVote {
    pub up: u16,
    pub down: u16,
}

fn default_agents_per_feed() -> u16 {
    6
}

fn default_feed_votes() -> [FeedVote; FEED_COUNT] {
    [FeedVote { up: 0, down: 0 }; FEED_COUNT]
}

/// Votes of the whole swarm right after one or more feeds closed a bar.
///
/// The ensemble emits one sample per trade that closed at least one feed, once every feed is
/// warm. Counts are over all `agents`; an agent's vote persists until its own feed closes again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsensusSample {
    /// Timestamp of the trade that closed the feed(s) (Unix ms).
    pub timestamp: i64,
    /// Number of agents in the swarm (`N`).
    pub agents: u32,
    /// Number of agents per feed (`N / 16`).
    #[serde(default = "default_agents_per_feed")]
    pub agents_per_feed: u16,
    /// Agents voting +1 on the long axis.
    pub long_up: u32,
    /// Agents voting -1 on the long axis.
    pub long_down: u32,
    /// Agents voting +1 on the short axis.
    pub short_up: u32,
    /// Agents voting -1 on the short axis.
    pub short_down: u32,
    /// Agents whose vote is `(0, 0)`.
    pub waits: u32,
    /// Agents whose last evaluation ended in a conflict.
    pub conflicts: u32,
    /// Long votes per feed (16 feeds).
    #[serde(default = "default_feed_votes")]
    pub feed_long: [FeedVote; FEED_COUNT],
    /// Short votes per feed (16 feeds).
    #[serde(default = "default_feed_votes")]
    pub feed_short: [FeedVote; FEED_COUNT],
    /// ATR (price units) of feed 1 (time, 1h), the reference of the disaster stop.
    pub atr_stop_reference: Decimal,
    /// ALMA slope of feed 2 (time 1h, `STOP_REFERENCE_FEED`), in ATR units per bar.
    #[serde(default)]
    pub macro_trend_slope: Decimal,
    /// ALMA slope of feed 3 (time 2h), in ATR units per bar.
    #[serde(default)]
    pub macro_trend_slope_2h: Decimal,
    /// Price of the trade that produced the sample (audit only).
    pub price: Decimal,
}

impl ConsensusSample {
    /// Net long vote as a fraction of the swarm, in `[-1, 1]`; 0 for an empty swarm.
    pub fn long_net(&self) -> Decimal {
        net(self.long_up, self.long_down, self.agents)
    }

    /// Net short vote as a fraction of the swarm, in `[-1, 1]`; 0 for an empty swarm.
    pub fn short_net(&self) -> Decimal {
        net(self.short_up, self.short_down, self.agents)
    }

    /// Weighted net long vote using the provided feed weights, in `[-1, 1]`.
    pub fn weighted_long_net(&self, weights: &FeedWeights) -> Decimal {
        weighted_net(&self.feed_long, self.agents_per_feed, weights)
    }

    /// Weighted net short vote using the provided feed weights, in `[-1, 1]`.
    pub fn weighted_short_net(&self, weights: &FeedWeights) -> Decimal {
        weighted_short_net(&self.feed_short, self.agents_per_feed, weights)
    }
}

fn net(up: u32, down: u32, agents: u32) -> Decimal {
    if agents == 0 {
        return Decimal::ZERO;
    }
    (Decimal::from(up) - Decimal::from(down)) / Decimal::from(agents)
}

fn weighted_net(
    feed_votes: &[FeedVote; FEED_COUNT],
    agents_per_feed: u16,
    weights: &FeedWeights,
) -> Decimal {
    if agents_per_feed == 0 {
        return Decimal::ZERO;
    }
    let per_feed = Decimal::from(agents_per_feed);
    let mut weighted_sum = Decimal::ZERO;
    for (feed, w) in weights.weights.iter().enumerate() {
        if w.is_zero() {
            continue;
        }
        let vote = &feed_votes[feed];
        let diff = Decimal::from(vote.up as i32 - vote.down as i32);
        weighted_sum += *w * diff;
    }
    weighted_sum / per_feed
}

fn weighted_short_net(
    feed_votes: &[FeedVote; FEED_COUNT],
    agents_per_feed: u16,
    weights: &FeedWeights,
) -> Decimal {
    weighted_net(feed_votes, agents_per_feed, weights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample(long_up: u32, long_down: u32, short_up: u32, short_down: u32) -> ConsensusSample {
        let mut feed_long = [FeedVote::default(); FEED_COUNT];
        let mut feed_short = [FeedVote::default(); FEED_COUNT];
        // Distribute votes evenly across 16 feeds (6 agents per feed = 96 total)
        let per_feed = 6;
        let mut remaining_l_up = long_up;
        let mut remaining_l_down = long_down;
        let mut remaining_s_up = short_up;
        let mut remaining_s_down = short_down;
        for f in 0..FEED_COUNT {
            let l_up = remaining_l_up.min(per_feed as u32) as u16;
            remaining_l_up -= l_up as u32;
            let l_down = remaining_l_down.min(per_feed as u32) as u16;
            remaining_l_down -= l_down as u32;
            let s_up = remaining_s_up.min(per_feed as u32) as u16;
            remaining_s_up -= s_up as u32;
            let s_down = remaining_s_down.min(per_feed as u32) as u16;
            remaining_s_down -= s_down as u32;

            feed_long[f] = FeedVote {
                up: l_up,
                down: l_down,
            };
            feed_short[f] = FeedVote {
                up: s_up,
                down: s_down,
            };
        }

        ConsensusSample {
            timestamp: 1,
            agents: 96,
            agents_per_feed: 6,
            long_up,
            long_down,
            short_up,
            short_down,
            waits: 0,
            conflicts: 0,
            feed_long,
            feed_short,
            atr_stop_reference: dec!(500),
            macro_trend_slope: Decimal::ZERO,
            macro_trend_slope_2h: Decimal::ZERO,
            price: dec!(40000),
        }
    }

    #[test]
    fn net_votes_are_fractions_of_the_swarm() {
        let s = sample(60, 12, 6, 30);
        assert_eq!(s.long_net(), dec!(0.5));
        assert_eq!(s.short_net(), dec!(-0.25));
    }

    #[test]
    fn an_empty_swarm_has_no_net_vote() {
        let mut s = sample(0, 0, 0, 0);
        s.agents = 0;
        s.agents_per_feed = 0;
        assert_eq!(s.long_net(), Decimal::ZERO);
        assert_eq!(s.weighted_long_net(&FeedWeights::equal()), Decimal::ZERO);
    }

    #[test]
    fn equal_weights_produce_exact_same_net_vote_as_unweighted() {
        // Equal distribution case
        let s = sample(48, 16, 16, 48);
        let equal_weights = FeedWeights::equal();
        assert_eq!(s.weighted_long_net(&equal_weights), s.long_net());
        assert_eq!(s.weighted_short_net(&equal_weights), s.short_net());
    }

    #[test]
    fn feed_pruning_zeroes_out_pruned_feeds() {
        let mut s = sample(0, 0, 0, 0);
        // Feed 0 (time): 6 agents all vote long down (-1.0)
        s.feed_long[0] = FeedVote { up: 0, down: 6 };
        // Feed 4 (tick): 6 agents all vote long up (+1.0)
        s.feed_long[4] = FeedVote { up: 6, down: 0 };
        s.long_up = 6;
        s.long_down = 6;

        // Equal weights: Net vote cancels out to 0
        let equal_weights = FeedWeights::equal();
        assert_eq!(s.weighted_long_net(&equal_weights), Decimal::ZERO);

        // Pruning time feeds: Feed 0 has weight 0. Feed 4 has positive weight.
        // Categories: time=0, tick=1, vol=0, range=0 -> tick feeds get 1/4 = 0.25 each.
        let prune_time = FeedWeights::from_categories(dec!(0), dec!(1), dec!(0), dec!(0)).unwrap();
        // Feed 4 alone contributes +1.0 * 0.25 = +0.25
        assert_eq!(s.weighted_long_net(&prune_time), dec!(0.25));
    }

    #[test]
    fn feed_weights_validation_and_normalization() {
        assert!(FeedWeights::equal().validate().is_ok());

        // Negative weight fails
        assert!(FeedWeights::from_categories(dec!(-0.1), dec!(0.5), dec!(0.3), dec!(0.3)).is_err());

        // Unnormalized categories with sum != 1 fails in from_categories
        assert!(FeedWeights::from_categories(dec!(1), dec!(1), dec!(1), dec!(1)).is_err());

        // But succeeds in from_categories_normalized
        let norm =
            FeedWeights::from_categories_normalized(dec!(1), dec!(1), dec!(1), dec!(1)).unwrap();
        assert_eq!(norm, FeedWeights::equal());

        // Standard profiles are all valid
        let profiles = FeedWeights::standard_profiles();
        assert_eq!(profiles.len(), 5);
        for p in profiles {
            assert!(p.validate().is_ok());
        }
    }
}
