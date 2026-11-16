//! Simulation-versus-live parity: replay the market orders a live run actually sent on the
//! tape that was trading at that moment, and measure how far the simulated fills sit from the
//! real ones.
//!
//! The comparison replays on the simulated venue of the composition, the one the
//! tick-level simulation and the paper session use, so the latency it recommends is the one to
//! configure there. Inputs are the fills of a real run, as JSON lines, and a tape of the same
//! market (for production, the `aggTrades` archive of that day).
//!
//! The submit time is the local wall clock and the fill time is the exchange's, so clock skew
//! shifts the time error one-to-one; the price error is what the latency sweep is scored on.

use domain::{Side, Trade};
use ports::{FillLog, TapeVenue};
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

/// Parses the JSON-lines fills of a real run; blank lines are skipped.
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

/// Appends the entry fills of a live run to a [`FillLog`], as JSON, for [`compare_fills`].
///
/// The caller reports the local wall-clock time an entry order is sent and, once the position
/// opened, its average fill; the recorder pairs them.
pub struct FillRecorder<L: FillLog> {
    log: L,
    submitted_ms: std::sync::Mutex<Option<i64>>,
}

/// Why a fill could not be recorded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FillRecordError {
    /// The fill could not be turned into JSON.
    #[error("cannot serialize the fill: {0}")]
    Serialize(String),
    /// The log refused the record.
    #[error("cannot append the fill: {0}")]
    Log(String),
}

impl<L: FillLog> FillRecorder<L> {
    /// A recorder that appends to `log`.
    pub fn new(log: L) -> Self {
        Self {
            log,
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
    ) -> Result<Option<ObservedFill>, FillRecordError> {
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
        let record = serde_json::to_string(&fill)
            .map_err(|error| FillRecordError::Serialize(error.to_string()))?;
        self.log
            .append(&record)
            .map_err(|error| FillRecordError::Log(error.to_string()))?;
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
/// venue of each replay at a latency; the composition is the one to pass.
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
