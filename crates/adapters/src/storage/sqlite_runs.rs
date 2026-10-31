//! SQLite store of run analytics: the local registry of the runs the binaries execute.
//!
//! The store keeps the four row kinds of the [`RunRecorder`] port in four tables. Rows are
//! append-only, and the latest `runs` row of a `run_id` is its current state. Decimals are stored
//! as text with the port's scale of nine fractional digits, so no float ever touches a value.
//!
//! One thread owns the connection. Callers send their rows through a bounded channel and wait for
//! the commit: a full queue applies backpressure, and no lock is held across an `await`.

use std::path::Path;
use std::thread;

use ports::{RecorderError, RunMetricsRow, RunRecorder, RunRow, RunTradeRow, SeriesPointRow};
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, oneshot};

/// Schema version written to `PRAGMA user_version`. A store written by a newer build is refused
/// rather than misread.
pub const SCHEMA_VERSION: i64 = 1;

/// Commands queued for the writer thread before a caller waits.
const QUEUE_CAPACITY: usize = 64;

const SCHEMA_V1: &str = r#"
CREATE TABLE runs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    parent_run_id TEXT,
    baseline_run_id TEXT,
    run_type TEXT NOT NULL,
    status TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    strategy_id TEXT NOT NULL,
    symbol TEXT NOT NULL,
    market_type TEXT NOT NULL,
    sampling TEXT NOT NULL,
    params TEXT NOT NULL,
    execution_context TEXT NOT NULL,
    fill_model TEXT NOT NULL,
    data_source TEXT,
    data_start TEXT,
    data_end TEXT,
    git_sha TEXT
);
CREATE INDEX runs_by_run_id ON runs (run_id);

CREATE TABLE run_metrics (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    run_type TEXT NOT NULL,
    scope TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    total_trades INTEGER,
    winning_trades INTEGER,
    losing_trades INTEGER,
    win_rate TEXT,
    gross_profit TEXT,
    gross_loss TEXT,
    net_profit TEXT,
    total_fees TEXT,
    total_slippage TEXT,
    profit_factor TEXT,
    payoff_ratio TEXT,
    mathematical_expectancy TEXT,
    max_drawdown_amount TEXT,
    max_drawdown_pct TEXT,
    sortino_ratio TEXT,
    fee_drag_pct TEXT,
    avg_holding_seconds TEXT,
    avg_mae_pct TEXT,
    avg_mfe_pct TEXT,
    exit_reason_counts TEXT,
    halted INTEGER,
    rejected_orders TEXT,
    extras TEXT
);
CREATE INDEX run_metrics_by_run_id ON run_metrics (run_id);

CREATE TABLE trades (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    trade_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    run_type TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    symbol TEXT NOT NULL,
    side TEXT NOT NULL,
    entry_timestamp TEXT NOT NULL,
    exit_timestamp TEXT NOT NULL,
    entry_price TEXT NOT NULL,
    exit_price TEXT NOT NULL,
    quantity TEXT NOT NULL,
    gross_pnl TEXT NOT NULL,
    fees_paid TEXT NOT NULL,
    funding_paid TEXT NOT NULL,
    slippage_paid TEXT NOT NULL,
    net_pnl TEXT NOT NULL,
    return_pct TEXT NOT NULL,
    exit_reason TEXT NOT NULL,
    holding_duration_seconds INTEGER NOT NULL,
    entry_equity TEXT NOT NULL,
    trough_equity TEXT NOT NULL,
    trough_timestamp TEXT NOT NULL,
    peak_equity TEXT NOT NULL,
    peak_timestamp TEXT NOT NULL,
    exit_equity TEXT NOT NULL,
    max_drawdown_amount TEXT NOT NULL,
    max_drawdown_pct TEXT NOT NULL
);
CREATE INDEX trades_by_run_id ON trades (run_id);

CREATE TABLE run_series (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    run_type TEXT NOT NULL,
    series TEXT NOT NULL,
    event TEXT NOT NULL,
    ts TEXT,
    x INTEGER,
    value TEXT NOT NULL,
    trade_id TEXT
);
CREATE INDEX run_series_by_run_id ON run_series (run_id);
"#;

