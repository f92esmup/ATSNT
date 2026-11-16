//! Storage of market tapes on disk: the trade archives that backtests replay and parity checks
//! compare against.

pub mod jsonl_file;
pub mod sqlite_runs;
pub mod tape_parquet;
pub mod text_file;
