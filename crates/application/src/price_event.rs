//! Price events: what the execution core trades against (strategy spec
//! `docs/strategy/03-swarm-consensus.md` §3.5 and §11.1).
//!
//! A price event is an OHLC bucket of trades. With the bar fill model it is one dollar bar; at
//! trade resolution it is one trade; in HPO it is a fixed-width bucket of trades
//! ([`bucket_trades`]).

use domain::{DollarBar, Trade};
use rust_decimal::Decimal;

use crate::error::BacktestError;

/// One OHLC price observation the execution core fills orders and evaluates exits against.
///
/// `start_ms` is the timestamp of the first trade in the event and `end_ms` that of the last.
/// Pending orders become due against `start_ms` (an order fills at the first event whose start
/// is at or after its due time), while day and month rollovers, funding windows, exit times and
/// the decision timestamp use `end_ms`, exactly as the bar engine used `DollarBar::end_time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PriceEvent {
    /// Timestamp of the first trade in the event, in Unix milliseconds.
    pub start_ms: i64,
    /// Timestamp of the last trade in the event, in Unix milliseconds.
    pub end_ms: i64,
    /// First traded price.
    pub open: Decimal,
    /// Highest traded price.
    pub high: Decimal,
    /// Lowest traded price.
    pub low: Decimal,
    /// Last traded price.
    pub close: Decimal,
    /// Traded asset quantity/volume in this event.
    pub volume: Decimal,
    /// Traded quote dollar volume in this event.
    pub dollar_volume: Decimal,
    /// Buy-initiated quote dollar volume.
    pub buy_dollar_volume: Decimal,
    /// Sell-initiated quote dollar volume.
    pub sell_dollar_volume: Decimal,
}

impl From<&DollarBar> for PriceEvent {
    /// The bar's OHLC and volume metrics, with `start_ms = start_time` and `end_ms = end_time`.
    fn from(bar: &DollarBar) -> Self {
        Self {
            start_ms: bar.start_time,
            end_ms: bar.end_time,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
            dollar_volume: bar.dollar_volume,
            buy_dollar_volume: bar.buy_dollar_volume,
            sell_dollar_volume: bar.sell_dollar_volume,
        }
    }
}

impl From<&Trade> for PriceEvent {
    /// A flat event at the trade's price and timestamp.
    fn from(trade: &Trade) -> Self {
        let dv = trade.price * trade.quantity;
        Self {
            start_ms: trade.timestamp,
            end_ms: trade.timestamp,
            open: trade.price,
            high: trade.price,
            low: trade.price,
            close: trade.price,
            volume: trade.quantity,
            dollar_volume: dv,
            buy_dollar_volume: match trade.side {
                domain::Side::Buy => dv,
                domain::Side::Sell => Decimal::ZERO,
            },
            sell_dollar_volume: match trade.side {
                domain::Side::Buy => Decimal::ZERO,
                domain::Side::Sell => dv,
            },
        }
    }
}

