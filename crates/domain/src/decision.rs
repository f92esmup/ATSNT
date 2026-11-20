//! What a strategy reacts to, what it decides, and the exits that protect the position its
//! entry opens.
//!
//! A strategy decides a triple-barrier entry ([`crate::OrderIntent`]) or the close of its open
//! position ([`Decision`]); the barriers travel with the position as [`ProtectiveExits`].

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::order::OrderIntent;
use crate::position::PositionSide;
use crate::trade::Trade;

/// One event a strategy reacts to.
///
/// Non-exhaustive: a strategy matches the trade and ignores whatever input is added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MarketEvent<'a> {
    /// A single market trade.
    Trade(&'a Trade),
}

impl MarketEvent<'_> {
    /// When the event happened (Unix ms).
    pub fn timestamp(&self) -> i64 {
        match self {
            MarketEvent::Trade(trade) => trade.timestamp,
        }
    }
}

/// What a strategy decides on one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Open a triple-barrier position. Ignored while a position is open or an entry is pending.
    Enter(OrderIntent),
    /// Close the open position at market, labelled [`SIGNAL_EXIT`]. Ignored while flat.
    Exit,
}

/// Exit reason of a position the strategy closed with [`Decision::Exit`].
pub const SIGNAL_EXIT: &str = "SignalExit";

/// The strategy's open position, as it may see it when it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionView {
    /// Direction of the position.
    pub side: PositionSide,
    /// Quantity held, in base units.
    pub quantity: Decimal,
    /// Average entry price.
    pub entry_price: Decimal,
}

/// What a strategy may see of its own book when it decides: its position and whether an entry is
/// on its way. It shows no balances; sizing and the account rules belong to the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StrategyView {
    /// The open position, if any.
    pub position: Option<PositionView>,
    /// `true` while an entry order is pending and the position not yet open.
    pub entry_pending: bool,
}

impl StrategyView {
    /// A flat book with nothing pending.
    pub const FLAT: Self = Self {
        position: None,
        entry_pending: false,
    };

    /// Whether nothing is open or pending.
    pub fn is_flat(&self) -> bool {
        self.position.is_none() && !self.entry_pending
    }
}

/// The exits that protect an open position: a stop always, and for triple-barrier entries a
/// take profit and a time limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectiveExits {
    /// Stop price on the losing side of the position.
    pub stop: Decimal,
    /// Take-profit price on the winning side; `None` when only the stop protects.
    pub take_profit: Option<Decimal>,
    /// Optional step-trailing ATR distance for adaptive breakeven and trailing stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_trailing_atr: Option<Decimal>,
    /// Optional breakeven trigger price distance from entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakeven_dist: Option<Decimal>,
    /// Time barrier in Unix ms: the position closes on the first event that ends at or after it.
    /// Every triple-barrier entry has one; `None` only for a stop-only protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<i64>,
}

impl ProtectiveExits {
    /// The barriers of a triple-barrier entry, copied with trailing and deadline configuration.
    pub fn from_intent(intent: &OrderIntent) -> Self {
        Self {
            stop: intent.stop_loss,
            take_profit: Some(intent.take_profit),
            step_trailing_atr: intent.trailing_atr,
            breakeven_dist: intent.breakeven_atr,
            deadline_ms: Some(intent.deadline_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OrderIntent, Side, Trade};
    use rust_decimal_macros::dec;

    #[test]
    fn exits_from_an_intent_copy_its_three_barriers() {
        let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(98), dec!(104), 7_000).unwrap();
        assert_eq!(
            ProtectiveExits::from_intent(&intent),
            ProtectiveExits {
                stop: dec!(98),
                take_profit: Some(dec!(104)),
                step_trailing_atr: None,
                breakeven_dist: None,
                deadline_ms: Some(7_001),
            }
        );
    }

    #[test]
    fn a_market_event_reports_when_it_happened() {
        let trade = Trade::new(42, dec!(100), dec!(1), Side::Buy).unwrap();
        assert_eq!(MarketEvent::Trade(&trade).timestamp(), 42);
    }
}
