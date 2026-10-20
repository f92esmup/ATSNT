//! Contracts between the trading core and the outside world (hexagonal ports).
//!
//! This crate depends only on `domain`. Adapters implement these traits (Binance, files, the
//! simulator, Telegram) and the application crate wires them together, so the core never
//! names a concrete exchange or transport.

pub mod alerts;
pub mod execution;
pub mod exits;
pub mod market_data;

pub use alerts::AlertNotifier;
pub use execution::{EntryAck, ExecutionReport, ExecutionVenue, OrderRequest};
pub use exits::ExitGateway;
pub use market_data::{AsyncMarketDataStream, MarketDataStream};
