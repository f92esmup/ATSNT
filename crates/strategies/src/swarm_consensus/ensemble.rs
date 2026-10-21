//! Stage A of the swarm: 16 bar feeds, their indicators, the agents, and the persistent vote
//! table (strategy spec §5, §6.5, §9).
//!
//! For every trade, every feed aggregator (ids 0 to 15, in order) ingests it; every feed that
//! closed a bar updates its window and indicators and re-evaluates **only** its agents; then, if
//! at least one feed closed and every feed is warm, one [`ConsensusSample`] is emitted. Votes
//! persist until the agent's own feed closes again.

use std::collections::VecDeque;

use domain::{
    BarAggregator, BarInput, DomainError, FeedBar, FeedIndicators, FeedThresholds,
    IndicatorSnapshot, RangeBarAggregator, TickBarAggregator, TimeBarAggregator, Trade,
    VolumeBarAggregator,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use super::audit::SwarmAudit;
use super::genome::{
    sample_swarm, sample_swarm_with_priors, Agent, Swarm, SwarmPriorsConfig, FEED_COUNT,
};
use super::rules::{evaluate_agent, rule_book, AgentVote, ConflictKind, Rule};
use super::sample::{ConsensusSample, FeedVote};
use crate::strategy::{FeatureExtractor, MarketEvent};

/// Horizons of the four time feeds (ids 0–3): 15m, 30m, 1h, 2h.
pub const TIME_HORIZONS_MS: [i64; 4] = [900_000, 1_800_000, 3_600_000, 7_200_000];

/// Feed whose ATR is the reference of the disaster stop: time 1h (id 2).
pub const STOP_REFERENCE_FEED: usize = 2;

/// Structural configuration of the swarm (Stage A, frozen during HPO; spec §14.1).
///
/// The default thresholds of the tick, volume, and range feeds are **placeholders** of the right
/// order of magnitude for BTCUSDT aggTrades; a real run replaces them with the output of
/// [`domain::calibrate`] (see [`SwarmConfig::with_thresholds`]) on a period that ends before the
/// evaluated data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmConfig {
    /// Seed of the swarm's genome sampling.
    pub genome_seed: u64,
    /// Agents per feed; `N = 16 · agents_per_feed`.
    pub agents_per_feed: u32,
    /// Closed bars kept per feed window.
    pub history_size: usize,
    /// aggTrades per bar, feeds 4–7.
    pub tick_thresholds: [u64; 4],
    /// Base volume per bar, feeds 8–11.
    pub volume_thresholds: [Decimal; 4],
    /// Relative range `(high - low) / open` per bar, feeds 12–15.
    pub range_thresholds: [Decimal; 4],
    /// Last timestamp used to calibrate the thresholds (Unix ms).
    pub calibration_end_ms: i64,
    /// `false` reproduces the thesis's long-only axis.
    pub allow_short: bool,
    /// Optional configurable gene priors for sampling (§7.1). If `None`, uses default [`GENE_PRIORS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priors: Option<SwarmPriorsConfig>,
    /// Optional pre-selected or pruned agent population. If `Some`, overrides sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_agents: Option<Vec<Agent>>,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            genome_seed: 123_456,
            agents_per_feed: 6,
            history_size: 50,
            // Placeholders until calibrated (see the type documentation).
            tick_thresholds: [1_500, 18_000, 72_000, 432_000],
            volume_thresholds: [dec!(25), dec!(300), dec!(1200), dec!(7200)],
            range_thresholds: [dec!(0.002), dec!(0.007), dec!(0.014), dec!(0.035)],
            calibration_end_ms: 0,
            allow_short: true,
            priors: None,
            custom_agents: None,
        }
    }
}

impl SwarmConfig {
    /// Replaces the tick, volume, and range thresholds and the calibration end with calibrated
    /// ones.
    pub fn with_thresholds(mut self, thresholds: &FeedThresholds) -> Self {
        self.tick_thresholds = thresholds.tick;
        self.volume_thresholds = thresholds.volume;
        self.range_thresholds = thresholds.range;
        self.calibration_end_ms = thresholds.calibration_end_ms;
        self
    }

