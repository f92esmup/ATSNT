use domain::{DollarBar, DomainError, OrderIntent};

pub use domain::{Decision, InputKind, MarketEvent};
use serde::{Deserialize, Serialize};

/// Trading venue model a strategy is designed to consume and emit orders for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketType {
    /// Cash-only exchange trading with distinct base and quote balances.
    Spot,
    /// USD-M perpetual futures trading.
    UsdMPerpetual,
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

    /// The market input this strategy consumes; runners prepare exactly that stream.
    ///
    /// Defaults to finished dollar bars, the input of every triple-barrier strategy.
    fn input_kind(&self) -> InputKind {
        InputKind::DollarBars
    }

    /// Ingests a completed DollarBar and optionally emits an OrderIntent.
    ///
    /// The convenience form of [`Strategy::on_event`] for dollar-bar strategies; strategies
    /// that override `on_event` leave it at its default, which decides nothing.
    fn on_bar(&mut self, _bar: &DollarBar) -> Option<OrderIntent> {
        None
    }

    /// Reacts to one market event and optionally decides.
    ///
    /// Every runner calls this method. By default a dollar bar goes to [`Strategy::on_bar`]
    /// and its intent becomes a [`Decision::Entry`]; raw trades are ignored.
    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<Decision> {
        match event {
            MarketEvent::DollarBar(bar) => self.on_bar(bar).map(Decision::Entry),
            MarketEvent::Trade(_) => None,
        }
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

/// Extractor of dollar-bar strategies: the feature is the bar itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BarPassThrough;

impl FeatureExtractor for BarPassThrough {
    type Features = DollarBar;

    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<DollarBar> {
        match event {
            MarketEvent::DollarBar(bar) => Some((*bar).clone()),
            MarketEvent::Trade(_) => None,
        }
    }

    fn reset(&mut self) {}
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

    /// Canonical identifier for the strategy (e.g. "dollar_bars_cusum").
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
    ) -> Option<Decision>;

    /// Default search space defining hyperparameter candidate domains and ranges.
    fn default_space() -> Self::Space;
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

    fn input_kind(&self) -> InputKind {
        (**self).input_kind()
    }

    fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
        (**self).on_bar(bar)
    }

    fn on_event(&mut self, event: &MarketEvent<'_>) -> Option<Decision> {
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

    fn bar(close: rust_decimal::Decimal) -> DollarBar {
        DollarBar {
            start_time: 0,
            end_time: 60_000,
            open: close,
            high: close,
            low: close,
            close,
            volume: dec!(1),
            dollar_volume: close,
            trade_count: 1,
            buy_volume: dec!(1),
            sell_volume: dec!(0),
            buy_dollar_volume: close,
            sell_dollar_volume: dec!(0),
        }
    }

    /// A bar strategy written against the original contract: it only implements `on_bar`.
    struct BarOnly;

    impl Strategy for BarOnly {
        fn name(&self) -> &str {
            "bar_only"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
            OrderIntent::new(bar.end_time, Side::Buy, bar.close, dec!(90), dec!(110), 3).ok()
        }

        fn reset(&mut self) {}
    }

    #[test]
    fn a_bar_strategy_consumes_dollar_bars_and_decides_entries() {
        let mut strategy = BarOnly;
        assert_eq!(strategy.input_kind(), InputKind::DollarBars);
        let b = bar(dec!(100));
        assert_eq!(
            strategy.on_event(&MarketEvent::DollarBar(&b)),
            strategy.on_bar(&b).map(Decision::Entry)
        );
        assert!(strategy.on_event(&MarketEvent::DollarBar(&b)).is_some());
    }

    #[test]
    fn a_bar_strategy_ignores_raw_trades() {
        let trade = Trade::new(1, dec!(100), dec!(1), Side::Buy).unwrap();
        assert_eq!(BarOnly.on_event(&MarketEvent::Trade(&trade)), None);
    }

    #[test]
    fn a_boxed_strategy_forwards_every_method() {
        let mut boxed: Box<dyn Strategy> = Box::new(BarOnly);
        let b = bar(dec!(100));
        assert_eq!(boxed.input_kind(), InputKind::DollarBars);
        assert!(matches!(
            boxed.on_event(&MarketEvent::DollarBar(&b)),
            Some(Decision::Entry(_))
        ));
        assert!(boxed.on_bar(&b).is_some());
        assert_eq!(boxed.name(), "bar_only");
    }

    #[test]
    fn bar_pass_through_extracts_the_bar_and_nothing_from_trades() {
        let mut extractor = BarPassThrough;
        let b = bar(dec!(100));
        assert_eq!(
            extractor.on_event(&MarketEvent::DollarBar(&b)),
            Some(b.clone())
        );
        let trade = Trade::new(1, dec!(100), dec!(1), Side::Buy).unwrap();
        assert_eq!(extractor.on_event(&MarketEvent::Trade(&trade)), None);
    }
}
