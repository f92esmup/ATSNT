//! Composition of the concrete adapters for the binaries and the simulated runs.
//!
//! The library names no adapter (see the architecture contract): the binaries take the clock, the
//! recorder and the simulated venue from here and pass them down as ports.

use std::path::Path;

use adapters::{
    NullRunRecorder, SimulatedVenue, SimulatedVenueConfig, SqliteRunRecorder, SystemClock,
};
use domain::futures::FundingSchedule;
use domain::{RiskError, RiskPolicy};
use ports::{
    Clock, RecorderError, RunMetricsRow, RunRecorder, RunRow, RunTradeRow, SeriesPointRow,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::Strategy;

use crate::tick_simulation::{TickSimulation, TickSimulationConfig, TickSimulationError};

/// Current wall-clock time in Unix milliseconds, for the binaries that stamp their runs.
pub fn now_unix_ms() -> i64 {
    SystemClock.now_ms()
}

/// The clock of the binaries: the machine's wall clock.
pub fn system_clock() -> SystemClock {
    SystemClock
}

/// The recorder of runs whose analytics are not persisted: the paper and live binaries keep
/// nothing, so their memory does not grow with the run.
pub fn disabled_recorder() -> NullRunRecorder {
    NullRunRecorder
}

/// The recorder of a binary's `--record` option: the SQLite store at `path`, or nothing when the
/// option is absent.
pub fn recorder(path: Option<&Path>) -> Result<Recorder, RecorderError> {
    match path {
        Some(path) => Ok(Recorder::Sqlite(SqliteRunRecorder::open(path)?)),
        None => Ok(Recorder::Null(NullRunRecorder)),
    }
}

/// A run recorder chosen at start-up: the SQLite store, or nothing.
pub enum Recorder {
    /// Keeps nothing.
    Null(NullRunRecorder),
    /// Writes to the SQLite store.
    Sqlite(SqliteRunRecorder),
}

impl RunRecorder for Recorder {
    async fn record_runs(&self, rows: &[RunRow]) -> Result<(), RecorderError> {
        match self {
            Self::Null(recorder) => recorder.record_runs(rows).await,
            Self::Sqlite(recorder) => recorder.record_runs(rows).await,
        }
    }

    async fn record_metrics(&self, rows: &[RunMetricsRow]) -> Result<(), RecorderError> {
        match self {
            Self::Null(recorder) => recorder.record_metrics(rows).await,
            Self::Sqlite(recorder) => recorder.record_metrics(rows).await,
        }
    }

    async fn record_trades(&self, rows: &[RunTradeRow]) -> Result<(), RecorderError> {
        match self {
            Self::Null(recorder) => recorder.record_trades(rows).await,
            Self::Sqlite(recorder) => recorder.record_trades(rows).await,
        }
    }

    async fn record_series(&self, rows: &[SeriesPointRow]) -> Result<(), RecorderError> {
        match self {
            Self::Null(recorder) => recorder.record_series(rows).await,
            Self::Sqlite(recorder) => recorder.record_series(rows).await,
        }
    }
}

/// A tick-level simulation on the simulated exchange.
pub type SimulatedTickSimulation<S> = TickSimulation<S, SimulatedVenue>;

/// A tick-level simulation on the simulated exchange, configured from one [`TickSimulationConfig`]
/// so that the venue and the session agree on balance, fees and risk.
pub fn tick_simulation<S: Strategy>(
    config: TickSimulationConfig,
    strategy: S,
) -> Result<SimulatedTickSimulation<S>, TickSimulationError> {
    let venue = simulated_venue(&config)?;
    TickSimulation::new(config, strategy, venue)
}

/// The simulated exchange of a tick-level run: the fees, latency, funding and risk of `config`.
pub fn simulated_venue(config: &TickSimulationConfig) -> Result<SimulatedVenue, RiskError> {
    let backtest = &config.backtest;
    let maker_fee_pct = if backtest.resting_maker_exits {
        Some(backtest.maker_fee_pct)
    } else {
        None
    };
    Ok(SimulatedVenue::new(SimulatedVenueConfig {
        initial_equity: backtest.initial_capital,
        taker_fee_pct: backtest.taker_fee_pct,
        maker_fee_pct,
        latency_ms: config.latency_ms,
        risk_per_trade_pct: backtest.risk_per_trade_pct,
        risk_policy: backtest.risk_policy()?,
        funding: config.funding.clone(),
        maintenance_margin_rate: backtest.maintenance_margin_rate,
    }))
}

/// The venue a parity replay runs on at `latency_ms`: a balance no replay can exhaust, no fees and
/// no funding, so a replay differs from the observed fills by latency and the tape alone.
pub fn parity_venue(latency_ms: i64) -> SimulatedVenue {
    SimulatedVenue::new(SimulatedVenueConfig {
        initial_equity: dec!(1_000_000_000),
        taker_fee_pct: Decimal::ZERO,
        maker_fee_pct: None,
        latency_ms,
        risk_per_trade_pct: dec!(0.01),
        risk_policy: RiskPolicy::default(),
        funding: FundingSchedule::constant(Decimal::ZERO),
        maintenance_margin_rate: Decimal::ZERO,
    })
}