    /// Sets or replaces the configurable gene priors.
    pub fn with_priors(mut self, priors: SwarmPriorsConfig) -> Self {
        self.priors = Some(priors);
        self
    }

    /// Sets or replaces the custom agent population.
    pub fn with_custom_agents(mut self, agents: Vec<Agent>) -> Self {
        self.custom_agents = Some(agents);
        self
    }

    /// Checks that every count and threshold is strictly positive and that the window holds at
    /// least two bars.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.agents_per_feed == 0 {
            return Err(DomainError::InvalidThreshold(
                "agents_per_feed must be at least 1".to_string(),
            ));
        }
        if self.history_size < 2 {
            return Err(DomainError::InvalidWindowSize(self.history_size));
        }
        if let Some(t) = self.tick_thresholds.iter().find(|t| **t == 0) {
            return Err(DomainError::InvalidThreshold(format!("tick threshold {t}")));
        }
        let decimals = self
            .volume_thresholds
            .iter()
            .chain(self.range_thresholds.iter());
        for threshold in decimals {
            if *threshold <= Decimal::ZERO {
                return Err(DomainError::InvalidThreshold(threshold.to_string()));
            }
        }
        if let Some(ref priors) = self.priors {
            priors.validate()?;
        }
        if let Some(ref agents) = self.custom_agents {
            Swarm::from_agents(agents.clone(), self.agents_per_feed)?;
        }
        Ok(())
    }
}

/// One of the four aggregator kinds, so the ensemble stays `Clone` and comparable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FeedAggregator {
    Time(TimeBarAggregator),
    Tick(TickBarAggregator),
    Volume(VolumeBarAggregator),
    Range(RangeBarAggregator),
}

impl FeedAggregator {
    fn process_trade(&mut self, trade: &Trade) -> Option<FeedBar> {
        match self {
            FeedAggregator::Time(a) => a.process_trade(trade),
            FeedAggregator::Tick(a) => a.process_trade(trade),
            FeedAggregator::Volume(a) => a.process_trade(trade),
            FeedAggregator::Range(a) => a.process_trade(trade),
        }
    }

    fn reset(&mut self) {
        match self {
            FeedAggregator::Time(a) => a.reset(),
            FeedAggregator::Tick(a) => a.reset(),
            FeedAggregator::Volume(a) => a.reset(),
            FeedAggregator::Range(a) => a.reset(),
        }
    }
}

/// One feed: its aggregator, its window of closed bars (index 0 = last closed), its indicators,
/// and the snapshot after its last closed bar.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Feed {
    aggregator: FeedAggregator,
    window: VecDeque<FeedBar>,
    indicators: FeedIndicators,
    snapshot: Option<IndicatorSnapshot>,
}

impl Feed {
    fn new(aggregator: FeedAggregator, history_size: usize) -> Result<Self, DomainError> {
        Ok(Self {
            aggregator,
            window: VecDeque::with_capacity(history_size),
            indicators: FeedIndicators::new()?,
            snapshot: None,
        })
    }

    fn push(&mut self, bar: FeedBar, history_size: usize) {
        self.snapshot = self.indicators.update(&BarInput::from(&bar));
        self.window.push_front(bar);
        self.window.truncate(history_size);
    }

    fn reset(&mut self) {
        self.aggregator.reset();
        self.window.clear();
        self.indicators.reset();
        self.snapshot = None;
    }
}

/// The 16 aggregators in the fixed feed order of §5.1.
fn build_feeds(config: &SwarmConfig) -> Result<Vec<Feed>, DomainError> {
    let mut aggregators = Vec::with_capacity(FEED_COUNT);
    for horizon in TIME_HORIZONS_MS {
        aggregators.push(FeedAggregator::Time(TimeBarAggregator::new(horizon)?));
    }
    for count in config.tick_thresholds {
        aggregators.push(FeedAggregator::Tick(TickBarAggregator::new(count)?));
    }
    for volume in config.volume_thresholds {
        aggregators.push(FeedAggregator::Volume(VolumeBarAggregator::new(volume)?));
    }
    for range in config.range_thresholds {
        aggregators.push(FeedAggregator::Range(RangeBarAggregator::new(range)?));
    }
    aggregators
        .into_iter()
        .map(|a| Feed::new(a, config.history_size))
        .collect()
}

