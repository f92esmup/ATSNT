// Test-only: the generic strategy contract suite, run by the registry tests. Its panics are
// assertions, so it is never compiled into production code.
#[cfg(test)]
mod contract;
pub mod registry;
pub mod strategy;
pub mod swarm_consensus;

pub use registry::{
    canonical_id, dispatch, dispatch_all, strategy_ids, strategy_names, unknown_strategy_message,
    StrategyVisitor,
};
pub use strategy::{
    BarPassThrough, Decision, FeatureExtractor, HyperparameterSpace, InputKind, MarketEvent,
    MarketType, OptimizableStrategy, Strategy,
};
pub use swarm_consensus::{
    Agent, FeedVote, FeedWeights, FeedWeightsError, PolicyConfig, PriorParam, SwarmConfig,
    SwarmConsensusStrategy, SwarmEnsemble, SwarmPolicySpace, SwarmPriorsConfig,
};
