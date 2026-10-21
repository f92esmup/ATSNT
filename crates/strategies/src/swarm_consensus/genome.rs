//! The genome of every agent and the deterministic sampling of the swarm (strategy spec §7).
//!
//! Each agent samples 11 thresholds once, in the fixed table order of §7.1, from one
//! `StdRng` seeded with `genome_seed`. Normals come from Box–Muller computed in `Decimal`
//! (§7.2), so a seed fully determines the swarm on every platform. Agent `i` watches feed
//! `i / agents_per_feed` (stratified assignment, §7.3).

use domain::{DomainError, EmpiricalPriors};
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use rust_decimal::{Decimal, MathematicalOps};
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Number of bar feeds the swarm watches (§5.1).
pub const FEED_COUNT: usize = 16;

/// Number of genes per agent (§7.1).
pub const GENE_COUNT: usize = 11;

/// Minimum gap between `atr_expansion` and `atr_contraction`, so `High` and `Low` never
/// coexist (§7.1, row 4).
pub const ATR_GAP: Decimal = dec!(0.05);

/// Decimal places every gene is rounded to before clamping (§7.2).
pub const GENE_DECIMALS: u32 = 8;

/// `2^32`, the divisor that maps `next_u32() + 1` into `(0, 1]` (§7.2).
const TWO_POW_32: Decimal = dec!(4294967296);

/// The prior and clamp of one gene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenePrior {
    /// Gene name, as in the genome struct.
    pub name: &'static str,
    /// Mean of the normal prior.
    pub mean: Decimal,
    /// Standard deviation of the normal prior.
    pub std: Decimal,
    /// Lower clamp bound.
    pub min: Decimal,
    /// Upper clamp bound. For `atr_contraction` the effective bound is also at most
    /// `atr_expansion - ATR_GAP`.
    pub max: Decimal,
}

/// The configurable prior parameters of one gene (mean, std, min, max).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorParam {
    /// Mean of the normal prior.
    pub mean: Decimal,
    /// Standard deviation of the normal prior.
    pub std: Decimal,
    /// Lower clamp bound.
    pub min: Decimal,
    /// Upper clamp bound.
    pub max: Decimal,
}

impl PriorParam {
    /// Creates a new prior parameter set.
    pub const fn new(mean: Decimal, std: Decimal, min: Decimal, max: Decimal) -> Self {
        Self {
            mean,
            std,
            min,
            max,
        }
    }

    /// Converts to a `GenePrior` with the given static name.
    pub const fn to_gene_prior(&self, name: &'static str) -> GenePrior {
        GenePrior {
            name,
            mean: self.mean,
            std: self.std,
            min: self.min,
            max: self.max,
        }
    }

    /// Validates internal consistency: std > 0, min < max, and min <= mean <= max.
    pub fn validate(&self, gene_name: &str) -> Result<(), DomainError> {
        if self.std <= Decimal::ZERO {
            return Err(DomainError::InvalidThreshold(format!(
                "gene '{gene_name}' std must be strictly positive, got {}",
                self.std
            )));
        }
        if self.min >= self.max {
            return Err(DomainError::InvalidThreshold(format!(
                "gene '{gene_name}' min ({}) must be strictly less than max ({})",
                self.min, self.max
            )));
        }
        if self.mean < self.min || self.mean > self.max {
            return Err(DomainError::InvalidThreshold(format!(
                "gene '{gene_name}' mean ({}) must be within [min, max] = [{}, {}]",
                self.mean, self.min, self.max
            )));
        }
        Ok(())
    }
}

impl From<GenePrior> for PriorParam {
    fn from(p: GenePrior) -> Self {
        Self {
            mean: p.mean,
            std: p.std,
            min: p.min,
            max: p.max,
        }
    }
}

/// Configurable distribution priors for the 11 genes of the swarm genome (§7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmPriorsConfig {
    pub rsi_overbought: PriorParam,
    pub rsi_oversold: PriorParam,
    pub atr_expansion: PriorParam,
    pub atr_contraction: PriorParam,
    pub vwap_deviation: PriorParam,
    pub alma_slope: PriorParam,
    pub pinbar_long_ratio: PriorParam,
    pub pinbar_short_ratio: PriorParam,
    pub engulfing_ratio: PriorParam,
    pub bos_margin: PriorParam,
    pub insidebar_ratio: PriorParam,
}