const INSERT_RUN: &str = "INSERT INTO runs (run_id, parent_run_id, baseline_run_id, run_type, \
    status, recorded_at, started_at, finished_at, strategy_id, symbol, market_type, sampling, \
    params, execution_context, fill_model, data_source, data_start, data_end, git_sha) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)";

const INSERT_METRICS: &str = "INSERT INTO run_metrics (run_id, run_type, scope, recorded_at, \
    total_trades, winning_trades, losing_trades, win_rate, gross_profit, gross_loss, net_profit, \
    total_fees, total_slippage, profit_factor, payoff_ratio, mathematical_expectancy, \
    max_drawdown_amount, max_drawdown_pct, sortino_ratio, fee_drag_pct, avg_holding_seconds, \
    avg_mae_pct, avg_mfe_pct, exit_reason_counts, halted, rejected_orders, extras) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, \
    ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)";

const INSERT_TRADE: &str = "INSERT INTO trades (trade_id, run_id, run_type, strategy_id, symbol, \
    side, entry_timestamp, exit_timestamp, entry_price, exit_price, quantity, gross_pnl, fees_paid, \
    funding_paid, slippage_paid, net_pnl, return_pct, exit_reason, holding_duration_seconds, \
    entry_equity, trough_equity, trough_timestamp, peak_equity, peak_timestamp, exit_equity, \
    max_drawdown_amount, max_drawdown_pct) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, \
    ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)";

const INSERT_SERIES: &str =
    "INSERT INTO run_series (run_id, run_type, series, event, ts, x, value, \
    trade_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

/// Recorder writing run analytics to a SQLite file or to memory.
#[derive(Debug)]
pub struct SqliteRunRecorder {
    commands: mpsc::Sender<Command>,
}

impl SqliteRunRecorder {
    /// Opens the store at `path`, creating it if needed, and migrates it to the current schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RecorderError> {
        let connection = Connection::open(path).map_err(storage)?;
        Self::from_connection(connection)
    }

    /// A store in memory, for tests and for runs whose rows only need to live as long as the
    /// process.
    pub fn in_memory() -> Result<Self, RecorderError> {
        let connection = Connection::open_in_memory().map_err(storage)?;
        Self::from_connection(connection)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, RecorderError> {
        migrate(&mut connection)?;
        let commands = start_writer(connection)?;
        Ok(Self { commands })
    }

    /// Sends one batch to the writer and waits until it is committed.
    async fn submit(&self, command: impl FnOnce(Reply) -> Command) -> Result<(), RecorderError> {
        let (reply, outcome) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| writer_stopped())?;
        outcome.await.map_err(|_| writer_stopped())?
    }
}

impl RunRecorder for SqliteRunRecorder {
    async fn record_runs(&self, rows: &[RunRow]) -> Result<(), RecorderError> {
        let rows = rows.to_vec();
        self.submit(|reply| Command::Runs(rows, reply)).await
    }

    async fn record_metrics(&self, rows: &[RunMetricsRow]) -> Result<(), RecorderError> {
        let rows = rows.to_vec();
        self.submit(|reply| Command::Metrics(rows, reply)).await
    }

    async fn record_trades(&self, rows: &[RunTradeRow]) -> Result<(), RecorderError> {
        let rows = rows.to_vec();
        self.submit(|reply| Command::Trades(rows, reply)).await
    }

    async fn record_series(&self, rows: &[SeriesPointRow]) -> Result<(), RecorderError> {
        let rows = rows.to_vec();
        self.submit(|reply| Command::Series(rows, reply)).await
    }
}

type Reply = oneshot::Sender<Result<(), RecorderError>>;

/// One batch for the writer thread, with the channel to answer on.
enum Command {
    Runs(Vec<RunRow>, Reply),
    Metrics(Vec<RunMetricsRow>, Reply),
    Trades(Vec<RunTradeRow>, Reply),
    Series(Vec<SeriesPointRow>, Reply),
}

impl Command {
    fn apply(self, connection: &mut Connection) {
        let (outcome, reply) = match self {
            Self::Runs(rows, reply) => (insert_runs(connection, &rows), reply),
            Self::Metrics(rows, reply) => (insert_metrics(connection, &rows), reply),
            Self::Trades(rows, reply) => (insert_trades(connection, &rows), reply),
            Self::Series(rows, reply) => (insert_series(connection, &rows), reply),
        };
        // The caller may have stopped waiting. The batch is committed or rolled back either way.
        let _ = reply.send(outcome.map_err(storage));
    }
}

