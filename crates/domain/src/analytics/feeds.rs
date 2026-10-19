//! Trade-driven bar feeds: time, tick, volume, and range bars.
//!
//! Every aggregator consumes a stream of [`Trade`]s and emits a completed
//! [`FeedBar`] when its closing rule fires. The rules follow the swarm
//! consensus strategy specification (§5.3):
//!
//! - **Time bars** are aligned to UTC epoch multiples of the interval `Δ`.
//!   Bucket `b` covers `[b·Δ, (b+1)·Δ)`. The bar of bucket `b` is emitted when
//!   the first trade with `timestamp ≥ (b+1)·Δ` arrives; that trade does not
//!   belong to the emitted bar and opens the next one. Empty buckets emit
//!   nothing.
//! - **Tick, volume, and range bars** close on the trade that reaches the
//!   threshold, and that trade belongs to the closed bar. No volume is carried
//!   into the next bar.
//!
//! A partial bar at the end of the input is never emitted.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::trade::Trade;

/// A completed bar emitted by a [`BarAggregator`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedBar {
    /// Timestamp of the first trade in the bar (inclusive, ms).
    pub start_time: i64,
    /// Timestamp of the last trade in the bar (inclusive, ms).
    pub end_time: i64,
    /// Price of the first trade.
    pub open: Decimal,
    /// Highest traded price.
    pub high: Decimal,
    /// Lowest traded price.
    pub low: Decimal,
    /// Price of the last trade.
    pub close: Decimal,
    /// Sum of base-asset quantity.
    pub volume: Decimal,
    /// Number of trades aggregated into the bar.
    pub trade_count: u64,
}

/// Streaming contract shared by every bar feed.
pub trait BarAggregator {
    /// Ingests one trade and returns a bar when the closing rule fires.
    fn process_trade(&mut self, trade: &Trade) -> Option<FeedBar>;

    /// Discards the bar in formation, returning to the freshly built state.
    fn reset(&mut self);
}

/// OHLCV accumulation of the bar in formation, shared by every aggregator.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InProgressBar {
    start_time: i64,
    end_time: i64,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Decimal,
    trade_count: u64,
}

impl InProgressBar {
    fn new(trade: &Trade) -> Self {
        Self {
            start_time: trade.timestamp,
            end_time: trade.timestamp,
            open: trade.price,
            high: trade.price,
            low: trade.price,
            close: trade.price,
            volume: trade.quantity,
            trade_count: 1,
        }
    }

    fn update(&mut self, trade: &Trade) {
        if trade.price > self.high {
            self.high = trade.price;
        }
        if trade.price < self.low {
            self.low = trade.price;
        }
        self.close = trade.price;
        self.end_time = trade.timestamp;
        self.volume += trade.quantity;
        self.trade_count += 1;
    }

    /// Relative range `(high - low) / open`, or `None` for a zero open price.
    fn relative_range(&self) -> Option<Decimal> {
        (self.high - self.low).checked_div(self.open)
    }

    fn into_feed_bar(self) -> FeedBar {
        FeedBar {
            start_time: self.start_time,
            end_time: self.end_time,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            trade_count: self.trade_count,
        }
    }
}

/// Adds `trade` to the bar in formation (opening one if needed) and closes the
/// bar on this same trade when `closes` holds for the updated accumulation.
///
/// This is the shared rule of tick, volume, and range bars: the trade that
/// reaches the threshold belongs to the bar it closes.
fn accumulate_and_close(
    current: &mut Option<InProgressBar>,
    trade: &Trade,
    closes: impl Fn(&InProgressBar) -> bool,
) -> Option<FeedBar> {
    let bar = match current.take() {
        Some(mut bar) => {
            bar.update(trade);
            bar
        }
        None => InProgressBar::new(trade),
    };
    if closes(&bar) {
        Some(bar.into_feed_bar())
    } else {
        *current = Some(bar);
        None
    }
}

/// Clock-aligned time bars of a fixed interval in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeBarAggregator {
    interval_ms: i64,
    bucket: i64,
    current: Option<InProgressBar>,
}

impl TimeBarAggregator {
    /// Builds an aggregator for a strictly positive interval in milliseconds.
    pub fn new(interval_ms: i64) -> Result<Self, DomainError> {
        if interval_ms <= 0 {
            return Err(DomainError::InvalidThreshold(interval_ms.to_string()));
        }
        Ok(Self {
            interval_ms,
            bucket: 0,
            current: None,
        })
    }
}

