//! What a strategy decides and what it reacts to, independent of how it is executed.
//!
//! Every strategy returns a [`Decision`]. A triple-barrier [`Decision::Entry`] and a signed
//! exposure [`Decision::Target`] are two forms of one execution model: an entry is a target of
//! one level whose [`ProtectiveExits`] carry a take profit and a time limit (strategy spec
//! `docs/strategy/03-swarm-consensus.md` §3.2).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::bar::DollarBar;
use crate::error::DomainError;
use crate::order::OrderIntent;
use crate::trade::{Side, Trade};

/// Highest absolute exposure level a [`TargetPosition`] may ask for.
pub const MAX_TARGET_LEVEL: i8 = 3;

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

/// A signed exposure level requested by a strategy, with the risk it may take.
///
/// Positive levels are long, negative short, 0 flat. The engine sizes the position as
/// `equity * risk_fraction / stop_distance` and holds it until another target replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetPosition {
    timestamp: i64,
    level: i8,
    risk_fraction: Decimal,
    stop_distance: Decimal,
    take_profit_distance: Option<Decimal>,
}

impl TargetPosition {
    /// Builds a validated target with no take-profit barrier.
    ///
    /// The level lies in `-3..=3`; `risk_fraction` lies in `[0, 1]` and is 0 exactly when the
    /// level is 0; an open level needs a positive `stop_distance` (price units).
    pub fn new(
        timestamp: i64,
        level: i8,
        risk_fraction: Decimal,
        stop_distance: Decimal,
    ) -> Result<Self, DomainError> {
        Self::with_exits(timestamp, level, risk_fraction, stop_distance, None)
    }

    /// Builds a validated target with an optional take-profit distance.
    ///
    /// If `take_profit_distance` is specified, it must be positive and the level must not be 0.
    pub fn with_exits(
        timestamp: i64,
        level: i8,
        risk_fraction: Decimal,
        stop_distance: Decimal,
        take_profit_distance: Option<Decimal>,
    ) -> Result<Self, DomainError> {
        if !(-MAX_TARGET_LEVEL..=MAX_TARGET_LEVEL).contains(&level) {
            return Err(DomainError::InvalidTarget(format!(
                "level {level} outside -{MAX_TARGET_LEVEL}..={MAX_TARGET_LEVEL}"
            )));
        }
        if risk_fraction < Decimal::ZERO || risk_fraction > Decimal::ONE {
            return Err(DomainError::InvalidTarget(format!(
                "risk fraction {risk_fraction} outside [0, 1]"
            )));
        }
        if (level == 0) != risk_fraction.is_zero() {
            return Err(DomainError::InvalidTarget(format!(
                "risk fraction {risk_fraction} must be 0 exactly when the level ({level}) is 0"
            )));
        }
        if level != 0 && stop_distance <= Decimal::ZERO {
            return Err(DomainError::InvalidTarget(format!(
                "stop distance {stop_distance} must be positive for level {level}"
            )));
        }
        if level == 0 && take_profit_distance.is_some() {
            return Err(DomainError::InvalidTarget(
                "take profit distance cannot be set for a flat (level 0) target".to_string(),
            ));
        }
        if let Some(tp) = take_profit_distance {
            if tp <= Decimal::ZERO {
                return Err(DomainError::InvalidTarget(format!(
                    "take profit distance {tp} must be positive for level {level}"
                )));
            }
        }
        Ok(Self {
            timestamp,
            level,
            risk_fraction,
            stop_distance,
            take_profit_distance,
        })
    }

    /// When the strategy decided (Unix ms).
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    /// Signed exposure level.
    pub fn level(&self) -> i8 {
        self.level
    }

    /// Share of equity the position may lose at its stop.
    pub fn risk_fraction(&self) -> Decimal {
        self.risk_fraction
    }

    /// Distance from the fill price to the protective stop, in price units.
    pub fn stop_distance(&self) -> Decimal {
        self.stop_distance
    }