/// Moves the connection to a thread of its own and returns the channel that feeds it. The thread
/// stops when every sender is dropped.
fn start_writer(mut connection: Connection) -> Result<mpsc::Sender<Command>, RecorderError> {
    let (sender, mut commands) = mpsc::channel::<Command>(QUEUE_CAPACITY);
    thread::Builder::new()
        .name("run-recorder-sqlite".to_string())
        .spawn(move || {
            while let Some(command) = commands.blocking_recv() {
                command.apply(&mut connection);
            }
        })
        .map_err(|error| {
            RecorderError::Storage(format!("cannot start the run recorder thread: {error}"))
        })?;
    Ok(sender)
}

/// Brings the store to [`SCHEMA_VERSION`]: an empty store gets the schema, a current one is
/// accepted, and a newer one is refused.
fn migrate(connection: &mut Connection) -> Result<(), RecorderError> {
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(storage)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(storage)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(storage)?;
    match version {
        0 => {
            let transaction = connection.transaction().map_err(storage)?;
            transaction.execute_batch(SCHEMA_V1).map_err(storage)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        }
        SCHEMA_VERSION => Ok(()),
        newer => Err(RecorderError::Storage(format!(
            "the store is at schema version {newer}; this build reads version {SCHEMA_VERSION}"
        ))),
    }
}

fn insert_runs(connection: &mut Connection, rows: &[RunRow]) -> rusqlite::Result<()> {
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare_cached(INSERT_RUN)?;
        for row in rows {
            statement.execute(params![
                row.run_id,
                row.parent_run_id,
                row.baseline_run_id,
                row.run_type,
                row.status,
                row.recorded_at,
                row.started_at,
                row.finished_at,
                row.strategy_id,
                row.symbol,
                row.market_type,
                row.sampling,
                row.params,
                row.execution_context,
                row.fill_model,
                row.data_source,
                row.data_start,
                row.data_end,
                row.git_sha,
            ])?;
        }
    }
    transaction.commit()
}

fn insert_metrics(connection: &mut Connection, rows: &[RunMetricsRow]) -> rusqlite::Result<()> {
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare_cached(INSERT_METRICS)?;
        for row in rows {
            statement.execute(params![
                row.run_id,
                row.run_type,
                row.scope,
                row.recorded_at,
                row.total_trades,
                row.winning_trades,
                row.losing_trades,
                optional_decimal_text(row.win_rate.as_ref()),
                optional_decimal_text(row.gross_profit.as_ref()),
                optional_decimal_text(row.gross_loss.as_ref()),
                optional_decimal_text(row.net_profit.as_ref()),
                optional_decimal_text(row.total_fees.as_ref()),
                optional_decimal_text(row.total_slippage.as_ref()),
                optional_decimal_text(row.profit_factor.as_ref()),
                optional_decimal_text(row.payoff_ratio.as_ref()),
                optional_decimal_text(row.mathematical_expectancy.as_ref()),
                optional_decimal_text(row.max_drawdown_amount.as_ref()),
                optional_decimal_text(row.max_drawdown_pct.as_ref()),
                optional_decimal_text(row.sortino_ratio.as_ref()),
                optional_decimal_text(row.fee_drag_pct.as_ref()),
                optional_decimal_text(row.avg_holding_seconds.as_ref()),
                optional_decimal_text(row.avg_mae_pct.as_ref()),
                optional_decimal_text(row.avg_mfe_pct.as_ref()),
                row.exit_reason_counts,
                row.halted,
                row.rejected_orders,
                row.extras,
            ])?;
        }
    }
    transaction.commit()
}