impl BarAggregator for TimeBarAggregator {
    /// Feeds one trade. Emits the bar of the open bucket when `trade` falls in a
    /// later bucket; `trade` itself then opens the bar of its own bucket.
    ///
    /// A trade stamped earlier than the open bucket (out-of-order input) is
    /// absorbed into the bar in formation rather than reopening a past bucket.
    fn process_trade(&mut self, trade: &Trade) -> Option<FeedBar> {
        let bucket = trade.timestamp.div_euclid(self.interval_ms);
        match self.current.take() {
            Some(bar) if bucket > self.bucket => {
                self.bucket = bucket;
                self.current = Some(InProgressBar::new(trade));
                Some(bar.into_feed_bar())
            }
            Some(mut bar) => {
                bar.update(trade);
                self.current = Some(bar);
                None
            }
            None => {
                self.bucket = bucket;
                self.current = Some(InProgressBar::new(trade));
                None
            }
        }
    }

    fn reset(&mut self) {
        self.bucket = 0;
        self.current = None;
    }
}

/// Bars that close after a fixed number of trades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickBarAggregator {
    count: u64,
    current: Option<InProgressBar>,
}

impl TickBarAggregator {
    /// Builds an aggregator that closes a bar every `count` trades (`count > 0`).
    pub fn new(count: u64) -> Result<Self, DomainError> {
        if count == 0 {
            return Err(DomainError::InvalidThreshold(count.to_string()));
        }
        Ok(Self {
            count,
            current: None,
        })
    }
}

impl BarAggregator for TickBarAggregator {
    fn process_trade(&mut self, trade: &Trade) -> Option<FeedBar> {
        let count = self.count;
        accumulate_and_close(&mut self.current, trade, |bar| bar.trade_count >= count)
    }

    fn reset(&mut self) {
        self.current = None;
    }
}

/// Bars that close once the traded base volume reaches a threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeBarAggregator {
    volume: Decimal,
    current: Option<InProgressBar>,
}

impl VolumeBarAggregator {
    /// Builds an aggregator for a strictly positive base-volume threshold.
    pub fn new(volume: Decimal) -> Result<Self, DomainError> {
        if volume <= Decimal::ZERO {
            return Err(DomainError::InvalidThreshold(volume.to_string()));
        }
        Ok(Self {
            volume,
            current: None,
        })
    }
}

impl BarAggregator for VolumeBarAggregator {
    fn process_trade(&mut self, trade: &Trade) -> Option<FeedBar> {
        let volume = self.volume;
        accumulate_and_close(&mut self.current, trade, |bar| bar.volume >= volume)
    }

    fn reset(&mut self) {
        self.current = None;
    }
}

/// Bars that close once the relative range `(high - low) / open` reaches a threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeBarAggregator {
    relative_range: Decimal,
    current: Option<InProgressBar>,
}

impl RangeBarAggregator {
    /// Builds an aggregator for a strictly positive relative-range threshold.
    pub fn new(relative_range: Decimal) -> Result<Self, DomainError> {
        if relative_range <= Decimal::ZERO {
            return Err(DomainError::InvalidThreshold(relative_range.to_string()));
        }
        Ok(Self {
            relative_range,
            current: None,
        })
    }
}

impl BarAggregator for RangeBarAggregator {
    fn process_trade(&mut self, trade: &Trade) -> Option<FeedBar> {
        let threshold = self.relative_range;
        accumulate_and_close(&mut self.current, trade, |bar| {
            bar.relative_range().is_some_and(|range| range >= threshold)
        })
    }