    /// Distance from the fill price to the protective take-profit barrier, in price units.
    pub fn take_profit_distance(&self) -> Option<Decimal> {
        self.take_profit_distance
    }

    /// Side of the requested exposure; `None` for flat.
    pub fn side(&self) -> Option<Side> {
        match self.level {
            l if l > 0 => Some(Side::Buy),
            l if l < 0 => Some(Side::Sell),
            _ => None,
        }
    }
}

/// The exits that protect an open position: a stop always, and for triple-barrier entries a
/// take profit and a time limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectiveExits {
    /// Stop price on the losing side of the position.
    pub stop: Decimal,
    /// Take-profit price on the winning side; `None` for targets without a TP.
    pub take_profit: Option<Decimal>,
    /// Bars to hold before a forced exit; `None` for targets.
    pub max_bars: Option<usize>,
    /// Optional step-trailing ATR distance for adaptive breakeven and trailing stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_trailing_atr: Option<Decimal>,
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
            deadline_ms: intent.deadline_ms,
        }
    }

    /// A stop `stop_distance` away from `fill_price` on the losing side of `side`, and an
    /// optional take profit `take_profit_distance` away on the winning side.
    pub fn for_target(
        side: Side,
        fill_price: Decimal,
        stop_distance: Decimal,
        take_profit_distance: Option<Decimal>,
    ) -> Result<Self, DomainError> {
        if stop_distance <= Decimal::ZERO {
            return Err(DomainError::InvalidTarget(format!(
                "stop distance {stop_distance} must be positive"
            )));
        }
        let stop = match side {
            Side::Buy => fill_price - stop_distance,
            Side::Sell => fill_price + stop_distance,
        };
        if stop <= Decimal::ZERO {
            return Err(DomainError::InvalidTarget(format!(
                "stop {stop} must be a positive price"
            )));
        }
        let take_profit = match take_profit_distance {
            Some(tp_dist) => {
                if tp_dist <= Decimal::ZERO {
                    return Err(DomainError::InvalidTarget(format!(
                        "take profit distance {tp_dist} must be positive"
                    )));
                }
                let tp = match side {
                    Side::Buy => fill_price + tp_dist,
                    Side::Sell => fill_price - tp_dist,
                };
                if tp <= Decimal::ZERO {
                    return Err(DomainError::InvalidTarget(format!(
                        "take profit {tp} must be a positive price"
                    )));
                }
                Some(tp)
            }
            None => None,
        };
        Ok(Self {
            stop,
            take_profit,
            max_bars: None,
            step_trailing_atr: None,
            deadline_ms: None,
        })
    }
}

