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
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
