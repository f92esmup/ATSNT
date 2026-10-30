//! Pure accounting rules of an account: the daily equity watermark, the monthly kill-switch, the
//! commissions a live position paid and the equity path of a trade. Decimal arithmetic only: no
//! I/O, no clock and no runtime, so every rule is deterministic and unit-testable.

pub mod daily_watermark;
pub mod excursion;
pub mod fee_ledger;
pub mod monthly_guard;

pub use daily_watermark::DailyWatermark;
pub use excursion::{ExcursionTracker, TradeExcursion};
pub use fee_ledger::FeeLedger;
pub use monthly_guard::{marked_equity, utc_month_key, MonthlyGuard};
