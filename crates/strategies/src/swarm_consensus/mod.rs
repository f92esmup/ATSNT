//! Swarm consensus strategy: 96 rule-based agents over 16 bar feeds vote, and a net vote that
//! clears its threshold opens a triple-barrier entry (strategy spec
//! `docs/strategy/03-swarm-consensus.md`).

pub mod agent;
pub mod audit;
pub mod calibration;
pub mod ensemble;
pub mod genome;
pub mod indicators;
pub mod policy;
pub mod priors_calibration;
pub mod rules;
pub mod sample;

pub use audit::SwarmAudit;
pub use calibration::{calibrate, FeedThresholds};
pub use ensemble::{SwarmConfig, SwarmEnsemble};
pub use genome::{
    sample_swarm, sample_swarm_with_priors, Agent, GenePrior, Genome, PriorParam, Swarm,
    SwarmPriorsConfig, GENE_COUNT, GENE_PRIORS,
};
pub use indicators::{FeedIndicators, IndicatorSnapshot};
pub use policy::{ConsensusPolicy, PolicyConfig, PolicyConfigError, SwarmPolicySpace};
pub use priors_calibration::{calibrate_empirical_priors, EmpiricalPriors};
pub use rules::{AgentVote, ConflictKind};
pub use sample::{ConsensusSample, FeedVote, FeedWeights, FeedWeightsError};

use domain::DomainError;

use crate::strategy::{
    Decision, FeatureExtractor, MarketEvent, MarketType, OptimizableStrategy, Strategy,
    StrategyView,
};

/// Lead the data must give the swarm before its first evaluated decision (strategy spec §15.2):
/// 70 days, so the slowest feed (1 day, 64 closed bars) is warm.
pub const SWARM_WARM_UP_MS: i64 = 70 * 86_400_000;

/// The swarm consensus strategy: the [`SwarmEnsemble`] (frozen first stage) feeding the
/// [`ConsensusPolicy`] (searched decision stage). It consumes raw trades and emits
/// triple-barrier entries.
#[derive(Debug, Clone)]
pub struct SwarmConsensusStrategy {
    ensemble: SwarmEnsemble,
    policy: ConsensusPolicy,
    allow_short: bool,
}

impl Strategy for SwarmConsensusStrategy {
    fn name(&self) -> &str {
        Self::id()
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        &[MarketType::Spot, MarketType::UsdMPerpetual]
    }

    /// The two stages in one call, so the live path and the optimizer cannot drift apart.
    fn on_event(&mut self, event: &MarketEvent<'_>, view: &StrategyView, out: &mut Vec<Decision>) {
        if let Some(sample) = self.ensemble.on_event(event) {
            self.on_features(&sample, view, out);
        }
    }

    fn reset(&mut self) {
        self.ensemble.reset();
        self.policy.reset();
    }

    fn audit(&self) -> Option<serde_json::Value> {
        Some(self.ensemble.audit().to_json())
    }
}

impl OptimizableStrategy for SwarmConsensusStrategy {
    type SharedConfig = SwarmConfig;
    type Config = PolicyConfig;
    type Space = SwarmPolicySpace;
    type Extractor = SwarmEnsemble;

