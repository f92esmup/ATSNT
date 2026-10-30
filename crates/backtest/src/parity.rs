//! Simulation-versus-live parity: replay the market orders a live run actually sent on the
//! tape that was trading at that moment, and measure how far the simulated fills sit from the
//! real ones.
//!
//! The comparison replays on the simulated venue of [`crate::wiring::parity_venue`], the one the
//! tick-level simulation and the paper session use, so the latency it recommends is the one to
//! configure there. Inputs are the fills `live_trading --record-fills` writes and a tape of the
//! same market (for production, the `aggTrades` archive of that day).
//!
//! The submit time is the local wall clock and the fill time is the exchange's, so clock skew
//! shifts the time error one-to-one; the price error is what the latency sweep is scored on.

use domain::{Side, Trade};
use ports::TapeVenue;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// One market order a live run sent and the fill the exchange reported for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedFill {
    /// Local wall-clock time the order was sent, Unix milliseconds.
    pub submitted_ms: i64,
    /// Exchange time of the last execution of the order, Unix milliseconds.
    pub filled_ms: i64,
    /// Direction of the order.
    pub side: Side,
    /// Executed quantity.
    pub quantity: Decimal,
    /// Volume-weighted average execution price.
    pub avg_price: Decimal,
}

/// What the simulated venue did with the same order on the same tape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatedFill {
    /// Time of the last simulated execution.
    pub filled_ms: i64,
    /// Volume-weighted average simulated price.
    pub avg_price: Decimal,
    /// Quantity the tape let the order fill.
    pub quantity: Decimal,
}

/// An observed fill next to its simulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillComparison {
    /// The live fill.
    pub observed: ObservedFill,
    /// The simulated fill, `None` when the tape ended before the order filled at all.
    pub simulated: Option<SimulatedFill>,
    /// How much worse the simulated price is than the real one, in basis points of the real
    /// price: positive when the simulation is pessimistic, negative when it is optimistic.
    pub adverse_error_bps: Option<Decimal>,
    /// Simulated minus observed fill time in milliseconds.
    pub time_error_ms: Option<i64>,
}

/// How well one latency setting reproduces a set of fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatencyScore {
    /// Latency under test in milliseconds.
    pub latency_ms: i64,
    /// Fills the simulation completed.
    pub matched: usize,
    /// Mean absolute price error in basis points.
    pub mean_abs_error_bps: Decimal,
    /// Mean signed price error in basis points (positive: pessimistic simulation).
    pub mean_bias_bps: Decimal,
}

/// Parses the JSON-lines fills `live_trading --record-fills` writes; blank lines are skipped.
pub fn parse_fills_jsonl(text: &str) -> Result<Vec<ObservedFill>, serde_json::Error> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect()
}

/// Time range of a tape that covers every fill: from a second before the first order was sent
/// to a minute after the last execution (room for a slow walk of the book).
pub fn tape_window(fills: &[ObservedFill]) -> Option<(i64, i64)> {
    let start = fills.iter().map(|fill| fill.submitted_ms).min()?;
    let end = fills.iter().map(|fill| fill.filled_ms).max()?;
    Some((start - 1_000, end + 60_000))
}

/// Appends the entry fills of a live run to a JSON-lines file for [`compare_fills`].
///
/// The caller reports the local wall-clock time an entry order is sent and, once the position
/// opened, its average fill; the recorder pairs them.
pub struct FillRecorder {
    path: std::path::PathBuf,
    submitted_ms: std::sync::Mutex<Option<i64>>,
}

