//! Quantitative analytics, information-driven sampling, and statistical filters.
//!
//! Implements mathematical and feature-engineering primitives following
//! Marcos López de Prado's *Advances in Financial Machine Learning* (AFML):
//! - Information-driven sampling (Dollar Bars) in [`sampling`].
//! - Structural break & event-driven quality control filters (Symmetric CUSUM) in [`filters`].
//! - Microstructural rolling statistics (Rolling Z-Score) in [`stats`].
//! - Time, tick, volume and range bar feeds in [`feeds`], with their threshold calibration in
//!   [`calibration`].
//! - Per-feed technical indicators (RSI, ATR, ATR ratio, ALMA slope, VWAP deviation) in
//!   [`indicators`].

pub mod calibration;
pub mod feeds;
pub mod filters;
pub mod indicators;
pub mod sampling;
pub mod stats;

pub use calibration::{calibrate, FeedThresholds};
pub use feeds::{
    BarAggregator, FeedBar, RangeBarAggregator, TickBarAggregator, TimeBarAggregator,
    VolumeBarAggregator,
};
pub use filters::{CusumEvent, CusumFilter};
pub use indicators::{
    AlmaSlope, AtrRatio, BarInput, FeedIndicators, IndicatorSnapshot, VwapDeviation, WilderAtr,
    WilderRsi,
};
pub use sampling::DollarBarAggregator;
pub use stats::{RollingZScore, ZScoreResult};