fn insert_trades(connection: &mut Connection, rows: &[RunTradeRow]) -> rusqlite::Result<()> {
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare_cached(INSERT_TRADE)?;
        for row in rows {
            statement.execute(params![
                row.trade_id,
                row.run_id,
                row.run_type,
                row.strategy_id,
                row.symbol,
                row.side,
                row.entry_timestamp,
                row.exit_timestamp,
                decimal_text(&row.entry_price),
                decimal_text(&row.exit_price),
                decimal_text(&row.quantity),
                decimal_text(&row.gross_pnl),
                decimal_text(&row.fees_paid),
                decimal_text(&row.funding_paid),
                decimal_text(&row.slippage_paid),
                decimal_text(&row.net_pnl),
                decimal_text(&row.return_pct),
                row.exit_reason,
                row.holding_duration_seconds,
                decimal_text(&row.entry_equity),
                decimal_text(&row.trough_equity),
                row.trough_timestamp,
                decimal_text(&row.peak_equity),
                row.peak_timestamp,
                decimal_text(&row.exit_equity),
                decimal_text(&row.max_drawdown_amount),
                decimal_text(&row.max_drawdown_pct),
            ])?;
        }
    }
    transaction.commit()
}

fn insert_series(connection: &mut Connection, rows: &[SeriesPointRow]) -> rusqlite::Result<()> {
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare_cached(INSERT_SERIES)?;
        for row in rows {
            statement.execute(params![
                row.run_id,
                row.run_type,
                row.series,
                row.event,
                row.ts,
                row.x,
                decimal_text(&row.value),
                row.trade_id,
            ])?;
        }
    }
    transaction.commit()
}

/// The stored form of a decimal: at most nine fractional digits, trailing zeros dropped, as the
/// port's serializers write it.
fn decimal_text(value: &Decimal) -> String {
    value.round_dp(9).normalize().to_string()
}

fn optional_decimal_text(value: Option<&Decimal>) -> Option<String> {
    value.map(decimal_text)
}

fn storage(error: rusqlite::Error) -> RecorderError {
    RecorderError::Storage(error.to_string())
}

