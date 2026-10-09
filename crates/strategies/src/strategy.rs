use domain::{DollarBar, OrderIntent};

/// Trading venue model a strategy is designed to consume and emit orders for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    /// Ingests a completed DollarBar event and optionally emits an OrderIntent.
    fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent>;

    /// Clears internal state buffers to allow clean replay or backtest isolation.
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
pub trait OptimizableStrategy: Strategy + Sync + Sized {
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

    /// Constructs an active strategy instance from its configuration parameters.
    fn from_config(config: Self::Config) -> Result<Self, domain::DomainError>;

    /// Default search space defining hyperparameter candidate domains and ranges.
    fn default_space() -> Self::Space;
}
