// AGENTS.md: no unwrap or expect in production code; test builds keep them.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod analytics;
pub mod binance_csv;
pub mod binance_fetcher;
pub mod binance_funding;
pub mod entropy;
pub mod error;
pub mod fetch_plan;
pub mod simulated_venue;
pub mod storage;
#[cfg(test)]
pub(crate) mod test_http;
pub mod wall_time;

pub use analytics::{InMemoryRunRecorder, NullRunRecorder, RecordedRuns};
pub use binance_csv::BinanceCsvReader;
pub use binance_fetcher::{BinanceDataFetcher, FetchParams, FetchSummary};
pub use binance_funding::{funding_schedule_from_history, parse_funding_history};
pub use entropy::OsEntropy;
pub use error::AdapterError;
pub use fetch_plan::{is_not_archived, plan_fetch_tasks, run_bounded, FetchTask};
pub use simulated_venue::{SimulatedVenue, SimulatedVenueConfig, SimulatedVenueError};
pub use storage::jsonl_file::JsonLinesFile;
pub use storage::sqlite_runs::SqliteRunRecorder;
pub use storage::tape_parquet::{TapeReader, TapeWriter};
pub use storage::text_file::read_text;