/// What a strategy decides on an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Triple-barrier entry; accepted only when flat with nothing pending.
    Entry(OrderIntent),
    /// Signed exposure level; accepted at any time and replaces a pending target.
    Target(TargetPosition),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DollarBar, OrderIntent, Side, Trade};
    use rust_decimal_macros::dec;

    fn target(
        level: i8,
        risk: rust_decimal::Decimal,
        stop: rust_decimal::Decimal,
    ) -> Result<TargetPosition, DomainError> {
        TargetPosition::new(1_000, level, risk, stop)
    }

    #[test]
    fn a_target_keeps_its_fields_and_side() {
        let long = target(2, dec!(0.015), dec!(450)).unwrap();
        assert_eq!(long.timestamp(), 1_000);
        assert_eq!(long.level(), 2);
        assert_eq!(long.risk_fraction(), dec!(0.015));
        assert_eq!(long.stop_distance(), dec!(450));
        assert_eq!(long.side(), Some(Side::Buy));
        assert_eq!(
            target(-3, dec!(0.02), dec!(1)).unwrap().side(),
            Some(Side::Sell)
        );
        assert_eq!(target(0, dec!(0), dec!(0)).unwrap().side(), None);
    }

    #[test]
    fn a_target_level_must_lie_between_minus_three_and_three() {
        assert!(target(3, dec!(0.01), dec!(1)).is_ok());
        assert!(target(-3, dec!(0.01), dec!(1)).is_ok());
        assert!(matches!(
            target(4, dec!(0.01), dec!(1)),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(matches!(
            target(-4, dec!(0.01), dec!(1)),
            Err(DomainError::InvalidTarget(_))
        ));
    }

    #[test]
    fn risk_is_zero_exactly_when_the_level_is_zero() {
        assert!(matches!(
            target(0, dec!(0.01), dec!(1)),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(matches!(
            target(1, dec!(0), dec!(1)),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(target(0, dec!(0), dec!(5)).is_ok());
    }

    #[test]
    fn risk_must_lie_between_zero_and_one() {
        assert!(target(1, dec!(1), dec!(1)).is_ok());
        assert!(matches!(
            target(1, dec!(1.0001), dec!(1)),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(matches!(
            target(1, dec!(-0.01), dec!(1)),
            Err(DomainError::InvalidTarget(_))
        ));
    }

    #[test]
    fn an_open_target_needs_a_positive_stop_distance() {
        assert!(matches!(
            target(1, dec!(0.01), dec!(0)),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(matches!(
            target(-1, dec!(0.01), dec!(-5)),
            Err(DomainError::InvalidTarget(_))
        ));
    }

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
                deadline_ms: None,
            }
        );
    }

    #[test]
    fn exits_for_a_target_hold_a_stop_and_optional_take_profit() {
        assert_eq!(
            ProtectiveExits::for_target(Side::Buy, dec!(40000), dec!(1200), None).unwrap(),
            ProtectiveExits {
                stop: dec!(38800),
                take_profit: None,
                max_bars: None,
                step_trailing_atr: None,
                deadline_ms: None,
            }
        );
        assert_eq!(
            ProtectiveExits::for_target(Side::Sell, dec!(40000), dec!(1200), None)
                .unwrap()
                .stop,
            dec!(41200)
        );
        let long_with_tp =
            ProtectiveExits::for_target(Side::Buy, dec!(40000), dec!(1200), Some(dec!(2400)))
                .unwrap();
        assert_eq!(long_with_tp.stop, dec!(38800));
        assert_eq!(long_with_tp.take_profit, Some(dec!(42400)));

        let short_with_tp =
            ProtectiveExits::for_target(Side::Sell, dec!(40000), dec!(1200), Some(dec!(2400)))
                .unwrap();
        assert_eq!(short_with_tp.stop, dec!(41200));
        assert_eq!(short_with_tp.take_profit, Some(dec!(37600)));
    }

    #[test]
    fn exits_for_a_target_reject_invalid_parameters() {
        assert!(matches!(
            ProtectiveExits::for_target(Side::Buy, dec!(100), dec!(100), None),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(matches!(
            ProtectiveExits::for_target(Side::Buy, dec!(100), dec!(0), None),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(matches!(
            ProtectiveExits::for_target(Side::Buy, dec!(100), dec!(10), Some(dec!(0))),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(matches!(
            ProtectiveExits::for_target(Side::Sell, dec!(100), dec!(10), Some(dec!(100))),
            Err(DomainError::InvalidTarget(_))
        ));
    }

    #[test]
    fn target_with_exits_validates_take_profit() {
        let ok = TargetPosition::with_exits(1, 2, dec!(0.02), dec!(500), Some(dec!(1000))).unwrap();
        assert_eq!(ok.take_profit_distance(), Some(dec!(1000)));

        assert!(matches!(
            TargetPosition::with_exits(1, 0, Decimal::ZERO, Decimal::ZERO, Some(dec!(1000))),
            Err(DomainError::InvalidTarget(_))
        ));
        assert!(matches!(
            TargetPosition::with_exits(1, 1, dec!(0.01), dec!(500), Some(Decimal::ZERO)),
            Err(DomainError::InvalidTarget(_))
        ));
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
