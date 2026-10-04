use domain::{DollarBar, OrderIntent};

/// Universal contract for all quantitative trading strategies in ATSNT.
///
/// Ensures compliance with the Open-Closed Principle (SOLID): the backtester
/// and execution engines interact exclusively with this trait, allowing new
/// strategies to be added without modifying existing simulator code.
pub trait Strategy: Send {
    /// Human-readable identifier for the strategy.
    fn name(&self) -> &str;

    /// Ingests a completed DollarBar event and optionally emits an OrderIntent.
    fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent>;

    /// Clears internal state buffers to allow clean replay or backtest isolation.
    fn reset(&mut self);
}