/// Stage A of the swarm consensus strategy: trades in, [`ConsensusSample`]s out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmEnsemble {
    config: SwarmConfig,
    swarm: Swarm,
    rules: Vec<Rule>,
    feeds: Vec<Feed>,
    votes: Vec<AgentVote>,
    closed: [bool; FEED_COUNT],
    audit: SwarmAudit,
}

impl SwarmEnsemble {
    /// Validates `config`, samples the swarm, and builds the 16 feeds.
    pub fn new(config: &SwarmConfig) -> Result<Self, DomainError> {
        config.validate()?;
        let swarm = if let Some(ref agents) = config.custom_agents {
            Swarm::from_agents(agents.clone(), config.agents_per_feed)?
        } else if let Some(ref priors) = config.priors {
            sample_swarm_with_priors(
                config.genome_seed,
                config.agents_per_feed,
                &priors.to_gene_priors(),
            )?
        } else {
            sample_swarm(config.genome_seed, config.agents_per_feed)?
        };
        let audit = SwarmAudit::new(&swarm);
        Ok(Self {
            config: config.clone(),
            rules: rule_book(config.allow_short),
            feeds: build_feeds(config)?,
            votes: vec![AgentVote::WAIT; swarm.agents().len()],
            closed: [false; FEED_COUNT],
            swarm,
            audit,
        })
    }

    /// The structural configuration.
    pub fn config(&self) -> &SwarmConfig {
        &self.config
    }

    /// The sampled swarm.
    pub fn swarm(&self) -> &Swarm {
        &self.swarm
    }

    /// The vote table, by agent id.
    pub fn votes(&self) -> &[AgentVote] {
        &self.votes
    }

    /// Which feeds closed a bar on the last trade.
    pub fn last_closed(&self) -> &[bool; FEED_COUNT] {
        &self.closed
    }

    /// The indicator snapshot of a feed after its last closed bar, `None` while it is cold.
    pub fn feed_snapshot(&self, feed: usize) -> Option<&IndicatorSnapshot> {
        self.feeds.get(feed).and_then(|f| f.snapshot.as_ref())
    }

    /// True when every feed is warm: each has an indicator snapshot (§6.5, §9.2).
    pub fn is_ready(&self) -> bool {
        self.feeds.iter().all(|f| f.snapshot.is_some())
    }

    /// The audit counters.
    pub fn audit(&self) -> &SwarmAudit {
        &self.audit
    }

    /// Processes one trade (§9.1) and returns a sample when at least one feed closed and the
    /// swarm is ready.
    pub fn on_trade(&mut self, trade: &Trade) -> Option<ConsensusSample> {
        let history_size = self.config.history_size;
        for (id, feed) in self.feeds.iter_mut().enumerate() {
            self.closed[id] = match feed.aggregator.process_trade(trade) {
                Some(bar) => {
                    feed.push(bar, history_size);
                    true
                }
                None => false,
            };
        }
        let per_feed = self.config.agents_per_feed as usize;
        for (id, feed) in self.feeds.iter_mut().enumerate() {
            if !self.closed[id] {
                continue;
            }
            let window = feed.window.make_contiguous();
            let agents = id * per_feed..(id + 1) * per_feed;
            for agent in &self.swarm.agents()[agents] {
                let evaluation = evaluate_agent(
                    &agent.genome,
                    feed.snapshot.as_ref(),
                    window,
                    &self.rules,
                    self.config.allow_short,
                );
                self.votes[agent.id as usize] = evaluation.vote;
                self.audit.record(&evaluation);
            }
        }
        if !self.closed.iter().any(|c| *c) || !self.is_ready() {
            return None;
        }
        let atr_stop_reference = self.feeds[STOP_REFERENCE_FEED].snapshot?.atr;
        let sample = self.sample(trade, atr_stop_reference);
        self.audit.record_sample(&sample);
        Some(sample)
    }