impl Default for SwarmPriorsConfig {
    fn default() -> Self {
        Self {
            rsi_overbought: PriorParam::from(GENE_PRIORS[0]),
            rsi_oversold: PriorParam::from(GENE_PRIORS[1]),
            atr_expansion: PriorParam::from(GENE_PRIORS[2]),
            atr_contraction: PriorParam::from(GENE_PRIORS[3]),
            vwap_deviation: PriorParam::from(GENE_PRIORS[4]),
            alma_slope: PriorParam::from(GENE_PRIORS[5]),
            pinbar_long_ratio: PriorParam::from(GENE_PRIORS[6]),
            pinbar_short_ratio: PriorParam::from(GENE_PRIORS[7]),
            engulfing_ratio: PriorParam::from(GENE_PRIORS[8]),
            bos_margin: PriorParam::from(GENE_PRIORS[9]),
            insidebar_ratio: PriorParam::from(GENE_PRIORS[10]),
        }
    }
}

impl SwarmPriorsConfig {
    /// Creates a swarm priors configuration from empirical distribution means,
    /// retaining the standard deviations and min/max clamp bounds of the baseline priors.
    pub fn from_empirical(empirical: &EmpiricalPriors) -> Self {
        let mut config = Self::default();
        config.rsi_overbought.mean = empirical.rsi_overbought_mean;
        config.rsi_oversold.mean = empirical.rsi_oversold_mean;
        config.atr_expansion.mean = empirical.atr_expansion_mean;
        config.atr_contraction.mean = empirical.atr_contraction_mean;
        config.vwap_deviation.mean = empirical.vwap_deviation_mean;
        config.alma_slope.mean = empirical.alma_slope_mean;
        config.pinbar_long_ratio.mean = empirical.pinbar_long_ratio_mean;
        config.pinbar_short_ratio.mean = empirical.pinbar_short_ratio_mean;
        config.engulfing_ratio.mean = empirical.engulfing_ratio_mean;
        config.bos_margin.mean = empirical.bos_margin_mean;
        config.insidebar_ratio.mean = empirical.insidebar_ratio_mean;
        config
    }

    /// Returns the 11 gene priors in spec §7.1 table order.
    pub fn to_gene_priors(&self) -> [GenePrior; GENE_COUNT] {
        [
            self.rsi_overbought.to_gene_prior("rsi_overbought"),
            self.rsi_oversold.to_gene_prior("rsi_oversold"),
            self.atr_expansion.to_gene_prior("atr_expansion"),
            self.atr_contraction.to_gene_prior("atr_contraction"),
            self.vwap_deviation.to_gene_prior("vwap_deviation"),
            self.alma_slope.to_gene_prior("alma_slope"),
            self.pinbar_long_ratio.to_gene_prior("pinbar_long_ratio"),
            self.pinbar_short_ratio.to_gene_prior("pinbar_short_ratio"),
            self.engulfing_ratio.to_gene_prior("engulfing_ratio"),
            self.bos_margin.to_gene_prior("bos_margin"),
            self.insidebar_ratio.to_gene_prior("insidebar_ratio"),
        ]
    }

    /// Validates all 11 gene priors and cross-gene constraints.
    pub fn validate(&self) -> Result<(), DomainError> {
        self.rsi_overbought.validate("rsi_overbought")?;
        self.rsi_oversold.validate("rsi_oversold")?;
        self.atr_expansion.validate("atr_expansion")?;
        self.atr_contraction.validate("atr_contraction")?;
        self.vwap_deviation.validate("vwap_deviation")?;
        self.alma_slope.validate("alma_slope")?;
        self.pinbar_long_ratio.validate("pinbar_long_ratio")?;
        self.pinbar_short_ratio.validate("pinbar_short_ratio")?;
        self.engulfing_ratio.validate("engulfing_ratio")?;
        self.bos_margin.validate("bos_margin")?;
        self.insidebar_ratio.validate("insidebar_ratio")?;

        if self.atr_contraction.min >= self.atr_expansion.min {
            return Err(DomainError::InvalidThreshold(
                "atr_contraction min must be less than atr_expansion min".into(),
            ));
        }
        if self.atr_contraction.mean >= self.atr_expansion.mean {
            return Err(DomainError::InvalidThreshold(
                "atr_contraction mean must be less than atr_expansion mean".into(),
            ));
        }
        if self.rsi_oversold.mean >= self.rsi_overbought.mean {
            return Err(DomainError::InvalidThreshold(
                "rsi_oversold mean must be strictly less than rsi_overbought mean".into(),
            ));
        }
        Ok(())
    }
}

