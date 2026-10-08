//! ATSNT Domain Engine - Pure financial accounting models and quantitative analytics.
//!
//! Enforces zero I/O, zero network dependencies, and 100% deterministic arithmetic.

pub mod analytics;
pub mod bar;
pub mod error;
pub mod order;
pub mod position;
pub mod risk;
pub mod trade;

// Backward-compatibility module aliases
pub use analytics::filters as cusum;
pub use analytics::sampling as aggregator;
pub use analytics::stats;

// Re-exports of core domain and analytical types
pub use analytics::{CusumEvent, CusumFilter, DollarBarAggregator, RollingZScore, ZScoreResult};
pub use bar::DollarBar;
pub use error::DomainError;
pub use order::{Order, OrderIntent, OrderStatus, OrderType, TimeInForce};
pub use position::{Position, PositionSide};
pub use risk::{RiskError, RiskPolicy};
pub use trade::{Side, Trade};