    fn sample(&self, trade: &Trade, atr_stop_reference: Decimal) -> ConsensusSample {
        let per_feed = self.config.agents_per_feed as usize;
        let mut feed_long = [FeedVote::default(); FEED_COUNT];
        let mut feed_short = [FeedVote::default(); FEED_COUNT];
        let mut long_up = 0u32;
        let mut long_down = 0u32;
        let mut short_up = 0u32;
        let mut short_down = 0u32;
        let mut waits = 0u32;
        let mut conflicts = 0u32;

        for f in 0..FEED_COUNT {
            let start = f * per_feed;
            let end = (start + per_feed).min(self.votes.len());
            let slice = if start < self.votes.len() {
                &self.votes[start..end]
            } else {
                &[]
            };
            let mut f_l_up = 0u16;
            let mut f_l_down = 0u16;
            let mut f_s_up = 0u16;
            let mut f_s_down = 0u16;
            for v in slice {
                if v.long > 0 {
                    f_l_up += 1;
                } else if v.long < 0 {
                    f_l_down += 1;
                }
                if v.short > 0 {
                    f_s_up += 1;
                } else if v.short < 0 {
                    f_s_down += 1;
                }
                if v.long == 0 && v.short == 0 {
                    waits += 1;
                }
                if v.conflict != ConflictKind::None {
                    conflicts += 1;
                }
            }
            feed_long[f] = FeedVote {
                up: f_l_up,
                down: f_l_down,
            };
            feed_short[f] = FeedVote {
                up: f_s_up,
                down: f_s_down,
            };
            long_up += f_l_up as u32;
            long_down += f_l_down as u32;
            short_up += f_s_up as u32;
            short_down += f_s_down as u32;
        }

        ConsensusSample {
            timestamp: trade.timestamp,
            agents: self.votes.len() as u32,
            agents_per_feed: self.config.agents_per_feed as u16,
            long_up,
            long_down,
            short_up,
            short_down,
            waits,
            conflicts,
            feed_long,
            feed_short,
            atr_stop_reference,
            price: trade.price,
        }
    }
}

impl FeatureExtractor for SwarmEnsemble {
    type Features = ConsensusSample;

    /// Accepts only trades; any other event is ignored (§9.3).
    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<ConsensusSample> {
        match event {
            MarketEvent::Trade(trade) => self.on_trade(trade),
            MarketEvent::DollarBar(_) => None,
        }
    }

    fn reset(&mut self) {
        for feed in &mut self.feeds {
            feed.reset();
        }
        self.votes.fill(AgentVote::WAIT);
        self.closed = [false; FEED_COUNT];
        self.audit = SwarmAudit::new(&self.swarm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DollarBar, Side};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    const DAY_MS: i64 = 86_400_000;

    /// Small thresholds so every feed warms within the fixture.
    fn test_config() -> SwarmConfig {
        SwarmConfig {
            tick_thresholds: [1, 2, 3, 4],
            volume_thresholds: [dec!(1), dec!(2), dec!(3), dec!(4)],
            range_thresholds: [dec!(0.001), dec!(0.002), dec!(0.004), dec!(0.008)],
            ..SwarmConfig::default()
        }
    }

    /// A seeded random walk: `per_day` bursts of three trades a day for `days` days, prices times `scale`.
    fn fixture(seed: u64, days: i64, per_day: i64, scale: Decimal) -> Vec<Trade> {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut price = dec!(40000);
        let mut trades = Vec::new();
        for day in 0..days {
            for k in 0..per_day {
                // Bursts of three trades ten minutes apart, so the 5m and 1h feeds differ.
                for burst in 0..3 {
                    let timestamp = day * DAY_MS
                        + k * (DAY_MS / per_day)
                        + burst * 600_000
                        + rng.gen_range(0..1000);
                    // A step in [-1%, +1%] in basis points of the price.
                    let step = Decimal::new(rng.gen_range(-100..=100), 4);
                    price = (price * (Decimal::ONE + step)).round_dp(2);
                    let quantity = Decimal::new(rng.gen_range(50..150), 2);
                    let side = if rng.gen_bool(0.5) {
                        Side::Buy
                    } else {
                        Side::Sell
                    };
                    trades.push(Trade::new(timestamp, price * scale, quantity, side).unwrap());
                }
            }
        }
        trades
    }

    fn standard_fixture() -> Vec<Trade> {
        fixture(7, 75, 8, Decimal::ONE)
    }

    fn run(ensemble: &mut SwarmEnsemble, trades: &[Trade]) -> Vec<Option<ConsensusSample>> {
        trades
            .iter()
            .map(|t| ensemble.on_event(&MarketEvent::Trade(t)))
            .collect()
    }

    #[test]
    fn the_config_defaults_match_the_spec_and_validate() {
        let config = SwarmConfig::default();
        assert_eq!(config.genome_seed, 123_456);
        assert_eq!(config.agents_per_feed, 6);
        assert_eq!(config.history_size, 50);
        assert!(config.allow_short);
        assert!(config.validate().is_ok());
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<SwarmConfig>(&json).unwrap(), config);

        let bad = [
            SwarmConfig {
                agents_per_feed: 0,
                ..config.clone()
            },
            SwarmConfig {
                history_size: 1,
                ..config.clone()
            },
            SwarmConfig {
                tick_thresholds: [1, 0, 1, 1],
                ..config.clone()
            },
            SwarmConfig {
                volume_thresholds: [dec!(1), dec!(1), dec!(0), dec!(1)],
                ..config.clone()
            },
            SwarmConfig {
                range_thresholds: [dec!(-0.1), dec!(1), dec!(1), dec!(1)],
                ..config.clone()
            },
        ];
        for config in bad {
            assert!(SwarmEnsemble::new(&config).is_err(), "{config:?}");
        }
    }

