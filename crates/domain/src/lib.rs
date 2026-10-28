//! ATSNT Domain Engine - Pure financial accounting models and quantitative analytics.
//!
//! Enforces zero I/O, zero network dependencies, and 100% deterministic arithmetic.

pub mod analytics;
pub mod bar;
pub mod decision;
pub mod error;
pub mod futures;
pub mod order;
pub mod position;
pub mod risk;
pub mod trade;
pub mod universe;

// Backward-compatibility module aliases
pub use analytics::filters as cusum;
pub use analytics::sampling as aggregator;
pub use analytics::stats;

// Re-exports of core domain and analytical types
pub use analytics::{
    calibrate, calibrate_empirical_priors, AlmaSlope, AtrRatio, BarAggregator, BarInput,
    CusumEvent, CusumFilter, DollarBarAggregator, EmpiricalPriors, FeedBar, FeedIndicators,
    FeedThresholds, IndicatorSnapshot, RangeBarAggregator, RollingZScore, TickBarAggregator,
    TimeBarAggregator, VolumeBarAggregator, VwapDeviation, WilderAtr, WilderRsi, ZScoreResult,
};
pub use bar::DollarBar;
pub use decision::{
    Decision, InputKind, MarketEvent, ProtectiveExits, TargetPosition, MAX_TARGET_LEVEL,
};
pub use error::DomainError;
pub use order::{Order, OrderIntent, OrderStatus, OrderType, TimeInForce};
pub use position::{Position, PositionSide};
pub use risk::{PortfolioRiskPolicy, RiskError, RiskPolicy, TargetSizing};
pub use trade::{Side, Trade};
pub use universe::{AssetRankingScore, UniverseSelectionPolicy};