    fn id() -> &'static str {
        "swarm_consensus"
    }

    fn from_config(shared: &SwarmConfig, config: PolicyConfig) -> Result<Self, DomainError> {
        let policy = ConsensusPolicy::new(config)
            .map_err(|error| DomainError::InvalidThreshold(error.to_string()))?;
        Ok(Self {
            ensemble: SwarmEnsemble::new(shared)?,
            policy,
            allow_short: shared.allow_short,
        })
    }

    fn extractor(shared: &SwarmConfig) -> Result<SwarmEnsemble, DomainError> {
        SwarmEnsemble::new(shared)
    }

    /// The swarm decides entries only; its barriers close the position.
    fn on_features(
        &mut self,
        sample: &ConsensusSample,
        _view: &StrategyView,
        out: &mut Vec<Decision>,
    ) {
        out.extend(
            self.policy
                .on_sample(sample, self.allow_short)
                .map(Decision::Enter),
        );
    }

    fn default_space() -> SwarmPolicySpace {
        SwarmPolicySpace::default()
    }

    fn warm_up_ms(_shared: &SwarmConfig) -> i64 {
        SWARM_WARM_UP_MS
    }

    /// The feed thresholds are calibrated on data that ends here (strategy spec §14.1).
    fn calibration_end_ms(shared: &SwarmConfig) -> Option<i64> {
        Some(shared.calibration_end_ms)
    }

    /// The default thresholds are not calibrated to any market, so an optimisation must be given
    /// the calibrated ones.
    fn requires_shared_config() -> bool {
        true
    }

    /// The default feed thresholds are sized for BTCUSDT and never close a bar on the contract's
    /// synthetic walk: small thresholds and a low vote threshold make the swarm decide on it.
    fn contract_fixture() -> Option<(SwarmConfig, PolicyConfig)> {
        let shared = SwarmConfig {
            tick_thresholds: [1, 2, 3, 4],
            volume_thresholds: [
                rust_decimal::Decimal::ONE,
                rust_decimal::Decimal::TWO,
                rust_decimal::Decimal::from(3),
                rust_decimal::Decimal::from(4),
            ],
            range_thresholds: [
                rust_decimal::Decimal::new(1, 3),
                rust_decimal::Decimal::new(2, 3),
                rust_decimal::Decimal::new(4, 3),
                rust_decimal::Decimal::new(8, 3),
            ],
            ..SwarmConfig::default()
        };
        let policy = PolicyConfig {
            threshold_base: rust_decimal::Decimal::new(5, 2),
            ..PolicyConfig::default()
        };
        Some((shared, policy))
    }

    /// A spot account cannot sell what it does not hold, so on spot the swarm is long only.
    fn shared_for_market(shared: SwarmConfig, market: MarketType) -> SwarmConfig {
        match market {
            MarketType::Spot => SwarmConfig {
                allow_short: false,
                ..shared
            },
            MarketType::UsdMPerpetual => shared,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::canonical_id;
    use crate::strategy::{OptimizableStrategy, Strategy};

    #[test]
    fn the_swarm_is_registered_under_its_id_and_alias() {
        assert_eq!(canonical_id("swarm_consensus"), Some("swarm_consensus"));
        assert_eq!(canonical_id("swarm"), Some("swarm_consensus"));
    }

    #[test]
    fn the_swarm_consumes_trades_and_reports_an_audit() {
        let strategy =
            SwarmConsensusStrategy::from_config(&SwarmConfig::default(), PolicyConfig::default())
                .unwrap();
        let audit = strategy.audit().expect("the swarm reports an audit");
        assert!(audit.is_object());
    }

    #[test]
    fn the_swarm_declares_its_warm_up_and_calibration() {
        let shared = SwarmConfig {
            calibration_end_ms: 42,
            ..SwarmConfig::default()
        };
        assert_eq!(SwarmConsensusStrategy::warm_up_ms(&shared), 70 * 86_400_000);
        assert_eq!(
            SwarmConsensusStrategy::calibration_end_ms(&shared),
            Some(42)
        );
        assert!(SwarmConsensusStrategy::requires_shared_config());
    }

    #[test]
    fn an_invalid_policy_configuration_is_rejected() {
        let config = PolicyConfig {
            threshold_base: rust_decimal::Decimal::ZERO,
            ..PolicyConfig::default()
        };
        assert!(SwarmConsensusStrategy::from_config(&SwarmConfig::default(), config).is_err());
    }

    #[test]
    fn the_swarm_trades_both_markets_and_is_long_only_on_spot() {
        let strategy =
            SwarmConsensusStrategy::from_config(&SwarmConfig::default(), PolicyConfig::default())
                .unwrap();
        assert!(strategy.supports_market_type(MarketType::Spot));
        assert!(strategy.supports_market_type(MarketType::UsdMPerpetual));

        let shorting = SwarmConfig::default();
        assert!(shorting.allow_short);
        let on_spot = SwarmConsensusStrategy::shared_for_market(shorting.clone(), MarketType::Spot);
        assert!(!on_spot.allow_short);
        assert_eq!(
            SwarmConsensusStrategy::shared_for_market(shorting.clone(), MarketType::UsdMPerpetual),
            shorting
        );
    }
}