    #[test]
    fn calibrated_thresholds_replace_the_placeholders() {
        let thresholds = FeedThresholds {
            tick: [5, 6, 7, 8],
            volume: [dec!(1), dec!(2), dec!(3), dec!(4)],
            range: [dec!(0.1), dec!(0.2), dec!(0.3), dec!(0.4)],
            calibration_end_ms: 99,
        };
        let config = SwarmConfig::default().with_thresholds(&thresholds);
        assert_eq!(config.tick_thresholds, thresholds.tick);
        assert_eq!(config.volume_thresholds, thresholds.volume);
        assert_eq!(config.range_thresholds, thresholds.range);
        assert_eq!(config.calibration_end_ms, 99);
    }

    #[test]
    fn no_sample_is_emitted_before_every_feed_is_warm() {
        let trades = standard_fixture();
        let mut ensemble = SwarmEnsemble::new(&test_config()).unwrap();
        let mut slowest = TimeBarAggregator::new(TIME_HORIZONS_MS[3]).unwrap();
        let mut slowest_bars = 0;
        let mut first_sample = None;
        for (index, trade) in trades.iter().enumerate() {
            slowest_bars += usize::from(slowest.process_trade(trade).is_some());
            let sample = ensemble.on_event(&MarketEvent::Trade(trade));
            if !ensemble.is_ready() {
                assert!(sample.is_none(), "sample before readiness at {index}");
            }
            if sample.is_some() && first_sample.is_none() {
                first_sample = Some(index);
                // The slowest feed needs 64 closed bars (§6.5).
                assert!(slowest_bars >= 64, "{slowest_bars}");
            }
        }
        assert!(first_sample.is_some_and(|i| i > 0));
    }

    #[test]
    fn a_feed_that_never_warms_blocks_every_sample() {
        let config = SwarmConfig {
            tick_thresholds: [1, 2, 3, 1_000_000],
            ..test_config()
        };
        let mut ensemble = SwarmEnsemble::new(&config).unwrap();
        assert!(run(&mut ensemble, &standard_fixture())
            .iter()
            .all(Option::is_none));
    }

