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
