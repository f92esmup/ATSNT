//! ATSNT Domain Engine - Pure financial accounting models and quantitative analytics.
//!
//! Enforces zero I/O, zero network dependencies, and 100% deterministic arithmetic.

// AGENTS.md: no unwrap or expect in production code; test builds keep them.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod accounting;
pub mod analytics;
pub mod decision;
pub mod error;
pub mod futures;
pub mod gate;
pub mod market;
pub mod metrics;
pub mod order;
pub mod position;
pub mod risk;
pub mod trade;

// Re-exports of core domain and analytical types
pub use analytics::{
    AlmaSlope, AtrRatio, BarAggregator, BarInput, FeedBar, RangeBarAggregator, RollingZScore,
    TickBarAggregator, TimeBarAggregator, VolumeBarAggregator, VwapDeviation, WilderAtr, WilderRsi,
    ZScoreResult,
};
pub use decision::{MarketEvent, ProtectiveExits};
pub use error::DomainError;
pub use gate::{EntryGate, EntryRefusal, MarginRule};
pub use market::MarketType;
pub use order::{Order, OrderIntent, OrderStatus, OrderType, TimeInForce};
pub use position::{Position, PositionSide};
pub use risk::{RiskError, RiskPolicy};
pub use trade::{Side, Trade};