fn writer_stopped() -> RecorderError {
    RecorderError::Storage("the run recorder thread stopped".to_string())
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn run_row(run_id: &str) -> RunRow {
        RunRow {
            run_id: run_id.to_string(),
            parent_run_id: None,
            baseline_run_id: None,
            run_type: "backtest".to_string(),
            status: "completed".to_string(),
            recorded_at: "2026-10-31T00:00:00Z".to_string(),
            started_at: "2026-10-31T00:00:00Z".to_string(),
            finished_at: Some("2026-10-31T00:00:01Z".to_string()),
            strategy_id: "swarm_consensus".to_string(),
            symbol: "BTCUSDT".to_string(),
            market_type: "usd_m_perpetual".to_string(),
            sampling: "{}".to_string(),
            params: "{}".to_string(),
            execution_context: "{}".to_string(),
            fill_model: "bar".to_string(),
            data_source: None,
            data_start: None,
            data_end: None,
            git_sha: None,
        }
    }

    fn metrics_row(run_id: &str) -> RunMetricsRow {
        RunMetricsRow {
            run_id: run_id.to_string(),
            run_type: "backtest".to_string(),
            scope: "full".to_string(),
            recorded_at: "2026-10-31T00:00:01Z".to_string(),
            total_trades: Some(3),
            net_profit: Some(dec!(12.5)),
            halted: Some(false),
            ..RunMetricsRow::default()
        }
    }

    fn trade_row(run_id: &str, number: usize) -> RunTradeRow {
        RunTradeRow {
            trade_id: format!("{run_id}-{number}"),
            run_id: run_id.to_string(),
            run_type: "backtest".to_string(),
            strategy_id: "swarm_consensus".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: "Long".to_string(),
            entry_timestamp: "2026-10-31T00:00:02Z".to_string(),
            exit_timestamp: "2026-10-31T00:00:03Z".to_string(),
            entry_price: dec!(65000.1200),
            exit_price: dec!(65100),
            quantity: dec!(0.000000001),
            gross_pnl: dec!(1),
            fees_paid: dec!(0.5),
            funding_paid: dec!(0),
            slippage_paid: dec!(0),
            net_pnl: dec!(0.5),
            return_pct: dec!(0.001),
            exit_reason: "TakeProfit".to_string(),
            holding_duration_seconds: 1,
            entry_equity: dec!(5000),
            trough_equity: dec!(4990),
            trough_timestamp: "2026-10-31T00:00:02Z".to_string(),
            peak_equity: dec!(5001),
            peak_timestamp: "2026-10-31T00:00:03Z".to_string(),
            exit_equity: dec!(5000.5),
            max_drawdown_amount: dec!(10),
            max_drawdown_pct: dec!(0.002),
        }
    }

    fn series_row(run_id: &str) -> SeriesPointRow {
        SeriesPointRow {
            run_id: run_id.to_string(),
            run_type: "backtest".to_string(),
            series: "equity".to_string(),
            event: "exit".to_string(),
            ts: Some("2026-10-31T00:00:03Z".to_string()),
            x: Some(1),
            value: dec!(5000.5),
            trade_id: None,
        }
    }

    fn count(path: &Path, table: &str) -> i64 {
        let connection = Connection::open(path).expect("second connection reads the store");
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count query")
    }

    #[tokio::test]
    async fn every_row_kind_lands_in_its_table() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs.sqlite");
        let recorder = SqliteRunRecorder::open(&path).expect("store opens");

        recorder
            .record_runs(&[run_row("run-1")])
            .await
            .expect("runs");
        recorder
            .record_metrics(&[metrics_row("run-1")])
            .await
            .expect("metrics");
        recorder
            .record_trades(&[trade_row("run-1", 1), trade_row("run-1", 2)])
            .await
            .expect("trades");
        recorder
            .record_series(&[series_row("run-1")])
            .await
            .expect("series");

        assert_eq!(count(&path, "runs"), 1);
        assert_eq!(count(&path, "run_metrics"), 1);
        assert_eq!(count(&path, "trades"), 2);
        assert_eq!(count(&path, "run_series"), 1);
    }

    #[tokio::test]
    async fn decimals_are_stored_as_exact_text_with_nine_digits() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs.sqlite");
        let recorder = SqliteRunRecorder::open(&path).expect("store opens");
        recorder
            .record_trades(&[trade_row("run-1", 1)])
            .await
            .expect("trades");

        let connection = Connection::open(&path).expect("second connection");
        let (entry_price, quantity): (String, String) = connection
            .query_row(
                "SELECT entry_price, quantity FROM trades WHERE trade_id = 'run-1-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("trade row");
        assert_eq!(entry_price, "65000.12");
        assert_eq!(quantity, "0.000000001");
    }

    #[tokio::test]
    async fn reopening_keeps_the_rows_and_refuses_a_newer_schema() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs.sqlite");
        {
            let recorder = SqliteRunRecorder::open(&path).expect("store opens");
            recorder
                .record_runs(&[run_row("run-1")])
                .await
                .expect("runs");
        }
        let reopened = SqliteRunRecorder::open(&path).expect("reopens");
        reopened
            .record_runs(&[run_row("run-2")])
            .await
            .expect("runs");
        assert_eq!(count(&path, "runs"), 2);

        let raw = Connection::open(&path).expect("raw connection");
        raw.pragma_update(None, "user_version", 99)
            .expect("bump version");
        drop(raw);
        let error = SqliteRunRecorder::open(&path).expect_err("newer schema is refused");
        assert!(error.to_string().contains("schema version 99"), "{error}");
    }

    #[tokio::test]
    async fn a_failing_write_is_reported_as_a_storage_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs.sqlite");
        let recorder = SqliteRunRecorder::open(&path).expect("store opens");
        Connection::open(&path)
            .expect("raw connection")
            .execute_batch("DROP TABLE trades;")
            .expect("drop table");

        let error = recorder
            .record_trades(&[trade_row("run-1", 1)])
            .await
            .expect_err("the table is gone");
        assert!(matches!(error, RecorderError::Storage(_)), "{error}");
    }

    #[tokio::test]
    async fn concurrent_callers_are_all_committed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("runs.sqlite");
        let recorder = std::sync::Arc::new(SqliteRunRecorder::open(&path).expect("store opens"));
        let mut tasks = Vec::new();
        for caller in 0..8 {
            let recorder = std::sync::Arc::clone(&recorder);
            tasks.push(tokio::spawn(async move {
                for run in 0..10 {
                    let run_id = format!("run-{caller}-{run}");
                    recorder
                        .record_runs(&[run_row(&run_id)])
                        .await
                        .expect("batch commits");
                }
            }));
        }
        for task in tasks {
            task.await.expect("caller finishes");
        }
        assert_eq!(count(&path, "runs"), 80);
    }
}