const fn prior(
    name: &'static str,
    mean: Decimal,
    std: Decimal,
    min: Decimal,
    max: Decimal,
) -> GenePrior {
    GenePrior {
        name,
        mean,
        std,
        min,
        max,
    }
}

/// The 11 gene priors of spec §7.1, in table order (which is also the sampling order).
pub const GENE_PRIORS: [GenePrior; GENE_COUNT] = [
    prior("rsi_overbought", dec!(70), dec!(3.5), dec!(50), dec!(85)),
    prior("rsi_oversold", dec!(30), dec!(3.5), dec!(15), dec!(50)),
    prior("atr_expansion", dec!(1.0), dec!(0.2), dec!(0.5), dec!(3.0)),
    prior(
        "atr_contraction",
        dec!(0.5),
        dec!(0.1),
        dec!(0.1),
        dec!(3.0),
    ),
    prior(
        "vwap_deviation",
        dec!(1.0),
        dec!(0.33),
        dec!(0.1),
        dec!(5.0),
    ),
    prior(
        "alma_slope",
        dec!(0.07),
        dec!(0.014),
        dec!(0.005),
        dec!(1.0),
    ),
    prior(
        "pinbar_long_ratio",
        dec!(2.8),
        dec!(0.4),
        dec!(1.5),
        dec!(10),
    ),
    prior(
        "pinbar_short_ratio",
        dec!(0.3),
        dec!(0.05),
        dec!(0.1),
        dec!(1.0),
    ),
    prior(
        "engulfing_ratio",
        dec!(1.15),
        dec!(0.05),
        dec!(1.01),
        dec!(3.0),
    ),
    prior("bos_margin", dec!(0.0), dec!(0.07), dec!(-0.5), dec!(0.5)),
    prior(
        "insidebar_ratio",
        dec!(0.7),
        dec!(0.08),
        dec!(0.1),
        dec!(1.0),
    ),
];

/// Index of `atr_expansion` in [`GENE_PRIORS`].
const ATR_EXPANSION: usize = 2;
/// Index of `atr_contraction` in [`GENE_PRIORS`].
const ATR_CONTRACTION: usize = 3;

/// The 11 thresholds of one agent (§7.1). Every value is adimensional or in ATR units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Genome {
    /// RSI above which momentum is `Overbought`.
    pub rsi_overbought: Decimal,
    /// RSI below which momentum is `Oversold`.
    pub rsi_oversold: Decimal,
    /// ATR ratio above which volatility is `High`.
    pub atr_expansion: Decimal,
    /// ATR ratio below which volatility is `Low`.
    pub atr_contraction: Decimal,
    /// VWAP deviation (ATR units) above which liquidity is `Stretched`.
    pub vwap_deviation: Decimal,
    /// ALMA slope (ATR units per bar) beyond which direction is `Bullish` or `Bearish`.
    pub alma_slope: Decimal,
    /// Minimum rejection wick of a pin bar, in body multiples.
    pub pinbar_long_ratio: Decimal,
    /// Maximum opposite wick of a pin bar, in body multiples.
    pub pinbar_short_ratio: Decimal,
    /// Minimum body ratio of an engulfing bar over the engulfed one.
    pub engulfing_ratio: Decimal,
    /// Break-of-structure margin, in ATR units.
    pub bos_margin: Decimal,
    /// Maximum range of an inside bar as a fraction of the previous range.
    pub insidebar_ratio: Decimal,
}

impl Genome {
    /// Builds a genome from its genes in table order.
    pub fn from_genes(genes: [Decimal; GENE_COUNT]) -> Self {
        let [rsi_overbought, rsi_oversold, atr_expansion, atr_contraction, vwap_deviation, alma_slope, pinbar_long_ratio, pinbar_short_ratio, engulfing_ratio, bos_margin, insidebar_ratio] =
            genes;
        Self {
            rsi_overbought,
            rsi_oversold,
            atr_expansion,
            atr_contraction,
            vwap_deviation,
            alma_slope,
            pinbar_long_ratio,
            pinbar_short_ratio,
            engulfing_ratio,
            bos_margin,
            insidebar_ratio,
        }
    }

    /// The genes in table order.
    pub fn genes(&self) -> [Decimal; GENE_COUNT] {
        [
            self.rsi_overbought,
            self.rsi_oversold,
            self.atr_expansion,
            self.atr_contraction,
            self.vwap_deviation,
            self.alma_slope,
            self.pinbar_long_ratio,
            self.pinbar_short_ratio,
            self.engulfing_ratio,
            self.bos_margin,
            self.insidebar_ratio,
        ]
    }
}

