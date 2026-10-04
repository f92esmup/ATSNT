pub mod aggregator;
pub mod bar;
pub mod error;
pub mod trade;

pub use aggregator::DollarBarAggregator;
pub use bar::DollarBar;
pub use error::DomainError;
pub use trade::{Side, Trade};
