pub mod binance_auth;
pub mod binance_csv;
pub mod binance_fetcher;
pub mod binance_gateway;
pub mod binance_parquet;
pub mod binance_private_stream;
pub mod binance_user_stream;
pub mod binance_ws;
pub mod error;
pub mod gcp;
pub mod traits;

pub use binance_auth::BinanceAuth;
pub use binance_csv::BinanceCsvReader;
pub use binance_fetcher::{BinanceDataFetcher, FetchParams, FetchSummary};
pub use binance_gateway::{
    BinanceGateway, BinanceGatewayConfig, GatewayError, OrderExecutionReport,
};
pub use binance_parquet::{BinanceParquetReader, BinanceParquetWriter};
pub use binance_private_stream::{
    BinancePrivateEvent, BinancePrivateStreamError, BinancePrivateUserDataStream,
    FuturesAccountUpdate, FuturesBalance, FuturesOrderTradeUpdate, FuturesPosition,
    ListenKeyRenewer, SpotAccountPosition, SpotBalance, SpotBalanceDelta,
};
pub use binance_user_stream::{BinanceUserDataStream, ExecutionUpdate};
pub use binance_ws::{BinanceAggTradePayload, BinanceWebSocketStream, BinanceWsConfig};
pub use error::AdapterError;
pub use gcp::{
    datetime_in_madrid, format_unix_ms_rfc3339, now_in_madrid, BigQueryRecord, BigQuerySink,
    DollarBarRow, EquitySnapshotRow, GcpSecretManager, GcsParquetSink, HpoEvaluationRow,
    MonteCarloRow, TelegramNotifier, TradeRow,
};
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
