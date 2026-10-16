use domain::Side;
use rust_decimal::Decimal;

/// Exchange operations the exit lifecycle needs.
#[allow(async_fn_in_trait)]
pub trait ExitGateway {
    /// Error reported by the exchange layer.
    type Error: std::fmt::Display;

    /// Places the exchange-held SL and TP that close the whole position.
    async fn place_exits(
        &self,
        symbol: &str,
        entry_side: Side,
        stop_loss: Decimal,
        take_profit: Decimal,
    ) -> Result<(), Self::Error>;

    /// Cancels every pending conditional and regular order of the symbol.
    async fn cancel_orders(&self, symbol: &str) -> Result<(), Self::Error>;

    /// Closes the open position at market; `true` when something was closed, `false` if flat.
    async fn close_position(&self, symbol: &str) -> Result<bool, Self::Error>;
}
