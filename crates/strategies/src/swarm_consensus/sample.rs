//! The features the swarm ensemble hands to the consensus policy (strategy spec §9.3).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Votes of the whole swarm right after one or more feeds closed a bar.
///
/// The ensemble emits one sample per trade that closed at least one feed, once every feed is
/// warm. Counts are over all `agents`; an agent's vote persists until its own feed closes again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsensusSample {
    /// Timestamp of the trade that closed the feed(s) (Unix ms).
    pub timestamp: i64,
    /// Number of agents in the swarm (`N`).
    pub agents: u32,
    /// Agents voting +1 on the long axis.
    pub long_up: u32,
    /// Agents voting -1 on the long axis.
    pub long_down: u32,
    /// Agents voting +1 on the short axis.
    pub short_up: u32,
    /// Agents voting -1 on the short axis.
    pub short_down: u32,
    /// Agents whose vote is `(0, 0)`.
    pub waits: u32,
    /// Agents whose last evaluation ended in a conflict.
    pub conflicts: u32,
    /// ATR (price units) of feed 1 (time, 1h), the reference of the disaster stop.
    pub atr_stop_reference: Decimal,
    /// Price of the trade that produced the sample (audit only).
    pub price: Decimal,
}

impl ConsensusSample {
    /// Net long vote as a fraction of the swarm, in `[-1, 1]`; 0 for an empty swarm.
    pub fn long_net(&self) -> Decimal {
        net(self.long_up, self.long_down, self.agents)
    }

    /// Net short vote as a fraction of the swarm, in `[-1, 1]`; 0 for an empty swarm.
    pub fn short_net(&self) -> Decimal {
        net(self.short_up, self.short_down, self.agents)
    }
}

fn net(up: u32, down: u32, agents: u32) -> Decimal {
    if agents == 0 {
        return Decimal::ZERO;
    }
    (Decimal::from(up) - Decimal::from(down)) / Decimal::from(agents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample(long_up: u32, long_down: u32, short_up: u32, short_down: u32) -> ConsensusSample {
        ConsensusSample {
            timestamp: 1,
            agents: 96,
            long_up,
            long_down,
            short_up,
            short_down,
            waits: 0,
            conflicts: 0,
            atr_stop_reference: dec!(500),
            price: dec!(40000),
        }
    }

    #[test]
    fn net_votes_are_fractions_of_the_swarm() {
        let s = sample(60, 12, 6, 30);
        assert_eq!(s.long_net(), dec!(0.5));
        assert_eq!(s.short_net(), dec!(-0.25));
    }

    #[test]
    fn an_empty_swarm_has_no_net_vote() {
        let mut s = sample(0, 0, 0, 0);
        s.agents = 0;
        assert_eq!(s.long_net(), Decimal::ZERO);
    }
}
