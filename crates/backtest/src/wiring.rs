//! Composition of the concrete adapters for the binaries and the simulated runs.
//!
//! The library names no adapter (see the architecture contract): the binaries take the clock, the
//! recorder and the simulated venue from here and pass them down as ports.

use std::path::Path;

use adapters::{
    read_text, NullRunRecorder, OsEntropy, SimulatedVenue, SimulatedVenueConfig, SqliteRunRecorder,
    SystemClock,
};
use domain::futures::FundingSchedule;
use domain::{RiskError, RiskPolicy};
use ports::{
    Clock, ProgressSink, RecorderError, RunMetricsRow, RunRecorder, RunRow, RunTradeRow,
    SeriesPointRow,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::de::DeserializeOwned;
use strategies::Strategy;

use crate::artifact::{parse_artifact_shared_config, parse_strategy_config, LoadedStrategyConfig};
use crate::error::{ConfigText, LoadError};
use crate::registry::{build_strategy, strategy_id_of, LoadedStrategy};
use crate::tick_simulation::{TickSimulation, TickSimulationConfig, TickSimulationError};

/// Current wall-clock time in Unix milliseconds, for the binaries that stamp their runs.
pub fn now_unix_ms() -> i64 {
    SystemClock.now_ms()
}

/// The clock of the binaries: the machine's wall clock.
pub fn system_clock() -> SystemClock {
    SystemClock
}

/// The random source of the binaries: the operating system's generator.
pub fn entropy() -> OsEntropy {
    OsEntropy
}

/// A terminal progress bar for the candidates of a hyper-parameter search. Each start opens a
/// new bar, so one sink can report several searches in turn.
#[derive(Default)]
pub struct TerminalProgress {
    bar: std::sync::Mutex<Option<indicatif::ProgressBar>>,
}

impl TerminalProgress {
    fn with_bar(&self, act: impl FnOnce(&mut Option<indicatif::ProgressBar>)) {
        act(&mut self
            .bar
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()));
    }
}

impl ProgressSink for TerminalProgress {
    fn start(&self, total: u64) {
        let bar = indicatif::ProgressBar::new(total);
        bar.set_style(
            indicatif::ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) | ETA: {eta_precise} | {msg}")
                .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
                .progress_chars("#>-"),
        );
        bar.set_message("Evaluating HPO parameter space");
        self.with_bar(|slot| *slot = Some(bar));
    }

    fn advance(&self) {
        self.with_bar(|slot| {
            if let Some(bar) = slot {
                bar.inc(1);
            }
        });
    }

    fn finish(&self) {
        self.with_bar(|slot| {
            if let Some(bar) = slot.take() {
                bar.finish_with_message("HPO parameter evaluation complete");
            }
        });
    }
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

/// The text of the configuration file at `path`.
fn config_file(path: &Path) -> Result<(String, String), LoadError> {
    let origin = path.display().to_string();
    let content = read_text(path).map_err(|error| LoadError::Read {
        origin: origin.clone(),
        message: error.to_string(),
    })?;
    Ok((origin, content))
}

/// Runs `parse` on the text of the optional configuration file at `path`.
fn with_config_file<T>(
    path: Option<&Path>,
    parse: impl FnOnce(Option<ConfigText<'_>>) -> Result<T, LoadError>,
) -> Result<T, LoadError> {
    match path {
        Some(path) => {
            let (origin, content) = config_file(path)?;
            parse(Some(ConfigText {
                origin: &origin,
                content: &content,
            }))
        }
        None => parse(None),
    }
}

/// [`strategy_id_of`] for a configuration file.
pub fn resolve_strategy_id(
    requested: Option<&str>,
    config_path: Option<&Path>,
) -> Result<String, LoadError> {
    with_config_file(config_path, |config| strategy_id_of(requested, config))
}

/// Builds the strategy registered under `name` from the configuration file at `config_path`, or
/// from its default hyperparameters when no file is given (see [`build_strategy`]).
pub fn load_strategy(name: &str, config_path: Option<&Path>) -> Result<LoadedStrategy, LoadError> {
    load_strategy_with_shared(name, config_path, None)
}

/// [`load_strategy`] with an optional shared-configuration file (`--shared-config`).
pub fn load_strategy_with_shared(
    name: &str,
    config_path: Option<&Path>,
    shared_path: Option<&Path>,
) -> Result<LoadedStrategy, LoadError> {
    with_config_file(config_path, |config| {
        with_config_file(shared_path, |shared| build_strategy(name, config, shared))
    })
}

/// [`parse_strategy_config`] for a configuration file.
pub fn load_strategy_config<C: DeserializeOwned>(
    path: &Path,
) -> Result<LoadedStrategyConfig<C>, LoadError> {
    with_config_file(Some(path), |config| match config {
        Some(text) => parse_strategy_config(text),
        None => Err(LoadError::Read {
            origin: path.display().to_string(),
            message: "no configuration".to_string(),
        }),
    })
}

/// [`parse_artifact_shared_config`] for an artifact file.
pub fn load_artifact_shared_config(path: &Path) -> Result<Option<serde_json::Value>, LoadError> {
    with_config_file(Some(path), |config| match config {
        Some(text) => parse_artifact_shared_config(text),
        None => Ok(None),
    })
}