/// One voter of the swarm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Agent id, `0..N`, which is also its sampling order.
    pub id: u32,
    /// Feed the agent subscribes to (`id / agents_per_feed`).
    pub feed: usize,
    /// The agent's thresholds.
    pub genome: Genome,
}

/// The sampled swarm and its clamp audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swarm {
    agents: Vec<Agent>,
    agents_per_feed: u32,
    clamp_hits: [u32; GENE_COUNT],
}

impl Swarm {
    /// Builds a swarm from pre-selected or pruned agents.
    ///
    /// Validates that:
    /// - `agents.len() == agents_per_feed * FEED_COUNT`
    /// - Each agent has `feed == id / agents_per_feed`
    pub fn from_agents(agents: Vec<Agent>, agents_per_feed: u32) -> Result<Self, DomainError> {
        if agents_per_feed == 0 {
            return Err(DomainError::InvalidThreshold(
                "agents_per_feed must be at least 1".to_string(),
            ));
        }
        let expected_total = (agents_per_feed as usize) * FEED_COUNT;
        if agents.len() != expected_total {
            return Err(DomainError::InvalidThreshold(format!(
                "expected {expected_total} agents for agents_per_feed {agents_per_feed}, got {}",
                agents.len()
            )));
        }
        let per_feed = agents_per_feed as usize;
        for (i, agent) in agents.iter().enumerate() {
            let expected_feed = i / per_feed;
            if agent.feed != expected_feed {
                return Err(DomainError::InvalidThreshold(format!(
                    "agent at index {i} has feed {}, expected {expected_feed}",
                    agent.feed
                )));
            }
        }
        Ok(Self {
            agents,
            agents_per_feed,
            clamp_hits: [0u32; GENE_COUNT],
        })
    }

    /// Agents in id order.
    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    /// Agents per feed.
    pub fn agents_per_feed(&self) -> u32 {
        self.agents_per_feed
    }

    /// Per gene (table order), how many agents had the gene clamped to a bound (§16).
    pub fn clamp_hits(&self) -> &[u32; GENE_COUNT] {
        &self.clamp_hits
    }
}

/// Samples the whole swarm: `16 · agents_per_feed` agents, in id order, genes in table order,
/// from one `StdRng::seed_from_u64(seed)` using default [`GENE_PRIORS`] (§7.2–7.3).
///
/// Errors when `agents_per_feed` is 0.
pub fn sample_swarm(seed: u64, agents_per_feed: u32) -> Result<Swarm, DomainError> {
    sample_swarm_with_priors(seed, agents_per_feed, &GENE_PRIORS)
}

/// Samples the whole swarm with custom priors (§7.1–7.3).
///
/// Errors when `agents_per_feed` is 0.
pub fn sample_swarm_with_priors(
    seed: u64,
    agents_per_feed: u32,
    priors: &[GenePrior; GENE_COUNT],
) -> Result<Swarm, DomainError> {
    if agents_per_feed == 0 {
        return Err(DomainError::InvalidThreshold(
            "agents_per_feed must be at least 1".to_string(),
        ));
    }
    let total = agents_per_feed
        .checked_mul(FEED_COUNT as u32)
        .ok_or_else(|| {
            DomainError::InvalidThreshold(format!("agents_per_feed {agents_per_feed}"))
        })?;
    let mut rng = StdRng::seed_from_u64(seed);
    let mut clamp_hits = [0u32; GENE_COUNT];
    let mut agents = Vec::with_capacity(total as usize);
    for id in 0..total {
        let mut genes = [Decimal::ZERO; GENE_COUNT];
        for (index, prior) in priors.iter().enumerate() {
            let max = if index == ATR_CONTRACTION {
                prior.max.min(genes[ATR_EXPANSION] - ATR_GAP)
            } else {
                prior.max
            };
            let (value, clamped) = draw_gene(&mut rng, prior, max)?;
            genes[index] = value;
            if clamped {
                clamp_hits[index] += 1;
            }
        }
        agents.push(Agent {
            id,
            feed: (id / agents_per_feed) as usize,
            genome: Genome::from_genes(genes),
        });
    }
    Ok(Swarm {
        agents,
        agents_per_feed,
        clamp_hits,
    })
}

