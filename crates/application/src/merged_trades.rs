//! Several instruments' trade streams replayed as one, in time order.

use domain::{InstrumentId, Trade};
use ports::MarketDataStream;

/// The trades of several streams, one per instrument, merged by timestamp. Trades of equal
/// timestamp come in instrument order, so a replay is deterministic.
pub struct MergedTrades<E> {
    lanes: Vec<MergedLane<E>>,
}

struct MergedLane<E> {
    instrument: InstrumentId,
    stream: Box<dyn MarketDataStream<Error = E>>,
    head: Option<Trade>,
    primed: bool,
}

impl<E> MergedTrades<E> {
    /// Merges `streams`, each the time-ordered trades of its instrument.
    pub fn new(streams: Vec<(InstrumentId, Box<dyn MarketDataStream<Error = E>>)>) -> Self {
        let mut lanes: Vec<MergedLane<E>> = streams
            .into_iter()
            .map(|(instrument, stream)| MergedLane {
                instrument,
                stream,
                head: None,
                primed: false,
            })
            .collect();
        lanes.sort_by(|a, b| a.instrument.cmp(&b.instrument));
        Self { lanes }
    }

    /// The next trade of any instrument, with its instrument; `None` when every stream ended.
    pub fn next_trade(&mut self) -> Result<Option<(InstrumentId, Trade)>, E> {
        for lane in &mut self.lanes {
            if !lane.primed {
                lane.head = lane.stream.next_trade()?;
                lane.primed = true;
            }
        }
        let next = self
            .lanes
            .iter()
            .enumerate()
            .filter_map(|(index, lane)| lane.head.as_ref().map(|trade| (trade.timestamp, index)))
            .min();
        let Some((_, index)) = next else {
            return Ok(None);
        };
        let lane = &mut self.lanes[index];
        let trade = lane.stream.next_trade()?;
        let current = std::mem::replace(&mut lane.head, trade);
        Ok(current.map(|trade| (lane.instrument.clone(), trade)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Side;
    use rust_decimal_macros::dec;
    use std::collections::VecDeque;

    struct Scripted(VecDeque<Trade>);

    impl MarketDataStream for Scripted {
        type Error = ();

        fn next_trade(&mut self) -> Result<Option<Trade>, ()> {
            Ok(self.0.pop_front())
        }
    }

    fn at(times: &[i64]) -> Box<dyn MarketDataStream<Error = ()>> {
        Box::new(Scripted(
            times
                .iter()
                .map(|t| Trade::new(*t, dec!(1), dec!(1), Side::Buy).unwrap())
                .collect(),
        ))
    }

    #[test]
    fn trades_come_in_time_order_and_ties_in_instrument_order() {
        let mut merged = MergedTrades::new(vec![
            (InstrumentId::from_static("ETH"), at(&[1, 3, 3, 6])),
            (InstrumentId::from_static("BTC"), at(&[2, 3, 5])),
        ]);
        let mut seen = Vec::new();
        while let Some((id, trade)) = merged.next_trade().unwrap() {
            seen.push(format!("{}@{}", id, trade.timestamp));
        }
        assert_eq!(
            seen,
            ["ETH@1", "BTC@2", "BTC@3", "ETH@3", "ETH@3", "BTC@5", "ETH@6"]
        );
    }
}
