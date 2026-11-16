//! The kind of market an account trades in.

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