    #[test]
    fn one_sample_per_trade_even_when_several_feeds_close() {
        let trades = standard_fixture();
        let mut ensemble = SwarmEnsemble::new(&test_config()).unwrap();
        let mut multi_close_samples = 0;
        for trade in &trades {
            let sample = ensemble.on_event(&MarketEvent::Trade(trade));
            let closed = ensemble.last_closed().iter().filter(|c| **c).count();
            if ensemble.is_ready() {
                assert_eq!(sample.is_some(), closed > 0);
            }
            if let Some(sample) = sample {
                // The sample reflects the vote table after every closed feed re-voted.
                let votes = ensemble.votes();
                assert_eq!(sample.agents as usize, votes.len());
                assert_eq!(
                    sample.long_up as usize,
                    votes.iter().filter(|v| v.long > 0).count()
                );
                assert_eq!(
                    sample.short_down as usize,
                    votes.iter().filter(|v| v.short < 0).count()
                );
                assert_eq!(sample.timestamp, trade.timestamp);
                assert_eq!(sample.price, trade.price);
                if closed >= 2 {
                    multi_close_samples += 1;
                }
            }
        }
        assert!(multi_close_samples > 0);
    }

    #[test]
    fn votes_persist_until_their_feed_closes() {
        let trades = standard_fixture();
        let mut ensemble = SwarmEnsemble::new(&test_config()).unwrap();
        let per_feed = ensemble.config().agents_per_feed as usize;
        let mut persisted_non_wait = 0;
        for trade in &trades {
            let before = ensemble.votes().to_vec();
            ensemble.on_event(&MarketEvent::Trade(trade));
            for (id, vote) in ensemble.votes().iter().enumerate() {
                if !ensemble.last_closed()[id / per_feed] {
                    assert_eq!(*vote, before[id]);
                    persisted_non_wait += usize::from(*vote != AgentVote::WAIT);
                }
            }
        }
        assert!(persisted_non_wait > 0);
    }

    #[test]
    fn the_stop_reference_is_the_atr_of_the_hourly_feed() {
        let trades = standard_fixture();
        let mut ensemble = SwarmEnsemble::new(&test_config()).unwrap();
        let mut hourly = TimeBarAggregator::new(3_600_000).unwrap();
        let mut indicators = FeedIndicators::new().unwrap();
        let mut hourly_atr = None;
        let mut samples = 0;
        for trade in &trades {
            if let Some(bar) = hourly.process_trade(trade) {
                hourly_atr = indicators.update(&BarInput::from(&bar)).map(|s| s.atr);
            }
            if let Some(sample) = ensemble.on_event(&MarketEvent::Trade(trade)) {
                assert_eq!(Some(sample.atr_stop_reference), hourly_atr);
                samples += 1;
            }
        }
        assert!(samples > 0);
    }

    #[test]
    fn prices_times_ten_give_identical_votes() {
        let config = test_config();
        let base = fixture(11, 75, 8, Decimal::ONE);
        let scaled = fixture(11, 75, 8, dec!(10));
        let mut a = SwarmEnsemble::new(&config).unwrap();
        let mut b = SwarmEnsemble::new(&config).unwrap();
        let mut samples = 0;
        let mut non_wait = 0;
        for (x, y) in base.iter().zip(&scaled) {
            let sa = a.on_event(&MarketEvent::Trade(x));
            let sb = b.on_event(&MarketEvent::Trade(y));
            assert_eq!(a.votes(), b.votes());
            non_wait += a.votes().iter().filter(|v| **v != AgentVote::WAIT).count();
            assert_eq!(sa.is_some(), sb.is_some());
            if let (Some(sa), Some(sb)) = (sa, sb) {
                assert_eq!(sb.price, sa.price * dec!(10));
                let ratio = sb.atr_stop_reference / sa.atr_stop_reference;
                assert!((ratio - dec!(10)).abs() < dec!(0.0000000001));
                let counts = |s: &ConsensusSample| {
                    (
                        s.timestamp,
                        s.long_up,
                        s.long_down,
                        s.short_up,
                        s.short_down,
                        s.waits,
                        s.conflicts,
                    )
                };
                assert_eq!(counts(&sa), counts(&sb));
                samples += 1;
            }
        }
        assert!(samples > 0 && non_wait > 0);
    }

    #[test]
    fn replay_is_deterministic() {
        let trades = standard_fixture();
        let mut a = SwarmEnsemble::new(&test_config()).unwrap();
        let mut b = SwarmEnsemble::new(&test_config()).unwrap();
        assert_eq!(run(&mut a, &trades), run(&mut b, &trades));
        assert_eq!(a.audit().to_json(), b.audit().to_json());
        assert_eq!(a, b);
    }

