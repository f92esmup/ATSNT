//! What a strategy reacts to, what it decides, and the exits that protect the position its
//! entry opens.
//!
//! A strategy decides a triple-barrier entry ([`crate::OrderIntent`]) or the close of its open
//! position ([`Decision`]); the barriers travel with the position as [`ProtectiveExits`].

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::instrument::InstrumentId;
use crate::order::OrderIntent;
use crate::position::PositionSide;
use crate::trade::Trade;

/// One event a strategy reacts to.
///
/// Non-exhaustive: a strategy matches the trade and ignores whatever input is added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MarketEvent<'a> {
    /// A single market trade of one instrument.
    Trade {
        /// The instrument traded.
        instrument: &'a InstrumentId,
        /// The trade.
        trade: &'a Trade,
    },
}

impl<'a> MarketEvent<'a> {
    /// A trade of `instrument`.
    pub fn trade(instrument: &'a InstrumentId, trade: &'a Trade) -> Self {
        Self::Trade { instrument, trade }
    }

    /// When the event happened (Unix ms).
    pub fn timestamp(&self) -> i64 {
        match self {
            MarketEvent::Trade { trade, .. } => trade.timestamp,
        }
    }

    /// The instrument the event is about.
    pub fn instrument(&self) -> &'a InstrumentId {
        match self {
            MarketEvent::Trade { instrument, .. } => instrument,
        }
    }
}

/// What a strategy decides on one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Open a triple-barrier position. Ignored while a position is open or an entry is pending.
    Enter(OrderIntent),
    /// Close the open position at market, labelled [`SIGNAL_EXIT`]. Ignored while flat.
    Exit {
        /// The instrument whose position to close; `None` for the instrument of the event the
        /// strategy is deciding on.
        instrument: Option<InstrumentId>,
    },
}

impl Decision {
    /// Close the position of the instrument of the event being decided on.
    pub fn exit() -> Self {
        Self::Exit { instrument: None }
    }

    /// Close the position of `instrument`.
    pub fn exit_of(instrument: InstrumentId) -> Self {
        Self::Exit {
            instrument: Some(instrument),
        }
    }

    /// The instrument the decision names, if it names one.
    pub fn instrument(&self) -> Option<&InstrumentId> {
        match self {
            Self::Enter(intent) => intent.instrument.as_ref(),
            Self::Exit { instrument } => instrument.as_ref(),
        }
    }

    /// The instrument the decision is about: the one it names, else the event's.
    pub fn instrument_or<'a>(&'a self, event_instrument: &'a InstrumentId) -> &'a InstrumentId {
        self.instrument().unwrap_or(event_instrument)
    }
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

/// An open position of the account on one instrument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentPosition {
    /// The instrument.
    pub instrument: InstrumentId,
    /// The position.
    pub position: PositionView,
}

/// What a strategy may see of its own book when it decides on an event: its position on the
/// event's instrument, whether an entry is on its way there, and its positions on the other
/// instruments. It shows no balances; sizing and the account rules belong to the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StrategyView<'a> {
    /// The open position on the event's instrument, if any.
    pub position: Option<PositionView>,
    /// `true` while an entry order on the event's instrument is pending.
    pub entry_pending: bool,
    /// The open positions on the other instruments, in instrument order. Always empty on a
    /// single-instrument run.
    pub others: &'a [InstrumentPosition],
}

impl StrategyView<'static> {
    /// A flat book with nothing pending.
    pub const FLAT: Self = Self {
        position: None,
        entry_pending: false,
        others: &[],
    };
}

impl StrategyView<'_> {
    /// Whether nothing is open or pending on the event's instrument.
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
        let event = MarketEvent::trade(&crate::UNNAMED_INSTRUMENT, &trade);
        assert_eq!(event.timestamp(), 42);
        assert_eq!(event.instrument().as_str(), "UNNAMED");
    }
}