    fn reset(&mut self) {
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::Side;
    use rust_decimal_macros::dec;

    fn trade(timestamp: i64, price: Decimal, quantity: Decimal) -> Trade {
        Trade::new(timestamp, price, quantity, Side::Buy).unwrap()
    }

    fn run(agg: &mut dyn BarAggregator, trades: &[Trade]) -> Vec<FeedBar> {
        trades.iter().filter_map(|t| agg.process_trade(t)).collect()
    }

    #[test]
    fn constructors_reject_non_positive_thresholds() {
        assert_eq!(
            TimeBarAggregator::new(0),
            Err(DomainError::InvalidThreshold("0".to_string()))
        );
        assert!(TimeBarAggregator::new(-1).is_err());
        assert!(TickBarAggregator::new(0).is_err());
        assert!(VolumeBarAggregator::new(dec!(0)).is_err());
        assert!(VolumeBarAggregator::new(dec!(-1)).is_err());
        assert!(RangeBarAggregator::new(dec!(0)).is_err());
        assert!(RangeBarAggregator::new(dec!(-0.01)).is_err());
        assert!(TimeBarAggregator::new(1).is_ok());
        assert!(TickBarAggregator::new(1).is_ok());
        assert!(VolumeBarAggregator::new(dec!(0.001)).is_ok());
        assert!(RangeBarAggregator::new(dec!(0.000001)).is_ok());
    }

    #[test]
    fn time_bar_trade_at_bucket_end_opens_next_bar() {
        let mut agg = TimeBarAggregator::new(1_000).unwrap();
        // Bucket 0 = [0, 1000). 999 = (b+1)·Δ − 1 stays in bucket 0.
        assert!(agg.process_trade(&trade(100, dec!(10), dec!(1))).is_none());
        assert!(agg.process_trade(&trade(999, dec!(11), dec!(1))).is_none());
        // 1000 = (b+1)·Δ closes bucket 0 and does not belong to it.
        let bar = agg
            .process_trade(&trade(1_000, dec!(50), dec!(7)))
            .expect("bucket 0 closes");
        assert_eq!(bar.start_time, 100);
        assert_eq!(bar.end_time, 999);
        assert_eq!(bar.close, dec!(11));
        assert_eq!(bar.high, dec!(11));
        assert_eq!(bar.volume, dec!(2));
        assert_eq!(bar.trade_count, 2);
        // The closing trade opened bucket 1.
        let next = agg
            .process_trade(&trade(2_000, dec!(1), dec!(1)))
            .expect("bucket 1 closes");
        assert_eq!(next.start_time, 1_000);
        assert_eq!(next.end_time, 1_000);
        assert_eq!(next.open, dec!(50));
        assert_eq!(next.volume, dec!(7));
        assert_eq!(next.trade_count, 1);
    }

    #[test]
    fn time_bar_buckets_are_epoch_aligned() {
        let mut agg = TimeBarAggregator::new(1_000).unwrap();
        // First trade at 1500 lives in bucket 1 = [1000, 2000); 1999 stays there.
        assert!(agg
            .process_trade(&trade(1_500, dec!(10), dec!(1)))
            .is_none());
        assert!(agg
            .process_trade(&trade(1_999, dec!(10), dec!(1)))
            .is_none());
        let bar = agg.process_trade(&trade(2_000, dec!(10), dec!(1))).unwrap();
        assert_eq!(
            (bar.start_time, bar.end_time, bar.trade_count),
            (1_500, 1_999, 2)
        );
    }

    #[test]
    fn time_bar_empty_buckets_emit_nothing() {
        let mut agg = TimeBarAggregator::new(1_000).unwrap();
        assert!(agg.process_trade(&trade(500, dec!(10), dec!(1))).is_none());
        // Jump over buckets 1..=4: exactly one bar (bucket 0) is emitted.
        let bar = agg.process_trade(&trade(5_200, dec!(12), dec!(1))).unwrap();
        assert_eq!((bar.start_time, bar.end_time), (500, 500));
        // Bucket 5 is open; nothing more until a trade at >= 6000.
        assert!(agg
            .process_trade(&trade(5_999, dec!(12), dec!(1)))
            .is_none());
        let bar = agg.process_trade(&trade(6_000, dec!(12), dec!(1))).unwrap();
        assert_eq!(
            (bar.start_time, bar.end_time, bar.trade_count),
            (5_200, 5_999, 2)
        );
    }

    #[test]
    fn exact_ohlcv_and_trade_count() {
        let trades = [
            trade(0, dec!(100), dec!(0.5)),
            trade(1, dec!(103), dec!(0.25)),
            trade(2, dec!(98), dec!(1.125)),
            trade(3, dec!(101), dec!(0.125)),
        ];
        let expected = FeedBar {
            start_time: 0,
            end_time: 3,
            open: dec!(100),
            high: dec!(103),
            low: dec!(98),
            close: dec!(101),
            volume: dec!(2.0),
            trade_count: 4,
        };
        let mut tick = TickBarAggregator::new(4).unwrap();
        assert_eq!(run(&mut tick, &trades), vec![expected.clone()]);

        let mut time = TimeBarAggregator::new(10).unwrap();
        let mut with_closer = trades.to_vec();
        with_closer.push(trade(10, dec!(1), dec!(1)));
        assert_eq!(run(&mut time, &with_closer), vec![expected]);
    }

    #[test]
    fn tick_bar_closes_exactly_at_threshold() {
        let mut agg = TickBarAggregator::new(3).unwrap();
        assert!(agg.process_trade(&trade(1, dec!(10), dec!(1))).is_none());
        assert!(agg.process_trade(&trade(2, dec!(11), dec!(1))).is_none());
        let bar = agg.process_trade(&trade(3, dec!(12), dec!(1))).unwrap();
        assert_eq!(bar.trade_count, 3);
        assert_eq!((bar.start_time, bar.end_time), (1, 3));
        assert_eq!(bar.close, dec!(12));
        // The next trade opens a new bar.
        assert!(agg.process_trade(&trade(4, dec!(13), dec!(1))).is_none());
        assert!(agg.process_trade(&trade(5, dec!(13), dec!(1))).is_none());
        let bar = agg.process_trade(&trade(6, dec!(14), dec!(1))).unwrap();
        assert_eq!(
            (bar.start_time, bar.open, bar.trade_count),
            (4, dec!(13), 3)
        );
    }

    #[test]
    fn volume_bar_closes_exactly_at_threshold() {
        let mut agg = VolumeBarAggregator::new(dec!(1.5)).unwrap();
        assert!(agg.process_trade(&trade(1, dec!(10), dec!(0.5))).is_none());
        assert!(agg
            .process_trade(&trade(2, dec!(10), dec!(0.999)))
            .is_none());
        let bar = agg.process_trade(&trade(3, dec!(10), dec!(0.001))).unwrap();
        assert_eq!(bar.volume, dec!(1.5));
        assert_eq!(bar.trade_count, 3);
        assert_eq!(bar.end_time, 3);
    }

    #[test]
    fn volume_bar_carries_no_excess_volume() {
        let mut agg = VolumeBarAggregator::new(dec!(1)).unwrap();
        // One trade of 5 closes a bar of volume 5 (true volume, no split).
        let bar = agg.process_trade(&trade(1, dec!(10), dec!(5))).unwrap();
        assert_eq!(bar.volume, dec!(5));
        // The next bar starts from zero: 0.6 does not close it.
        assert!(agg.process_trade(&trade(2, dec!(10), dec!(0.6))).is_none());
        let bar = agg.process_trade(&trade(3, dec!(10), dec!(0.4))).unwrap();
        assert_eq!(bar.volume, dec!(1.0));
        assert_eq!((bar.start_time, bar.trade_count), (2, 2));
    }

    #[test]
    fn range_bar_closes_exactly_at_threshold() {
        let mut agg = RangeBarAggregator::new(dec!(0.01)).unwrap();
        assert!(agg.process_trade(&trade(1, dec!(100), dec!(1))).is_none());
        assert!(agg
            .process_trade(&trade(2, dec!(100.99), dec!(1)))
            .is_none());
        // (101 - 100) / 100 = 0.01 closes the bar.
        let bar = agg.process_trade(&trade(3, dec!(101), dec!(1))).unwrap();
        assert_eq!(
            (bar.high, bar.low, bar.close),
            (dec!(101), dec!(100), dec!(101))
        );
        assert_eq!(bar.trade_count, 3);
        // Downward range uses the open as denominator too: (200 - 198) / 200 = 0.01.
        assert!(agg.process_trade(&trade(4, dec!(200), dec!(1))).is_none());
        assert!(agg
            .process_trade(&trade(5, dec!(198.01), dec!(1)))
            .is_none());
        let bar = agg.process_trade(&trade(6, dec!(198), dec!(1))).unwrap();
        assert_eq!(
            (bar.open, bar.low, bar.start_time),
            (dec!(200), dec!(198), 4)
        );
    }

    #[test]
    fn range_jump_keeps_true_high_and_low() {
        let mut agg = RangeBarAggregator::new(dec!(0.01)).unwrap();
        assert!(agg.process_trade(&trade(1, dec!(100), dec!(1))).is_none());
        assert!(agg.process_trade(&trade(2, dec!(99.5), dec!(1))).is_none());
        let bar = agg.process_trade(&trade(3, dec!(110), dec!(1))).unwrap();
        assert_eq!(bar.high, dec!(110));
        assert_eq!(bar.low, dec!(99.5));
        assert_eq!(bar.close, dec!(110));
    }

    #[test]
    fn flat_price_never_closes_a_range_bar() {
        let mut agg = RangeBarAggregator::new(dec!(0.000001)).unwrap();
        let trades: Vec<Trade> = (0..1_000).map(|i| trade(i, dec!(100), dec!(1))).collect();
        assert!(run(&mut agg, &trades).is_empty());
    }

    #[test]
    fn reset_equals_fresh() {
        let trades = [trade(10, dec!(100), dec!(1)), trade(20, dec!(105), dec!(1))];
        let mut aggs: Vec<(Box<dyn BarAggregator>, Box<dyn BarAggregator>)> = vec![
            (
                Box::new(TimeBarAggregator::new(1_000).unwrap()),
                Box::new(TimeBarAggregator::new(1_000).unwrap()),
            ),
            (
                Box::new(TickBarAggregator::new(3).unwrap()),
                Box::new(TickBarAggregator::new(3).unwrap()),
            ),
            (
                Box::new(VolumeBarAggregator::new(dec!(3)).unwrap()),
                Box::new(VolumeBarAggregator::new(dec!(3)).unwrap()),
            ),
            (
                Box::new(RangeBarAggregator::new(dec!(0.5)).unwrap()),
                Box::new(RangeBarAggregator::new(dec!(0.5)).unwrap()),
            ),
        ];
        let follow = [
            trade(3_000, dec!(50), dec!(1)),
            trade(3_100, dec!(80), dec!(1)),
            trade(3_200, dec!(60), dec!(1)),
            trade(9_000, dec!(61), dec!(1)),
        ];
        for (used, fresh) in aggs.iter_mut() {
            for t in &trades {
                used.process_trade(t);
            }
            used.reset();
            assert_eq!(run(used.as_mut(), &follow), run(fresh.as_mut(), &follow));
        }

        let mut time = TimeBarAggregator::new(1_000).unwrap();
        time.process_trade(&trades[0]);
        time.reset();
        assert_eq!(time, TimeBarAggregator::new(1_000).unwrap());
        let mut tick = TickBarAggregator::new(3).unwrap();
        tick.process_trade(&trades[0]);
        tick.reset();
        assert_eq!(tick, TickBarAggregator::new(3).unwrap());
        let mut volume = VolumeBarAggregator::new(dec!(3)).unwrap();
        volume.process_trade(&trades[0]);
        volume.reset();
        assert_eq!(volume, VolumeBarAggregator::new(dec!(3)).unwrap());
        let mut range = RangeBarAggregator::new(dec!(0.5)).unwrap();
        range.process_trade(&trades[0]);
        range.reset();
        assert_eq!(range, RangeBarAggregator::new(dec!(0.5)).unwrap());
    }

    #[test]
    fn no_bar_is_emitted_at_end_of_input() {
        let trades = [
            trade(0, dec!(100), dec!(1)),
            trade(1, dec!(100.5), dec!(1)),
            trade(2, dec!(100.2), dec!(1)),
        ];
        let mut time = TimeBarAggregator::new(1_000).unwrap();
        let mut tick = TickBarAggregator::new(4).unwrap();
        let mut volume = VolumeBarAggregator::new(dec!(4)).unwrap();
        let mut range = RangeBarAggregator::new(dec!(0.5)).unwrap();
        assert!(run(&mut time, &trades).is_empty());
        assert!(run(&mut tick, &trades).is_empty());
        assert!(run(&mut volume, &trades).is_empty());
        assert!(run(&mut range, &trades).is_empty());
    }

    #[test]
    fn feed_bar_round_trips_through_json() {
        let bar = FeedBar {
            start_time: 1,
            end_time: 2,
            open: dec!(1.5),
            high: dec!(2),
            low: dec!(1),
            close: dec!(1.75),
            volume: dec!(0.001),
            trade_count: 2,
        };
        let json = serde_json::to_string(&bar).unwrap();
        assert_eq!(serde_json::from_str::<FeedBar>(&json).unwrap(), bar);
    }
}
