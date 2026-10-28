//! The one place strategies are registered.
//!
//! Adding a strategy to ATSNT means implementing [`OptimizableStrategy`] and adding **one line**
//! to the `register_strategies!` call below. That line makes it selectable by name (and alias)
//! in the backtest CLI and the HPO runner, loadable from deployment artifacts, available to
//! paper and live trading, and puts it under the generic contract test suite
//! (`crates/strategies/src/contract.rs`, compiled only in tests).
//!
//! Callers stay generic over the strategy through a [`StrategyVisitor`]: the registry calls
//! `visit::<S>()` with the concrete strategy type, so nothing downstream matches on strategy
//! names.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::dollar_bars_cusum::DollarBarsCusumStrategy;
use crate::langevin_potential::LangevinPotentialStrategy;
use crate::order_flow_imbalance::OrderFlowImbalanceStrategy;
use crate::quantum_well_swing::QuantumWellSwingStrategy;
use crate::strategy::OptimizableStrategy;
use crate::swarm_consensus::SwarmConsensusStrategy;

/// Code that runs against one registered strategy type chosen at run time.
pub trait StrategyVisitor {
    /// What the visit produces.
    type Output;

    /// Called with the strategy the registry resolved.
    fn visit<S>(self) -> Self::Output
    where
        S: OptimizableStrategy + 'static,
        S::Config: Default,
        S::Space: Serialize + DeserializeOwned;
}

macro_rules! register_strategies {
    ($($strategy:ty => [$($alias:literal),* $(,)?]),+ $(,)?) => {
        /// Canonical ids of every registered strategy, in registration order.
        pub fn strategy_ids() -> Vec<&'static str> {
            vec![$(<$strategy as OptimizableStrategy>::id()),+]
        }

        /// Canonical id for a registered id or alias, `None` when nothing matches.
        pub fn canonical_id(name: &str) -> Option<&'static str> {
            $(
                if name == <$strategy as OptimizableStrategy>::id() $(|| name == $alias)* {
                    return Some(<$strategy as OptimizableStrategy>::id());
                }
            )+
            None
        }

        /// Every accepted spelling (canonical ids and aliases) of every strategy.
        pub fn strategy_names() -> Vec<&'static str> {
            vec![$(<$strategy as OptimizableStrategy>::id() $(, $alias)*),+]
        }

        /// Runs `visitor` against the strategy registered under `name` (id or alias).
        pub fn dispatch<V: StrategyVisitor>(name: &str, visitor: V) -> Option<V::Output> {
            $(
                if name == <$strategy as OptimizableStrategy>::id() $(|| name == $alias)* {
                    return Some(visitor.visit::<$strategy>());
                }
            )+
            None
        }

        /// Runs a clone of `visitor` against every registered strategy.
        pub fn dispatch_all<V: StrategyVisitor + Clone>(visitor: V) -> Vec<(&'static str, V::Output)> {
            vec![$((<$strategy as OptimizableStrategy>::id(), visitor.clone().visit::<$strategy>())),+]
        }
    };
}

register_strategies! {
    DollarBarsCusumStrategy => ["cusum"],
    OrderFlowImbalanceStrategy => ["ofi"],
    SwarmConsensusStrategy => ["swarm"],
    LangevinPotentialStrategy => ["langevin"],
    QuantumWellSwingStrategy => ["quantum", "quantum_well"],
}

/// Error text for an unknown strategy name, listing what is registered.
pub fn unknown_strategy_message(name: &str) -> String {
    format!(
        "Unknown strategy '{name}'. Available strategies: {}",
        strategy_ids()
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::assert_strategy_contract;
    use std::collections::HashSet;

    #[derive(Clone)]
    struct ContractCheck;

    impl StrategyVisitor for ContractCheck {
        type Output = ();

        fn visit<S>(self)
        where
            S: OptimizableStrategy + 'static,
            S::Config: Default,
            S::Space: Serialize + DeserializeOwned,
        {
            assert_strategy_contract::<S>();
        }
    }

    #[test]
    fn every_registered_strategy_honours_the_contract() {
        let checked = dispatch_all(ContractCheck);

        assert_eq!(checked.len(), strategy_ids().len());
        assert!(!checked.is_empty());
    }

    #[test]
    fn ids_and_aliases_never_collide() {
        let names = strategy_names();
        let unique: HashSet<_> = names.iter().collect();

        assert_eq!(
            unique.len(),
            names.len(),
            "a name resolves to two strategies: {names:?}"
        );
    }

    #[test]
    fn aliases_resolve_to_their_canonical_id() {
        assert_eq!(canonical_id("cusum"), Some("dollar_bars_cusum"));
        assert_eq!(canonical_id("dollar_bars_cusum"), Some("dollar_bars_cusum"));
        assert_eq!(canonical_id("ofi"), Some("order_flow_imbalance"));
        assert_eq!(canonical_id("nope"), None);
    }

    struct IdOf;

    impl StrategyVisitor for IdOf {
        type Output = &'static str;

        fn visit<S>(self) -> &'static str
        where
            S: OptimizableStrategy + 'static,
            S::Config: Default,
            S::Space: Serialize + DeserializeOwned,
        {
            S::id()
        }
    }

    #[test]
    fn dispatch_resolves_aliases_and_rejects_unknown_names() {
        assert_eq!(dispatch("ofi", IdOf), Some("order_flow_imbalance"));
        assert_eq!(dispatch("cusum", IdOf), Some("dollar_bars_cusum"));
        assert_eq!(dispatch("nope", IdOf), None);
    }

    #[test]
    fn the_unknown_strategy_message_lists_the_registered_ones() {
        let message = unknown_strategy_message("nope");

        assert!(message.contains("'nope'"));
        for id in strategy_ids() {
            assert!(message.contains(id));
        }
    }
}