    #[test]
    fn reset_equals_fresh() {
        let trades = standard_fixture();
        let fresh = SwarmEnsemble::new(&test_config()).unwrap();
        let mut used = fresh.clone();
        run(&mut used, &trades[..trades.len() / 2]);
        used.reset();
        assert_eq!(used, fresh);
        let mut again = SwarmEnsemble::new(&test_config()).unwrap();
        assert_eq!(run(&mut used, &trades), run(&mut again, &trades));
    }

    #[test]
    fn non_trade_events_are_ignored() {
        let mut ensemble = SwarmEnsemble::new(&test_config()).unwrap();
        let fresh = ensemble.clone();
        let one = dec!(1);
        let bar = DollarBar {
            start_time: 0,
            end_time: 1,
            open: one,
            high: one,
            low: one,
            close: one,
            volume: one,
            dollar_volume: one,
            trade_count: 1,
            buy_volume: one,
            sell_volume: Decimal::ZERO,
            buy_dollar_volume: one,
            sell_dollar_volume: Decimal::ZERO,
        };
        assert!(ensemble.on_event(&MarketEvent::DollarBar(&bar)).is_none());
        assert_eq!(ensemble, fresh);
    }

    #[test]
    fn audit_counters_sum_to_the_agent_evaluations() {
        let trades = standard_fixture();
        let config = test_config();
        let mut ensemble = SwarmEnsemble::new(&config).unwrap();
        // Count the evaluations independently: agents_per_feed per closed bar of every feed.
        let mut feeds = build_feeds(&config).unwrap();
        let mut closes = 0u64;
        let mut samples = 0u64;
        for trade in &trades {
            for feed in &mut feeds {
                closes += u64::from(feed.aggregator.process_trade(trade).is_some());
            }
            samples += u64::from(ensemble.on_event(&MarketEvent::Trade(trade)).is_some());
        }
        let audit = ensemble.audit();
        let expected = closes * u64::from(config.agents_per_feed);
        assert_eq!(audit.evaluations(), expected);
        let votes: u64 = audit.vote_counts().iter().flatten().sum();
        assert_eq!(votes, expected);
        let conflicts: u64 = [
            ConflictKind::None,
            ConflictKind::WithinAxis,
            ConflictKind::Directional,
            ConflictKind::Both,
        ]
        .into_iter()
        .map(|k| audit.conflict_count(k))
        .sum();
        assert_eq!(conflicts, expected);
        assert!(audit.cold_evaluations() + audit.noise_evaluations() <= expected);
        assert!(audit.cold_evaluations() > 0);
        assert_eq!(audit.samples(), samples);
        let json = audit.to_json();
        assert_eq!(json["evaluations"], serde_json::json!(expected));
        assert!(json["rule_firings"].as_object().unwrap().len() == 18);
    }

    #[test]
    fn long_only_swarms_never_vote_short() {
        let config = SwarmConfig {
            allow_short: false,
            ..test_config()
        };
        let mut ensemble = SwarmEnsemble::new(&config).unwrap();
        for sample in run(&mut ensemble, &standard_fixture())
            .into_iter()
            .flatten()
        {
            assert_eq!((sample.short_up, sample.short_down), (0, 0));
        }
    }

    #[test]
    fn ensemble_supports_custom_priors_and_custom_agents() {
        let priors = SwarmPriorsConfig::default();
        let config_with_priors = test_config().with_priors(priors);
        let ensemble_priors = SwarmEnsemble::new(&config_with_priors).unwrap();
        assert_eq!(ensemble_priors.swarm().agents().len(), 96);

        // Pre-sampled custom agents
        let agents = ensemble_priors.swarm().agents().to_vec();
        let config_with_agents = test_config().with_custom_agents(agents);
        let ensemble_custom = SwarmEnsemble::new(&config_with_agents).unwrap();
        assert_eq!(
            ensemble_custom.swarm().agents(),
            ensemble_priors.swarm().agents()
        );
    }
}
