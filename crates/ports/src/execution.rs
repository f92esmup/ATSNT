use domain::{OrderIntent, Side};
use rust_decimal::Decimal;

use crate::exits::ExitGateway;

/// Acknowledgement that the venue accepted an entry order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryAck {
    /// Venue-assigned identifier of the order (client order id on Binance).
    pub order_id: String,
    /// Venue status at acknowledgement time (e.g. `NEW`, `FILLED`).
    pub status: String,
    /// Quantity the venue accepted after sizing and exchange rounding.
    pub quantity: Decimal,
    /// Reference price of the order: the signal price the entry was sized at.
    pub price: Decimal,
    /// Local wall-clock time, in Unix milliseconds, at which the order request left for the
    /// venue (0 when the venue cannot tell). Parity calibration needs it: stamping the
    /// acknowledgement's arrival instead would hide the round trip.
    pub sent_at_ms: i64,
}

/// One execution (partial or complete) reported by the venue for any order of the session.
///
/// Live builds it from the private user-data stream; a simulated venue emits the same shape,
/// so the session cannot tell where the fills come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    /// Execution time in Unix milliseconds.
    pub time_ms: i64,
    /// Direction of the order that executed.
    pub side: Side,
    /// Quantity executed by this report (zero for acknowledgements and cancellations).
    pub last_quantity: Decimal,
    /// Price of this execution.
    pub last_price: Decimal,
    /// `true` when this report completes its order.
    pub order_filled: bool,
    /// `true` when the order will report no further executions: it was filled, cancelled,
    /// expired or rejected.
    pub order_closed: bool,
    /// Commission charged for this execution (zero when none).
    pub commission: Decimal,
    /// Asset the commission was charged in.
    pub commission_asset: Option<String>,
    /// `true` when the exchange's liquidation engine produced this execution, not the session.
    pub liquidation: bool,
    /// Identifier of the trade behind this execution, when the venue numbers them. A venue
    /// that repeats a report (after a reconnection, say) repeats the id, which lets the
    /// session count the fill once.
    pub trade_id: Option<i64>,
}

/// Everything a trading session needs from an execution venue.
///
/// A venue accepts entries and manages exchange-held exits; it reports what happened through
/// [`ExecutionReport`]s that the driver feeds back to the session. Binance implements it for
/// live trading and the simulator implements it for backtests, so one session core runs on
/// both.
#[allow(async_fn_in_trait)]
pub trait ExecutionVenue: ExitGateway {
    /// Sizes, gates and dispatches a market entry for a strategy intent.
    ///
    /// `equity` is the wallet balance and `session_peak` the reference peak of the drawdown
    /// breaker (the current UTC day's marked-equity peak).
    async fn submit_entry(
        &self,
        symbol: &str,
        intent: &OrderIntent,
        equity: Decimal,
        session_peak: Decimal,
    ) -> Result<EntryAck, Self::Error>;
}
