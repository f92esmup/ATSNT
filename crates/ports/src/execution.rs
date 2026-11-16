use domain::Side;
use rust_decimal::Decimal;

use crate::exits::ExitGateway;

/// State of an order when the venue acknowledged it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStatus {
    /// Accepted and working; its executions follow as reports.
    New,
    /// Already executed in full when it was acknowledged.
    Filled,
}

/// Acknowledgement that the venue accepted an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryAck {
    /// Venue-assigned identifier of the order (client order id of the venue).
    pub order_id: String,
    /// State of the order at acknowledgement time.
    pub status: AckStatus,
    /// Quantity the venue accepted.
    pub quantity: Decimal,
    /// Reference price of the order: the venue's last trade when it was sent.
    pub price: Decimal,
    /// Local wall-clock time, in Unix milliseconds, at which the order request left for the
    /// venue (0 when the venue cannot tell). Parity calibration needs it: stamping the
    /// acknowledgement's arrival instead would hide the round trip.
    pub sent_at_ms: i64,
}

/// A market order of an explicit quantity, sized by the caller.
///
/// It carries no sizing inputs: the session sizes the entry with the shared domain gate and
/// sends the quantity. A `reduce_only` order may only
/// shrink the open position: the venue never lets it flip the position or close more than is
/// open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderRequest {
    /// Direction of the order.
    pub side: Side,
    /// Quantity to trade, in base units; must be positive.
    pub quantity: Decimal,
    /// `true` when the order may only reduce the open position.
    pub reduce_only: bool,
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
/// A venue executes orders and manages exchange-held exits; it reports what happened through
/// [`ExecutionReport`]s that the driver feeds back to the session. It decides nothing: sizing and
/// the pre-trade risk rules belong to the session, so they are the same on every venue. A live
/// exchange adapter implements it for live trading and the simulator implements it for
/// backtests, so one session core runs on both.
#[allow(async_fn_in_trait)]
pub trait ExecutionVenue: ExitGateway {
    /// Dispatches a market order of the request's exact quantity.
    ///
    /// The acknowledgement's `quantity` is the requested quantity and its `price` the venue's
    /// reference price at send time (the last trade, or 0 when the venue cannot tell).
    async fn submit_order(
        &self,
        symbol: &str,
        request: &OrderRequest,
    ) -> Result<EntryAck, Self::Error>;
}
