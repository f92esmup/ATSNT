//! Quantitative analytics: bar feeds and indicators.
//!
//! Implements mathematical and feature-engineering primitives following
//! Marcos López de Prado's *Advances in Financial Machine Learning* (AFML):
//! - Microstructural rolling statistics (Rolling Z-Score) in [`stats`].
//! - Time, tick, volume and range bar feeds in [`feeds`].
//! - Technical indicators (RSI, ATR, ATR ratio, ALMA slope, VWAP deviation) in
//!   [`indicators`].

pub mod feeds;
pub mod indicators;
pub mod stats;

pub use feeds::{
    BarAggregator, FeedBar, RangeBarAggregator, TickBarAggregator, TimeBarAggregator,
    VolumeBarAggregator,
};
pub use indicators::{AlmaSlope, AtrRatio, BarInput, VwapDeviation, WilderAtr, WilderRsi};
pub use stats::{RollingZScore, ZScoreResult};