/// Draws one gene: `mean + std · z`, rounded to 8 dp, then clamped to `[prior.min, max]`.
/// Returns the value and whether the clamp changed it.
pub(crate) fn draw_gene(
    rng: &mut StdRng,
    prior: &GenePrior,
    max: Decimal,
) -> Result<(Decimal, bool), DomainError> {
    let z = standard_normal(rng)?;
    let raw = (prior.mean + prior.std * z).round_dp(GENE_DECIMALS);
    if raw < prior.min {
        Ok((prior.min, true))
    } else if raw > max {
        Ok((max, true))
    } else {
        Ok((raw, false))
    }
}

/// A uniform in `(0, 1]`: `(next_u32 + 1) / 2^32` (§7.2).
fn uniform(rng: &mut StdRng) -> Decimal {
    (Decimal::from(rng.next_u32()) + Decimal::ONE) / TWO_POW_32
}

/// A standard normal by Box–Muller in `Decimal`: `sqrt(-2·ln u1) · cos(2π·u2)`; the sine branch
/// is discarded (§7.2).
fn standard_normal(rng: &mut StdRng) -> Result<Decimal, DomainError> {
    let u1 = uniform(rng);
    let u2 = uniform(rng);
    let undefined = || DomainError::InvalidThreshold("Box-Muller step undefined".to_string());
    let radius = (dec!(-2) * u1.checked_ln().ok_or_else(undefined)?)
        .sqrt()
        .ok_or_else(undefined)?;
    let angle = (Decimal::TWO_PI * u2).checked_cos().ok_or_else(undefined)?;
    Ok(radius * angle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_swarm_and_another_seed_a_different_one() {
        let a = sample_swarm(123456, 6).unwrap();
        let b = sample_swarm(123456, 6).unwrap();
        let c = sample_swarm(654321, 6).unwrap();
        assert_eq!(a, b);
        assert_ne!(a.agents(), c.agents());
    }

    #[test]
    fn every_feed_gets_exactly_agents_per_feed_agents_by_id_range() {
        for per_feed in [1u32, 6, 7] {
            let swarm = sample_swarm(1, per_feed).unwrap();
            assert_eq!(swarm.agents().len(), FEED_COUNT * per_feed as usize);
            for feed in 0..FEED_COUNT {
                let ids: Vec<u32> = swarm
                    .agents()
                    .iter()
                    .filter(|a| a.feed == feed)
                    .map(|a| a.id)
                    .collect();
                let first = feed as u32 * per_feed;
                assert_eq!(ids, (first..first + per_feed).collect::<Vec<_>>());
            }
        }
        assert!(sample_swarm(1, 0).is_err());
    }

    #[test]
    fn every_gene_is_within_its_clamp_and_contraction_is_below_expansion() {
        let swarm = sample_swarm(123456, 60).unwrap();
        for agent in swarm.agents() {
            let genes = agent.genome.genes();
            for (value, prior) in genes.iter().zip(GENE_PRIORS.iter()) {
                assert!(*value >= prior.min && *value <= prior.max, "{}", prior.name);
                assert_eq!(value.round_dp(GENE_DECIMALS), *value);
            }
            assert!(agent.genome.atr_contraction <= agent.genome.atr_expansion - ATR_GAP);
            assert!(agent.genome.atr_contraction < agent.genome.atr_expansion);
        }
    }

    #[test]
    fn clamp_hits_are_counted_per_gene() {
        let swarm = sample_swarm(123456, 60).unwrap();
        // Recount from the genomes: a clamped gene sits exactly at a bound.
        let mut at_bound = [0u32; GENE_COUNT];
        for agent in swarm.agents() {
            let genes = agent.genome.genes();
            for (index, prior) in GENE_PRIORS.iter().enumerate() {
                let max = if index == ATR_CONTRACTION {
                    prior.max.min(genes[ATR_EXPANSION] - ATR_GAP)
                } else {
                    prior.max
                };
                if genes[index] == prior.min || genes[index] == max {
                    at_bound[index] += 1;
                }
            }
        }
        // bos_margin's bounds are about 7 standard deviations away: never hit.
        assert_eq!(swarm.clamp_hits()[9], 0);
        for (hits, bound) in swarm.clamp_hits().iter().zip(at_bound) {
            assert!(*hits <= bound);
        }
        // 960 agents always clamp some contraction genes to the expansion gap.
        assert!(swarm.clamp_hits()[ATR_CONTRACTION] > 0);
    }

    #[test]
    fn the_sample_mean_is_within_four_standard_errors_over_20000_draws() {
        let mut rng = StdRng::seed_from_u64(42);
        let prior = GENE_PRIORS[0];
        let draws = 20_000u32;
        let mut sum = Decimal::ZERO;
        for _ in 0..draws {
            sum += draw_gene(&mut rng, &prior, prior.max).unwrap().0;
        }
        let mean = sum / Decimal::from(draws);
        let standard_error = prior.std / Decimal::from(draws).sqrt().unwrap();
        assert!(
            (mean - prior.mean).abs() < dec!(4) * standard_error,
            "mean {mean}"
        );
    }

    #[test]
    fn uniforms_are_in_the_half_open_unit_interval() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let u = uniform(&mut rng);
            assert!(u > Decimal::ZERO && u <= Decimal::ONE);
        }
    }

    #[test]
    fn genes_round_trip_through_the_table_order() {
        let genes: [Decimal; GENE_COUNT] = std::array::from_fn(|i| Decimal::from(i as u32));
        assert_eq!(Genome::from_genes(genes).genes(), genes);
        assert_eq!(Genome::from_genes(genes).bos_margin, dec!(9));
    }

    #[test]
    fn swarm_priors_config_default_matches_gene_priors() {
        let default_config = SwarmPriorsConfig::default();
        let priors = default_config.to_gene_priors();
        assert_eq!(priors.len(), GENE_COUNT);
        for (i, p) in priors.iter().enumerate() {
            assert_eq!(p.name, GENE_PRIORS[i].name);
            assert_eq!(p.mean, GENE_PRIORS[i].mean);
            assert_eq!(p.std, GENE_PRIORS[i].std);
            assert_eq!(p.min, GENE_PRIORS[i].min);
            assert_eq!(p.max, GENE_PRIORS[i].max);
        }
        assert!(default_config.validate().is_ok());
    }

    #[test]
    fn swarm_priors_config_serde_roundtrip() {
        let original = SwarmPriorsConfig::default();
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: SwarmPriorsConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn swarm_priors_config_validation_rejects_invalid_values() {
        let mut config = SwarmPriorsConfig::default();
        config.rsi_overbought.std = dec!(0);
        assert!(config.validate().is_err());

        let mut config = SwarmPriorsConfig::default();
        config.rsi_overbought.min = dec!(90);
        config.rsi_overbought.max = dec!(80);
        assert!(config.validate().is_err());

        let mut config = SwarmPriorsConfig::default();
        config.rsi_oversold.mean = dec!(80);
        config.rsi_overbought.mean = dec!(70);
        assert!(config.validate().is_err());
    }

    #[test]
    fn sample_swarm_with_priors_is_deterministic() {
        let priors = SwarmPriorsConfig::default().to_gene_priors();
        let s1 = sample_swarm_with_priors(999, 6, &priors).unwrap();
        let s2 = sample_swarm_with_priors(999, 6, &priors).unwrap();
        let s3 = sample_swarm(999, 6).unwrap();
        assert_eq!(s1.agents(), s2.agents());
        assert_eq!(s1.agents(), s3.agents());
    }

    #[test]
    fn swarm_from_agents_validates_counts_and_feed_ordering() {
        let valid_swarm = sample_swarm(42, 6).unwrap();
        let agents = valid_swarm.agents().to_vec();
        assert_eq!(agents.len(), 96);

        let rebuilt = Swarm::from_agents(agents.clone(), 6).unwrap();
        assert_eq!(rebuilt.agents(), valid_swarm.agents());

        // Zero agents per feed fails
        assert!(Swarm::from_agents(agents.clone(), 0).is_err());

        // Wrong total agents fails
        let mut truncated = agents.clone();
        truncated.pop();
        assert!(Swarm::from_agents(truncated, 6).is_err());

        // Wrong feed fails
        let mut corrupted = agents;
        corrupted[0].feed = 1; // Agent 0 must have feed 0
        assert!(Swarm::from_agents(corrupted, 6).is_err());
    }

    /// No floating-point type appears anywhere in the swarm module (§13 C14, S4).
    #[test]
    fn the_swarm_module_has_no_floating_point_tokens() {
        let forbidden = [concat!("f", "32"), concat!("f", "64")];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/swarm_consensus");
        let mut scanned = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let tokens = source.split(|c: char| !(c.is_alphanumeric() || c == '_'));
            for token in tokens {
                assert!(
                    !forbidden.contains(&token),
                    "{} contains a floating-point token",
                    path.display()
                );
            }
            scanned += 1;
        }
        assert!(scanned >= 7);
    }
}
