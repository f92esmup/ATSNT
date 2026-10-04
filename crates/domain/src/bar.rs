use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Aggregated bar formed when accumulated dollar volume reaches the specified threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DollarBar {
    /// Start timestamp of the bar (inclusive) in milliseconds.
    pub start_time: i64,
    /// End timestamp of the bar (inclusive) in milliseconds.
    pub end_time: i64,
    /// Opening price of the bar.
    pub open: Decimal,
    /// Highest price reached within the bar.
    pub high: Decimal,
    /// Lowest price reached within the bar.
    pub low: Decimal,
    /// Closing price of the bar.
    pub close: Decimal,
    /// Total volume traded in base asset (e.g. BTC).
    pub volume: Decimal,
    /// Total dollar value traded (accumulated notional value).
    pub dollar_volume: Decimal,
    /// Total number of individual trade executions within this bar.
    pub trade_count: u64,
}
