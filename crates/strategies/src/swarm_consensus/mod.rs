//! Swarm consensus strategy: 96 rule-based agents over 16 bar feeds vote, and the net vote
//! drives a signed exposure level with hysteresis (strategy spec
//! `docs/strategy/03-swarm-consensus.md`).

pub mod agent;
pub mod audit;
pub mod ensemble;
pub mod genome;
pub mod policy;
pub mod rules;
pub mod sample;

pub use audit::SwarmAudit;
pub use ensemble::{SwarmConfig, SwarmEnsemble};
pub use genome::{sample_swarm, Agent, Genome, Swarm};
pub use policy::{axis_update, ConsensusPolicy, PolicyConfig, PolicyConfigError, SwarmPolicySpace};
pub use rules::{AgentVote, ConflictKind};
pub use sample::ConsensusSample;

use domain::DomainError;

use crate::strategy::{
    Decision, FeatureExtractor, InputKind, MarketEvent, MarketType, OptimizableStrategy, Strategy,
};

/// The swarm consensus strategy: the [`SwarmEnsemble`] (frozen first stage) feeding the
/// [`ConsensusPolicy`] (searched decision stage). It consumes raw trades and emits
/// `Decision::Target`s.
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
        &[MarketType::UsdMPerpetual]
    }

    fn input_kind(&self) -> InputKind {
        InputKind::Trades
    }

    /// The two stages in one call, so the live path and the optimizer cannot drift apart.
    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<Decision> {
        let sample = self.ensemble.on_event(event)?;
        self.on_features(&sample)
    }

    fn reset(&mut self) {
        self.ensemble.reset();
        self.policy.reset();
    }

    fn audit(&self) -> Option<serde_json::Value> {
        Some(self.ensemble.audit().to_json())
    }

    fn emits_targets(&self) -> bool {
        true
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

    fn on_features(&mut self, sample: &ConsensusSample) -> Option<Decision> {
        self.policy.on_sample(sample, self.allow_short)
    }

    fn default_space() -> SwarmPolicySpace {
        SwarmPolicySpace::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::canonical_id;
    use crate::strategy::{InputKind, OptimizableStrategy, Strategy};

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
        assert_eq!(strategy.input_kind(), InputKind::Trades);
        let audit = strategy.audit().expect("the swarm reports an audit");
        assert!(audit.is_object());
    }

    #[test]
    fn the_swarm_declares_that_it_emits_targets_also_through_a_box() {
        let strategy =
            SwarmConsensusStrategy::from_config(&SwarmConfig::default(), PolicyConfig::default())
                .unwrap();
        assert!(strategy.emits_targets());
        let boxed: Box<dyn Strategy> = Box::new(strategy);
        assert!(boxed.emits_targets());
    }

    #[test]
    fn an_invalid_policy_configuration_is_rejected() {
        let config = PolicyConfig {
            threshold_base: rust_decimal::Decimal::ZERO,
            ..PolicyConfig::default()
        };
        assert!(SwarmConsensusStrategy::from_config(&SwarmConfig::default(), config).is_err());
    }
}
