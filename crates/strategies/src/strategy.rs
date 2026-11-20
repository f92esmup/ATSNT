use domain::DomainError;

pub use domain::{Decision, MarketEvent, MarketType, PositionView, StrategyView};

/// How a strategy is run on an account that trades several instruments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrategyScope {
    /// One instance per instrument, each seeing only its instrument's trades and position; the
    /// instances share the account and its gate. A single-instrument strategy is this.
    #[default]
    PerInstrument,
    /// One instance sees every instrument's trades and decides on any of them, naming the
    /// instrument in its decisions.
    CrossSectional,
}

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

    /// Reacts to one market event and pushes its decisions, if any, to `out`.
    ///
    /// Every runner calls this method once per market trade, with `view` showing the strategy's
    /// open position. `out` is empty on entry and reused across calls, so deciding allocates
    /// nothing. Runners apply the decisions in order: an [`Decision::Enter`] while a position is
    /// open or an entry pending is ignored, as is a [`Decision::Exit`] while flat.
    fn on_event(&mut self, event: &MarketEvent<'_>, view: &StrategyView, out: &mut Vec<Decision>);

    /// How the strategy runs on several instruments; [`StrategyScope::PerInstrument`] by default.
    fn scope(&self) -> StrategyScope {
        StrategyScope::PerInstrument
    }

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

    /// The decision stage: decides on features produced by [`OptimizableStrategy::extractor`],
    /// as [`Strategy::on_event`] does on events.
    fn on_features(
        &mut self,
        features: &<Self::Extractor as FeatureExtractor>::Features,
        view: &StrategyView,
        out: &mut Vec<Decision>,
    );

    /// Default search space defining hyperparameter candidate domains and ranges.
    fn default_space() -> Self::Space;

    /// Data, in milliseconds, a run must give the strategy before its first evaluated decision:
    /// the time its slowest input needs to be warm. The optimizer evaluates nothing earlier.
    /// Every strategy declares it; `0` means it decides from its first trade.
    fn warm_up_ms(shared: &Self::SharedConfig) -> i64;

    /// Last timestamp (Unix ms) of the data `shared` was calibrated on, for a strategy whose
    /// shared configuration is fitted to data. Evaluated data must start after it, or the
    /// calibration leaks into the result. `None` (the default) for a strategy that calibrates
    /// nothing.
    fn calibration_end_ms(_shared: &Self::SharedConfig) -> Option<i64> {
        None
    }

    /// Whether a run must be given the shared configuration explicitly because its default
    /// would mislead (for example, uncalibrated thresholds). `false` by default.
    fn requires_shared_config() -> bool {
        false
    }

    /// The shared and searched configuration the contract suite runs the strategy with, when its
    /// defaults would never decide on the suite's synthetic data (for example, thresholds sized
    /// for a real market). `None` (the default) runs it with its defaults. Used by tests only.
    fn contract_fixture() -> Option<(Self::SharedConfig, Self::Config)> {
        None
    }

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

    fn on_event(&mut self, event: &MarketEvent<'_>, view: &StrategyView, out: &mut Vec<Decision>) {
        (**self).on_event(event, view, out)
    }

    fn scope(&self) -> StrategyScope {
        (**self).scope()
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
    use domain::{OrderIntent, Side, Trade};
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

        fn on_event(
            &mut self,
            event: &MarketEvent<'_>,
            _view: &StrategyView,
            out: &mut Vec<Decision>,
        ) {
            let MarketEvent::Trade { trade, .. } = event else {
                return;
            };
            out.extend(
                OrderIntent::new(
                    trade.timestamp,
                    Side::Buy,
                    trade.price,
                    dec!(90),
                    dec!(110),
                    180_000,
                )
                .ok()
                .map(Decision::Enter),
            );
        }

        fn reset(&mut self) {}
    }

    fn trade() -> Trade {
        Trade::new(1, dec!(100), dec!(1), Side::Buy).unwrap()
    }

    fn decide<S: Strategy>(strategy: &mut S, trade: &Trade) -> Vec<Decision> {
        let mut out = Vec::new();
        strategy.on_event(
            &MarketEvent::trade(&domain::UNNAMED_INSTRUMENT, trade),
            &StrategyView::FLAT,
            &mut out,
        );
        out
    }

    #[test]
    fn a_strategy_decides_on_a_trade() {
        let trade = trade();
        let [Decision::Enter(intent)] = &decide(&mut EveryTrade, &trade)[..] else {
            panic!("one entry expected");
        };
        assert_eq!(intent.price, dec!(100));
    }

    #[test]
    fn a_boxed_strategy_forwards_every_method() {
        let mut boxed: Box<dyn Strategy> = Box::new(EveryTrade);
        let trade = trade();
        assert_eq!(decide(&mut boxed, &trade).len(), 1);
        assert_eq!(boxed.name(), "every_trade");
        assert!(boxed.supports_market_type(MarketType::UsdMPerpetual));
        assert_eq!(boxed.scope(), StrategyScope::PerInstrument);
        assert_eq!(boxed.audit(), None);
        boxed.reset();
    }
}
