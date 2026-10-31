// AGENTS.md: no unwrap or expect in production code; test builds keep them.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod analytics;
pub mod binance_auth;
pub mod binance_csv;
pub mod binance_fetcher;
pub mod binance_funding;
pub mod binance_gateway;
pub mod binance_private_stream;
pub mod binance_recent_trades;
pub mod binance_user_stream;
pub mod binance_ws;
pub mod clock;
pub mod error;
pub mod fetch_plan;
pub mod simulated_venue;
pub mod storage;
pub mod telegram;
#[cfg(test)]
pub(crate) mod test_http;
pub mod traits;

pub use analytics::{InMemoryRunRecorder, NullRunRecorder, RecordedRuns};
pub use binance_auth::BinanceAuth;
pub use binance_csv::BinanceCsvReader;
pub use binance_fetcher::{BinanceDataFetcher, FetchParams, FetchSummary};
pub use binance_funding::{funding_schedule_from_history, parse_funding_history};
pub use binance_gateway::{
    AccountFix, AccountReport, BinanceGateway, BinanceGatewayConfig, ExitKind, Finding,
    FindingStatus, GatewayError, OrderExecutionReport, PositionClosureReport, ProtectiveExitReport,
};
pub use binance_private_stream::{
    BinancePrivateEvent, BinancePrivateStreamError, BinancePrivateUserDataStream,
    FuturesAccountUpdate, FuturesBalance, FuturesOrderTradeUpdate, FuturesPosition,
    ListenKeyRenewer, SpotAccountPosition, SpotBalance, SpotBalanceDelta,
};
pub use binance_recent_trades::BinanceRecentTrades;
pub use binance_user_stream::{BinanceUserDataStream, ExecutionUpdate};
pub use binance_ws::{BinanceAggTradePayload, BinanceWebSocketStream, BinanceWsConfig};
pub use clock::SystemClock;
pub use error::AdapterError;
pub use fetch_plan::{is_not_archived, plan_fetch_tasks, run_bounded, FetchTask};
pub use simulated_venue::{SimulatedVenue, SimulatedVenueConfig, SimulatedVenueError};
pub use storage::sqlite_runs::SqliteRunRecorder;
pub use storage::tape_parquet::{TapeReader, TapeWriter};
pub use telegram::{datetime_in_madrid, now_in_madrid, SymbolPerformanceSummary, TelegramNotifier};
pub use traits::{AlertNotifier, AsyncMarketDataStream, MarketDataStream};

/// Convenience helper to load `.env` key-value pairs into `std::env` if `.env` exists.
pub fn load_dotenv() {
    if let Ok(content) = std::fs::read_to_string(".env") {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim();
                let val = v.trim().trim_matches('"').trim_matches('\'');
                if std::env::var(key).is_err() {
                    std::env::set_var(key, val);
                }
            }
        }
    }
}
