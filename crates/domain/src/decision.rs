//! What a strategy reacts to and the exits that protect the position its entry opens.
//!
//! A strategy decides a triple-barrier entry ([`crate::OrderIntent`]); the barriers travel with
//! the position as [`ProtectiveExits`].

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::bar::DollarBar;
use crate::order::OrderIntent;
use crate::trade::Trade;

/// The market input a strategy declares it consumes; runners prepare exactly that stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// Finished dollar bars from the shared `DollarBarAggregator`.
    DollarBars,
    /// Raw aggregated trades; the strategy samples its own bars.
    Trades,
}

/// One event a strategy reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketEvent<'a> {
    /// A single market trade.
    Trade(&'a Trade),
    /// A finished dollar bar.
    DollarBar(&'a DollarBar),
}

impl MarketEvent<'_> {
    /// When the event happened (Unix ms): the trade time, or the bar's last trade time.
    pub fn timestamp(&self) -> i64 {
        match self {
            MarketEvent::Trade(trade) => trade.timestamp,
            MarketEvent::DollarBar(bar) => bar.end_time,
        }
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
    /// Bars to hold before a forced exit; `None` when a deadline is the time barrier.
    pub max_bars: Option<usize>,
    /// Optional step-trailing ATR distance for adaptive breakeven and trailing stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_trailing_atr: Option<Decimal>,
    /// Optional breakeven trigger price distance from entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakeven_dist: Option<Decimal>,
    /// Optional timestamp deadline in Unix ms for real-time / intraday time barriers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<i64>,
}

impl ProtectiveExits {
    /// The barriers of a triple-barrier entry, copied with trailing and deadline configuration.
    pub fn from_intent(intent: &OrderIntent) -> Self {
        Self {
            stop: intent.stop_loss,
            take_profit: Some(intent.take_profit),
            max_bars: if intent.deadline_ms.is_some() {
                None
            } else {
                Some(intent.max_bars_hold)
            },
            step_trailing_atr: intent.trailing_atr,
            breakeven_dist: intent.breakeven_atr,
            deadline_ms: intent.deadline_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DollarBar, OrderIntent, Side, Trade};
    use rust_decimal_macros::dec;

    #[test]
    fn exits_from_an_intent_copy_its_three_barriers() {
        let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(98), dec!(104), 7).unwrap();
        assert_eq!(
            ProtectiveExits::from_intent(&intent),
            ProtectiveExits {
                stop: dec!(98),
                take_profit: Some(dec!(104)),
                max_bars: Some(7),
                step_trailing_atr: None,
                breakeven_dist: None,
                deadline_ms: None,
            }
        );
    }

    #[test]
    fn a_market_event_reports_when_it_happened() {
        let trade = Trade::new(42, dec!(100), dec!(1), Side::Buy).unwrap();
        assert_eq!(MarketEvent::Trade(&trade).timestamp(), 42);
        let bar = DollarBar {
            start_time: 10,
            end_time: 99,
            open: dec!(1),
            high: dec!(1),
            low: dec!(1),
            close: dec!(1),
            volume: dec!(1),
            dollar_volume: dec!(1),
            trade_count: 1,
            buy_volume: dec!(1),
            sell_volume: dec!(0),
            buy_dollar_volume: dec!(1),
            sell_dollar_volume: dec!(0),
        };
        assert_eq!(MarketEvent::DollarBar(&bar).timestamp(), 99);
    }

    #[test]
    fn input_kind_round_trips_through_json() {
        for kind in [InputKind::DollarBars, InputKind::Trades] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(serde_json::from_str::<InputKind>(&json).unwrap(), kind);
        }
        assert_eq!(
            serde_json::to_string(&InputKind::Trades).unwrap(),
            "\"trades\""
        );
    }
}
