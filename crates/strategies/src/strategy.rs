use domain::{DomainError, OrderIntent};

pub use domain::{MarketEvent, MarketType};

/// Universal contract for all quantitative trading strategies in ATSNT.
///
/// Ensures compliance with the Open-Closed Principle (SOLID): the backtester
/// and execution engines interact exclusively with this trait, allowing new
/// strategies to be added without modifying existing simulator code.
pub trait Strategy: Send {
    /// Human-readable identifier for the strategy.
    fn name(&self) -> &str;

    /// Markets this strategy is explicitly designed to trade.
    fn compatible_market_types(&self) -> &'static [MarketType];

    /// Whether this strategy may be selected for the requested market.
    fn supports_market_type(&self, market_type: MarketType) -> bool {
        self.compatible_market_types().contains(&market_type)
    }

    /// Reacts to one market event and optionally decides a triple-barrier entry.
    ///
    /// Every runner calls this method, once per market trade.
    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent>;

    /// Clears internal state buffers to allow clean replay or backtest isolation.
    fn reset(&mut self);

    /// Strategy-specific audit written into reports as `strategy_audit`; `None` by default.
    fn audit(&self) -> Option<serde_json::Value> {
        None
    }
}

/// The first, frozen stage of a strategy: turns market events into the features its decision
/// stage reads. An HPO runs it once and replays only the decision stage per candidate.
pub trait FeatureExtractor: Send {
    /// What the stage produces.
    type Features: Clone + Send + Sync + 'static;

    /// Consumes one market event; returns features when the event produced new ones.
    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<Self::Features>;

    /// Clears internal state.
    fn reset(&mut self);
}

/// Universal contract for hyperparameter search spaces and stability perturbations.
pub trait HyperparameterSpace: Send + Sync + Clone {
    /// Strategy configuration type parameterized by this space.
    type Config: Clone
        + Send
        + Sync
        + serde::Serialize
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + std::fmt::Display
        + 'static;

    /// Generates candidate parameter combinations within this space, bounded by `max_candidates`.
    fn generate_candidates(&self, max_candidates: usize) -> Vec<Self::Config>;

    /// Generates neighboring configurations around `base` for local parameter stability analysis (AFML Ch. 14).
    fn perturb_neighbors(&self, base: &Self::Config) -> Vec<Self::Config>;
}

/// Extension trait for quantitative trading strategies that support automated hyperparameter optimization.
///
/// Optimisation runs in two stages (strategy spec `docs/strategy/03-swarm-consensus.md` §3.4): a
/// frozen [`FeatureExtractor`] built from [`OptimizableStrategy::SharedConfig`] once per run, and
/// the strategy itself as the decision stage, built per candidate from
/// [`OptimizableStrategy::Config`] and fed through [`OptimizableStrategy::on_features`].
pub trait OptimizableStrategy: Strategy + Sync + Sized {
    /// Configuration shared by every candidate of one HPO run (frozen); `()` when none.
    type SharedConfig: Clone
        + Send
        + Sync
        + serde::Serialize
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + Default
        + 'static;
    /// Configuration type that parameterizes this strategy.
    type Config: Clone
        + Send
        + Sync
        + serde::Serialize
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + std::fmt::Display
        + 'static;
    /// Search space defining hyperparameter domains and stability perturbations.
    type Space: HyperparameterSpace<Config = Self::Config>;

    /// Canonical identifier for the strategy (e.g. "swarm_consensus").
    fn id() -> &'static str;

    /// The strategy's frozen first stage.
    type Extractor: FeatureExtractor;

    /// Constructs an active strategy instance from the shared and the searched configuration.
    fn from_config(shared: &Self::SharedConfig, config: Self::Config) -> Result<Self, DomainError>;

    /// Builds the frozen first stage of this strategy.
    fn extractor(shared: &Self::SharedConfig) -> Result<Self::Extractor, DomainError>;

    /// The decision stage: decides on features produced by [`OptimizableStrategy::extractor`].
    fn on_features(
        &mut self,
        features: &<Self::Extractor as FeatureExtractor>::Features,
    ) -> Option<OrderIntent>;

    /// Default search space defining hyperparameter candidate domains and ranges.
    fn default_space() -> Self::Space;

    /// The shared configuration this strategy runs with on `market`. A strategy that trades
    /// several markets narrows itself here to what the market allows; by default the
    /// configuration is used as given.
    fn shared_for_market(shared: Self::SharedConfig, _market: MarketType) -> Self::SharedConfig {
        shared
    }
}

impl<S: ?Sized + Strategy> Strategy for Box<S> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn compatible_market_types(&self) -> &'static [MarketType] {
        (**self).compatible_market_types()
    }

    fn supports_market_type(&self, market_type: MarketType) -> bool {
        (**self).supports_market_type(market_type)
    }

    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent> {
        (**self).on_event(event)
    }

    fn reset(&mut self) {
        (**self).reset()
    }

    fn audit(&self) -> Option<serde_json::Value> {
        (**self).audit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Side, Trade};
    use rust_decimal_macros::dec;

    /// A strategy that buys on every trade.
    struct EveryTrade;

    impl Strategy for EveryTrade {
        fn name(&self) -> &str {
            "every_trade"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<OrderIntent> {
            let MarketEvent::Trade(trade) = event else {
                return None;
            };
            OrderIntent::new(
                trade.timestamp,
                Side::Buy,
                trade.price,
                dec!(90),
                dec!(110),
                3,
            )
            .ok()
        }

        fn reset(&mut self) {}
    }

    fn trade() -> Trade {
        Trade::new(1, dec!(100), dec!(1), Side::Buy).unwrap()
    }

    #[test]
    fn a_strategy_decides_on_a_trade() {
        let trade = trade();
        let intent = EveryTrade.on_event(&MarketEvent::Trade(&trade)).unwrap();
        assert_eq!(intent.price, dec!(100));
    }

    #[test]
    fn a_boxed_strategy_forwards_every_method() {
        let mut boxed: Box<dyn Strategy> = Box::new(EveryTrade);
        let trade = trade();
        assert!(boxed.on_event(&MarketEvent::Trade(&trade)).is_some());
        assert_eq!(boxed.name(), "every_trade");
        assert!(boxed.supports_market_type(MarketType::UsdMPerpetual));
        assert_eq!(boxed.audit(), None);
        boxed.reset();
    }
}
