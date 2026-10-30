//! Contracts between the trading core and the outside world (hexagonal ports).
//!
//! This crate depends only on `domain`. Adapters implement these traits (exchanges, files, the
//! simulator, messaging services) and the application crate wires them together, so the core
//! never names a concrete exchange, storage format or transport.

pub mod alerts;
pub mod clock;
pub mod execution;
pub mod exits;
pub mod market_data;
pub mod run_recorder;

pub use alerts::AlertNotifier;
pub use clock::Clock;
pub use execution::{EntryAck, ExecutionReport, ExecutionVenue, OrderRequest};
pub use exits::ExitGateway;
pub use market_data::{AsyncMarketDataStream, MarketDataStream};
pub use run_recorder::{
    RecorderError, RunMetricsRow, RunRecorder, RunRow, RunTradeRow, SeriesPointRow,
};
