//! Quantitative analytics, information-driven sampling, and statistical filters.
//!
//! Implements mathematical and feature-engineering primitives following
//! Marcos López de Prado's *Advances in Financial Machine Learning* (AFML):
//! - Information-driven sampling (Dollar Bars) in [`sampling`].
//! - Structural break & event-driven quality control filters (Symmetric CUSUM) in [`filters`].
//! - Microstructural rolling statistics (Rolling Z-Score) in [`stats`].

pub mod filters;
pub mod sampling;
pub mod stats;

pub use filters::{CusumEvent, CusumFilter};
pub use sampling::DollarBarAggregator;
pub use stats::{RollingZScore, ZScoreResult};