impl FillRecorder {
    /// A recorder that appends to `path`, creating it on the first fill.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            submitted_ms: std::sync::Mutex::new(None),
        }
    }

    /// Notes the local time an entry order was sent.
    pub fn on_entry_submitted(&self, now_ms: i64) {
        *self
            .submitted_ms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(now_ms);
    }

    /// Forgets the pending submission: its send time is unknown, so its fill must not be
    /// recorded with a guessed one.
    pub fn discard_pending(&self) {
        *self
            .submitted_ms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// Appends the fill of the entry sent last. Returns `None` when no order was noted.
    pub fn on_entry_filled(
        &self,
        side: Side,
        quantity: Decimal,
        avg_price: Decimal,
        filled_ms: i64,
    ) -> std::io::Result<Option<ObservedFill>> {
        let submitted = self
            .submitted_ms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(submitted_ms) = submitted else {
            return Ok(None);
        };
        let fill = ObservedFill {
            submitted_ms,
            filled_ms,
            side,
            quantity,
            avg_price,
        };
        let mut line = serde_json::to_string(&fill).map_err(std::io::Error::other)?;
        line.push('\n');
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(line.as_bytes())?;
        Ok(Some(fill))
    }
}

/// Replays `order` on `tape` (sorted by time) on `venue`. The venue must be fresh: it keeps the
/// state of every order it has seen, and this replay submits one more.
pub fn simulate_market_order<V: TapeVenue>(
    venue: &V,
    tape: &[Trade],
    order: &ObservedFill,
) -> Option<SimulatedFill> {
    venue
        .submit_market_order_at(order.side, order.quantity, order.submitted_ms)
        .ok()?;
    let start = tape.partition_point(|trade| trade.timestamp < order.submitted_ms);
    let mut quantity = Decimal::ZERO;
    let mut notional = Decimal::ZERO;
    for trade in &tape[start..] {
        for report in venue.on_trade(trade) {
            quantity += report.last_quantity;
            notional += report.last_quantity * report.last_price;
            if report.order_filled {
                return Some(SimulatedFill {
                    filled_ms: report.time_ms,
                    avg_price: notional / quantity,
                    quantity,
                });
            }
        }
    }
    None
}

/// Compares every observed fill with its simulation at one latency. `new_venue` builds the fresh
/// venue of each replay at a latency; [`crate::wiring::parity_venue`] is the one to pass.
pub fn compare_fills<V: TapeVenue>(
    tape: &[Trade],
    fills: &[ObservedFill],
    latency_ms: i64,
    new_venue: impl Fn(i64) -> V,
) -> Vec<FillComparison> {
    fills
        .iter()
        .map(|observed| {
            let simulated = simulate_market_order(&new_venue(latency_ms), tape, observed);
            let adverse_error_bps = simulated.as_ref().map(|sim| {
                let diff = match observed.side {
                    Side::Buy => sim.avg_price - observed.avg_price,
                    Side::Sell => observed.avg_price - sim.avg_price,
                };
                diff / observed.avg_price * dec!(10_000)
            });
            let time_error_ms = simulated
                .as_ref()
                .map(|sim| sim.filled_ms - observed.filled_ms);
            FillComparison {
                observed: observed.clone(),
                simulated,
                adverse_error_bps,
                time_error_ms,
            }
        })
        .collect()
}

/// Scores each candidate latency by how closely it reproduces the observed prices.
pub fn sweep_latency<V: TapeVenue>(
    tape: &[Trade],
    fills: &[ObservedFill],
    latencies: impl IntoIterator<Item = i64>,
    new_venue: impl Fn(i64) -> V,
) -> Vec<LatencyScore> {
    latencies
        .into_iter()
        .map(|latency_ms| {
            let errors: Vec<Decimal> = compare_fills(tape, fills, latency_ms, &new_venue)
                .into_iter()
                .filter_map(|comparison| comparison.adverse_error_bps)
                .collect();
            let matched = errors.len();
            let count = Decimal::from(matched.max(1));
            LatencyScore {
                latency_ms,
                matched,
                mean_abs_error_bps: errors.iter().map(|e| e.abs()).sum::<Decimal>() / count,
                mean_bias_bps: errors.iter().sum::<Decimal>() / count,
            }
        })
        .collect()
}

