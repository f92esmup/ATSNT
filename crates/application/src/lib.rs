//! The application core: the trading session and what it needs to run on any venue.
//!
//! This crate depends only on `domain`, `ports` and `strategies`. It holds the session state
//! machine, the driver that feeds it, the exit lifecycle, the position accounting shared by
//! every simulated fill, the tick-level simulation and the simulation-versus-live parity
//! measure. It names no adapter, reads no clock and no file: composition crates build the
//! adapters and pass them in as ports.

// AGENTS.md: no unwrap or expect in production code; test builds keep them.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod alert_throttle;
pub mod config;
pub mod driver;
pub mod error;
pub mod execution_core;
pub mod live_exits;
pub mod operator_alerts;
pub mod parity;
pub mod price_event;
pub mod session;
pub mod tick_simulation;

pub use domain::metrics;
pub use error::BacktestError;
