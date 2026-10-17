//! Run-centric data contract shared by every execution mode (backtest, HPO,
//! Monte Carlo, paper, and live).
//!
//! A run is the unit of analytics: everything persisted (metrics, trades,
//! equity events) hangs off a [`RunId`], and every run records the exact
//! [`RunSpec`] it executed so results can always be joined back to the
//! strategy parameters and friction schedule that produced them.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use strategies::MarketType;

use crate::artifact::SamplingConfig;
use crate::config::BacktestConfig;

/// Complete, strategy-agnostic description of what a run executes.
///
/// Strategy hyperparameters stay generic (`C`) so new strategies plug in
/// without schema changes; analytics sinks persist them as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSpec<C> {
    /// Canonical strategy identifier (e.g. "dollar_bars_cusum").
    pub strategy_id: String,
    /// Target trading symbol in uppercase (e.g. "BTCUSDT").
    pub symbol: String,
    /// Market venue model (e.g. UsdMPerpetual, Spot).
    pub market_type: MarketType,
    /// Data sampling configuration defining the discrete information clock.
    pub sampling: SamplingConfig,
    /// Strategy hyperparameters.
    pub strategy_config: C,
    /// Account risk settings and friction schedule (fees, slippage, sizing).
    pub execution_context: BacktestConfig,
}

impl<C> RunSpec<C> {
    /// Same market, sampling, and friction with different strategy parameters
    /// (e.g. an HPO trial derived from its sweep's spec).
    pub fn with_config<D>(&self, strategy_config: D) -> RunSpec<D> {
        RunSpec {
            strategy_id: self.strategy_id.clone(),
            symbol: self.symbol.clone(),
            market_type: self.market_type,
            sampling: self.sampling,
            strategy_config,
            execution_context: self.execution_context,
        }
    }
}

impl<C: Serialize> RunSpec<C> {
    /// Strategy hyperparameters as a JSON value for schema-free persistence.
    pub fn params_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(&self.strategy_config)
    }
}

/// Execution mode that produced a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunType {
    /// Single historical simulation.
    Backtest,
    /// Hyperparameter optimization sweep (parent of its trials).
    Hpo,
    /// One evaluated candidate inside an HPO sweep.
    HpoTrial,
    /// Monte Carlo stress test of a backtest's trade sequence.
    MonteCarlo,
    /// Paper trading session against live market data.
    Paper,
    /// Live trading session with real orders.
    Live,
}

impl RunType {
    /// Stable label used in persisted rows.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Backtest => "backtest",
            Self::Hpo => "hpo",
            Self::HpoTrial => "hpo_trial",
            Self::MonteCarlo => "monte_carlo",
            Self::Paper => "paper",
            Self::Live => "live",
        }
    }
}

/// How the orders of a run were filled, recorded with every run so results simulated at
/// different fidelity are never mixed up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillModel {
    /// Bar-level fills (`BacktestEngine`): the optimizer's fast loop.
    Bar,
    /// Tick-level fills (`SimulatedVenue`): latency and tape-walk market orders.
    Tick,
    /// Fills reported by the real exchange.
    Exchange,
}

impl FillModel {
    /// Stable label used in persisted rows.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Tick => "tick",
            Self::Exchange => "exchange",
        }
    }
}

impl RunType {
    /// Fill model a run of this type uses unless it says otherwise.
    pub fn default_fill_model(&self) -> FillModel {
        match self {
            Self::Backtest | Self::Hpo | Self::HpoTrial | Self::MonteCarlo => FillModel::Bar,
            Self::Paper => FillModel::Tick,
            Self::Live => FillModel::Exchange,
        }
    }
}

/// Why a typed run identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidRunId {
    /// Not 36 characters of lowercase hex groups separated by hyphens.
    #[error("`{0}` is not a run id (expected a lowercase UUID v7 such as 0198b3c0-7a4e-7c21-8f3d-5b2a9c6e1d40)")]
    Shape(String),
    /// Well formed but not a UUID v7 with the RFC 4122 variant.
    #[error("`{0}` is not a UUID v7 run id")]
    NotV7(String),
}

/// Globally unique, time-ordered run identifier in UUID v7 text form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// Generates a new identifier from the system clock and a random tail.
    pub fn generate() -> Self {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        Self::from_parts(unix_ms, rand::random::<u128>())
    }

    /// Builds a UUID v7 from a millisecond timestamp and random bits
    /// (RFC 9562 §5.7): 48-bit timestamp, version 7, RFC 4122 variant.
    pub fn from_parts(unix_ms: u64, random: u128) -> Self {
        let rand_a = (random >> 64) as u16 & 0x0FFF;
        let rand_b = random as u64 & 0x3FFF_FFFF_FFFF_FFFF;
        let ts = unix_ms & 0xFFFF_FFFF_FFFF;
        Self(format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            ts >> 16,
            ts & 0xFFFF,
            0x7000 | rand_a,
            0x8000 | (rand_b >> 48),
            rand_b & 0xFFFF_FFFF_FFFF,
        ))
    }

    /// Text form of the identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses the canonical text form (lowercase UUID v7, as printed by [`RunId::generate`]).
    ///
    /// Used for identifiers typed on a command line, where a typo must be rejected before it is
    /// recorded as a reference to a run that does not exist.
    pub fn parse(text: &str) -> Result<Self, InvalidRunId> {
        let bytes = text.as_bytes();
        let shape_ok = bytes.len() == 36
            && bytes.iter().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => *b == b'-',
                _ => matches!(b, b'0'..=b'9' | b'a'..=b'f'),
            });
        if !shape_ok {
            return Err(InvalidRunId::Shape(text.to_string()));
        }
        // Invariant: `shape_ok` proved a 36-byte string, so indices 14 and 19 exist.
        if bytes[14] != b'7' || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
            return Err(InvalidRunId::NotV7(text.to_string()));
        }
        Ok(Self(text.to_string()))
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