/// The score with the smallest mean absolute error among those that matched every fill the
/// best-covering latency matched; ties go to the lower latency.
pub fn best_latency(scores: &[LatencyScore]) -> Option<&LatencyScore> {
    let most_matched = scores.iter().map(|score| score.matched).max()?;
    if most_matched == 0 {
        return None;
    }
    scores
        .iter()
        .filter(|score| score.matched == most_matched)
        .min_by(|a, b| {
            a.mean_abs_error_bps
                .cmp(&b.mean_abs_error_bps)
                .then(a.latency_ms.cmp(&b.latency_ms))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::parity_venue;

    fn print(timestamp: i64, price: Decimal, quantity: Decimal, side: Side) -> Trade {
        Trade::new(timestamp, price, quantity, side).unwrap()
    }

    /// A tape whose offers climb 0.1 every 50 ms, so every extra millisecond of latency costs
    /// a buyer money.
    fn rising_tape() -> Vec<Trade> {
        (0..40)
            .map(|i| {
                print(
                    1_000 + i * 50,
                    dec!(100) + Decimal::from(i) / dec!(10),
                    dec!(1),
                    Side::Buy,
                )
            })
            .collect()
    }

    fn buy(
        submitted_ms: i64,
        filled_ms: i64,
        quantity: Decimal,
        avg_price: Decimal,
    ) -> ObservedFill {
        ObservedFill {
            submitted_ms,
            filled_ms,
            side: Side::Buy,
            quantity,
            avg_price,
        }
    }

    #[test]
    fn an_order_fills_on_the_first_print_after_its_latency() {
        let order = buy(1_000, 1_100, dec!(1), dec!(100.2));

        let sim = simulate_market_order(&parity_venue(100), &rising_tape(), &order).unwrap();

        // 100 ms after 1_000 is the print at 1_100, priced 100.2.
        assert_eq!(sim.avg_price, dec!(100.2));
        assert_eq!(sim.filled_ms, 1_100);
        assert_eq!(sim.quantity, dec!(1));
    }

    #[test]
    fn a_large_order_walks_several_prints_at_their_volume_weighted_price() {
        let order = buy(1_000, 1_100, dec!(2), dec!(100.25));

        let sim = simulate_market_order(&parity_venue(100), &rising_tape(), &order).unwrap();

        // Prints at 1_100 and 1_150: (100.2 + 100.3) / 2.
        assert_eq!(sim.avg_price, dec!(100.25));
        assert_eq!(sim.filled_ms, 1_150);
    }

    #[test]
    fn an_order_the_tape_cannot_fill_has_no_simulated_fill() {
        let order = buy(1_000, 1_100, dec!(1000), dec!(100));

        assert!(simulate_market_order(&parity_venue(0), &rising_tape(), &order).is_none());
    }

    #[test]
    fn the_adverse_error_is_positive_when_the_simulation_is_worse_than_live() {
        // Live bought at 100.0; the simulation paid 100.2 (20 bps worse).
        let order = buy(1_000, 1_000, dec!(1), dec!(100));

        let comparison = compare_fills(&rising_tape(), &[order], 100, parity_venue);

        assert_eq!(
            comparison[0].adverse_error_bps.unwrap().round_dp(2),
            dec!(20)
        );
        assert_eq!(comparison[0].time_error_ms, Some(100));
    }

    #[test]
    fn a_sell_is_pessimistic_when_the_simulation_sold_lower() {
        let tape: Vec<Trade> = (0..10)
            .map(|i| {
                print(
                    1_000 + i * 50,
                    dec!(100) - Decimal::from(i) / dec!(10),
                    dec!(1),
                    Side::Sell,
                )
            })
            .collect();
        let order = ObservedFill {
            submitted_ms: 1_000,
            filled_ms: 1_000,
            side: Side::Sell,
            quantity: dec!(1),
            avg_price: dec!(100),
        };

        let comparison = compare_fills(&tape, &[order], 100, parity_venue);

        // Sold at 99.8 against a live 100.0.
        assert_eq!(
            comparison[0].adverse_error_bps.unwrap().round_dp(2),
            dec!(20)
        );
    }

    #[test]
    fn the_sweep_prefers_the_latency_that_reproduces_the_live_prices() {
        // Live fills were generated with a true latency of 150 ms.
        let tape = rising_tape();
        let fills = vec![
            buy(1_000, 1_150, dec!(1), dec!(100.3)),
            buy(1_500, 1_650, dec!(1), dec!(101.3)),
        ];

        let scores = sweep_latency(&tape, &fills, [0, 50, 100, 150, 200, 300], parity_venue);
        let best = best_latency(&scores).unwrap();

        assert_eq!(best.latency_ms, 150);
        assert_eq!(best.mean_abs_error_bps, Decimal::ZERO);
        assert_eq!(best.matched, 2);
        let zero = scores.iter().find(|s| s.latency_ms == 0).unwrap();
        assert!(
            zero.mean_bias_bps < Decimal::ZERO,
            "too little latency is optimistic"
        );
    }

    #[test]
    fn nothing_is_recommended_when_no_fill_could_be_simulated() {
        let fills = vec![buy(1_000, 1_100, dec!(1000), dec!(100))];

        let scores = sweep_latency(&rising_tape(), &fills, [0, 100], parity_venue);

        assert!(best_latency(&scores).is_none());
    }

    #[test]
    fn observed_fills_round_trip_through_json_lines() {
        let fill = buy(1, 2, dec!(0.5), dec!(100.25));

        let json = serde_json::to_string(&fill).unwrap();

        assert_eq!(serde_json::from_str::<ObservedFill>(&json).unwrap(), fill);
    }

    #[test]
    fn jsonl_fills_parse_and_skip_blank_lines() {
        let a = buy(1, 2, dec!(0.5), dec!(100));
        let b = buy(3, 4, dec!(0.25), dec!(101));
        let text = format!(
            "{}\n\n{}\n",
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );

        assert_eq!(parse_fills_jsonl(&text).unwrap(), vec![a, b]);
        assert!(parse_fills_jsonl("not json").is_err());
    }

    #[test]
    fn the_tape_window_surrounds_every_fill() {
        let fills = vec![
            buy(5_000, 5_100, dec!(1), dec!(1)),
            buy(9_000, 9_400, dec!(1), dec!(1)),
        ];

        assert_eq!(tape_window(&fills), Some((4_000, 69_400)));
        assert_eq!(tape_window(&[]), None);
    }

    #[test]
    fn a_discarded_submission_is_never_paired_with_a_later_fill() {
        let path = std::env::temp_dir().join(format!("atsnt-discard-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let recorder = FillRecorder::new(&path);
        recorder.on_entry_submitted(1_000);

        // The next order's send time is unknown: nothing may be recorded for it.
        recorder.discard_pending();
        let fill = recorder
            .on_entry_filled(Side::Buy, dec!(1), dec!(100), 1_200)
            .unwrap();

        assert_eq!(fill, None);
        assert!(!path.exists());
    }

    #[test]
    fn the_recorder_pairs_a_submission_with_its_fill_and_appends_json_lines() {
        let path = std::env::temp_dir().join(format!("atsnt-fills-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let recorder = FillRecorder::new(&path);

        // A fill with no noted submission records nothing.
        assert_eq!(
            recorder
                .on_entry_filled(Side::Buy, dec!(1), dec!(100), 5)
                .unwrap(),
            None
        );
        recorder.on_entry_submitted(1_000);
        let first = recorder
            .on_entry_filled(Side::Buy, dec!(0.5), dec!(100.25), 1_120)
            .unwrap()
            .unwrap();
        recorder.on_entry_submitted(2_000);
        recorder
            .on_entry_filled(Side::Sell, dec!(0.25), dec!(99.5), 2_090)
            .unwrap();

        let fills = parse_fills_jsonl(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0], first);
        assert_eq!(fills[0].submitted_ms, 1_000);
        assert_eq!(fills[1].side, Side::Sell);
    }
}