/// Groups time-ordered trades into UTC-epoch-aligned buckets `[k·b, (k+1)·b)` of `bucket_ms`
/// milliseconds and returns one [`PriceEvent`] per non-empty bucket.
///
/// Each event starts at its first trade's timestamp and ends at its last; open and close follow
/// trade order. Empty buckets produce no event. A `bucket_ms` that is not positive is rejected
/// with [`BacktestError::InvalidPriceBucket`].
pub fn bucket_trades(
    trades: impl IntoIterator<Item = Trade>,
    bucket_ms: i64,
) -> Result<Vec<PriceEvent>, BacktestError> {
    if bucket_ms <= 0 {
        return Err(BacktestError::InvalidPriceBucket(bucket_ms));
    }
    let mut events = Vec::new();
    let mut current: Option<(i64, PriceEvent)> = None;
    for trade in trades {
        let bucket = trade.timestamp.div_euclid(bucket_ms);
        match current.as_mut() {
            Some((key, event)) if *key == bucket => {
                event.end_ms = trade.timestamp;
                event.high = event.high.max(trade.price);
                event.low = event.low.min(trade.price);
                event.close = trade.price;
                event.volume += trade.quantity;
                let dv = trade.price * trade.quantity;
                event.dollar_volume += dv;
                match trade.side {
                    domain::Side::Buy => event.buy_dollar_volume += dv,
                    domain::Side::Sell => event.sell_dollar_volume += dv,
                }
            }
            _ => {
                if let Some((_, event)) = current.take() {
                    events.push(event);
                }
                current = Some((bucket, PriceEvent::from(&trade)));
            }
        }
    }
    if let Some((_, event)) = current {
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DollarBar, Side, Trade};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    fn trade(timestamp: i64, price: Decimal) -> Trade {
        Trade {
            timestamp,
            price,
            quantity: dec!(1),
            side: Side::Buy,
        }
    }

    #[test]
    fn a_dollar_bar_maps_to_its_ohlc_and_both_times() {
        let bar = DollarBar {
            start_time: 1_000,
            end_time: 2_000,
            open: dec!(10),
            high: dec!(12),
            low: dec!(9),
            close: dec!(11),
            volume: dec!(1),
            dollar_volume: dec!(11),
            trade_count: 2,
            buy_volume: dec!(0.5),
            sell_volume: dec!(0.5),
            buy_dollar_volume: dec!(5.5),
            sell_dollar_volume: dec!(5.5),
        };
        assert_eq!(
            PriceEvent::from(&bar),
            PriceEvent {
                start_ms: 1_000,
                end_ms: 2_000,
                open: dec!(10),
                high: dec!(12),
                low: dec!(9),
                close: dec!(11),
                volume: dec!(1),
                dollar_volume: dec!(11),
                buy_dollar_volume: dec!(5.5),
                sell_dollar_volume: dec!(5.5),
            }
        );
    }

    #[test]
    fn a_trade_maps_to_a_flat_event_at_its_timestamp() {
        assert_eq!(
            PriceEvent::from(&trade(1_234, dec!(50.5))),
            PriceEvent {
                start_ms: 1_234,
                end_ms: 1_234,
                open: dec!(50.5),
                high: dec!(50.5),
                low: dec!(50.5),
                close: dec!(50.5),
                volume: dec!(1),
                dollar_volume: dec!(50.5),
                buy_dollar_volume: dec!(50.5),
                sell_dollar_volume: Decimal::ZERO,
            }
        );
    }

    #[test]
    fn buckets_are_epoch_aligned_and_a_trade_on_the_edge_opens_the_next_bucket() {
        let trades = vec![
            trade(999, dec!(1)),
            trade(1_000, dec!(2)),
            trade(1_999, dec!(3)),
            trade(2_000, dec!(4)),
        ];
        let events = bucket_trades(trades, 1_000).expect("positive bucket");
        assert_eq!(
            events
                .iter()
                .map(|e| (e.start_ms, e.end_ms))
                .collect::<Vec<_>>(),
            vec![(999, 999), (1_000, 1_999), (2_000, 2_000)]
        );
    }

    #[test]
    fn empty_buckets_produce_no_event() {
        let trades = vec![trade(100, dec!(1)), trade(5_500, dec!(2))];
        let events = bucket_trades(trades, 1_000).expect("positive bucket");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].start_ms, 100);
        assert_eq!(events[1].start_ms, 5_500);
    }

    #[test]
    fn a_bucket_takes_open_and_close_in_trade_order_and_the_extremes() {
        let trades = vec![
            trade(1_100, dec!(5)),
            trade(1_200, dec!(9)),
            trade(1_300, dec!(2)),
            trade(1_400, dec!(4)),
        ];
        let events = bucket_trades(trades, 1_000).expect("positive bucket");
        assert_eq!(
            events,
            vec![PriceEvent {
                start_ms: 1_100,
                end_ms: 1_400,
                open: dec!(5),
                high: dec!(9),
                low: dec!(2),
                close: dec!(4),
                volume: dec!(4),
                dollar_volume: dec!(20),
                buy_dollar_volume: dec!(20),
                sell_dollar_volume: Decimal::ZERO,
            }]
        );
    }

    #[test]
    fn negative_timestamps_bucket_by_floor_division() {
        let trades = vec![trade(-1, dec!(1)), trade(0, dec!(2))];
        let events = bucket_trades(trades, 1_000).expect("positive bucket");
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn a_bucket_that_is_not_positive_is_rejected() {
        for bucket_ms in [0, -1] {
            assert_eq!(
                bucket_trades(vec![trade(1, dec!(1))], bucket_ms),
                Err(BacktestError::InvalidPriceBucket(bucket_ms))
            );
        }
    }
}
