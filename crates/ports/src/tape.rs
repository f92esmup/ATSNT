//! Venue driven by a market tape: the port the tick-level simulation runs a strategy on.
//!
//! A tape venue receives every market print, executes the orders it holds against the prints,
//! and reports what each print did to the account. The simulated exchange implements it. A live
//! exchange does not: its fills arrive on its own streams, not from a replayed tape.

use domain::{Side, Trade};
use rust_decimal::Decimal;

use crate::execution::{ExecutionReport, ExecutionVenue};

/// A funding payment settled by a venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundingPayment {
    /// Settlement instant in Unix milliseconds.
    pub time_ms: i64,
    /// Amount paid by the account; negative when it received funding.
    pub amount: Decimal,
}

/// A venue replayed trade by trade.
pub trait TapeVenue: ExecutionVenue {
    /// Queues a raw market order sent at `at_ms`, with no sizing or risk gate, and returns its
    /// id. It reaches the market after the venue's latency. Used to replay orders a live run sent.
    fn submit_market_order_at(
        &self,
        side: Side,
        quantity: Decimal,
        at_ms: i64,
    ) -> Result<String, Self::Error>;

    /// Advances the market by one print and returns the executions it caused, oldest first.
    fn on_trade(&self, trade: &Trade) -> Vec<ExecutionReport>;

    /// Funding payments settled since the last call, oldest first.
    fn take_funding(&self) -> Vec<FundingPayment>;

    /// Wallet balance of the account.
    fn wallet_balance(&self) -> Decimal;

    /// Side, quantity and average entry price of the open position, if any.
    fn position(&self) -> Option<(Side, Decimal, Decimal)>;
}

/// A tape venue that keeps one position per symbol on one wallet balance, for simulations of an
/// account that trades several instruments. Its [`ExecutionVenue`] and
/// [`crate::ExitGateway`] methods act on the symbol they are given.
pub trait MultiTapeVenue: TapeVenue {
    /// Advances `symbol`'s market by one print and returns the executions it caused there.
    fn on_trade_of(&self, symbol: &str, trade: &Trade) -> Vec<ExecutionReport>;

    /// Funding payments settled on `symbol` since the last call, oldest first.
    fn take_funding_of(&self, symbol: &str) -> Vec<FundingPayment>;

    /// Side, quantity and average entry price of the position on `symbol`, if any.
    fn position_of(&self, symbol: &str) -> Option<(Side, Decimal, Decimal)>;

    /// Margin posted for every open position.
    fn margin_in_use(&self) -> Decimal;
}
