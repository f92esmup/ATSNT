//! Binance Live Execution Gateway CLI runner.
//!
//! Provides command-line utilities to test account balances, verify HMAC signatures,
//! and stream real-time order executions on Binance Testnet or Production.

use std::env;

use adapters::{BinanceGateway, BinanceGatewayConfig, BinanceUserDataStream};
use clap::Parser;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(
    name = "live_gateway",
    author = "ATSNT Authors",
    version,
    about = "ATSNT Live Exchange Execution Gateway (Binance Testnet & Production)"
)]
struct Cli {
    /// Binance API Key (can also be read from BINANCE_API_KEY env var).
    #[arg(long)]
    api_key: Option<String>,

    /// Binance Secret Key (can also be read from BINANCE_SECRET_KEY env var).
    #[arg(long)]
    secret_key: Option<String>,

    /// Use official Binance Testnet (defaults to true for capital safety).
    #[arg(long, default_value_t = true)]
    testnet: bool,

    /// Target USD-M Perpetual Futures instead of Spot.
    #[arg(long, default_value_t = false)]
    futures: bool,

    /// Query and print current available account balance.
    #[arg(long, default_value_t = false)]
    check_balance: bool,

    /// Currency asset to query balance for.
    #[arg(long, default_value = "USDT")]
    asset: String,

    /// Connect to User Data Stream WebSocket to listen for live order execution reports.
    #[arg(long, default_value_t = false)]
    listen: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,adapters=debug".into()),
        )
        .init();

    let cli = Cli::parse();
    let is_spot = !cli.futures;

    let api_key = cli
        .api_key
        .or_else(|| env::var("BINANCE_API_KEY").ok())
        .unwrap_or_default();

    let secret_key = cli
        .secret_key
        .or_else(|| env::var("BINANCE_SECRET_KEY").ok())
        .unwrap_or_default();

    if api_key.is_empty() || secret_key.is_empty() {
        error!("API key or Secret key not provided. Set BINANCE_API_KEY and BINANCE_SECRET_KEY or pass --api-key and --secret-key");
        std::process::exit(1);
    }

    let config = BinanceGatewayConfig {
        api_key,
        secret_key,
        testnet: cli.testnet,
        spot: is_spot,
        recv_window_ms: 5000,
        risk_policy: domain::RiskPolicy::default(),
    };

    let gateway = BinanceGateway::new(config)?;

    if cli.check_balance {
        info!(asset = %cli.asset, "Querying account balance from Binance...");
        match gateway.fetch_balance(&cli.asset).await {
            Ok(bal) => {
                info!(asset = %cli.asset, available_balance = %bal, "Balance query succeeded")
            }
            Err(e) => error!(error = %e, "Failed to query balance"),
        }
    }

    if cli.listen {
        info!("Requesting listenKey from Binance...");
        let listen_key = gateway.create_listen_key().await?;
        info!(listen_key = %listen_key, "Acquired listenKey for User Data Stream");

        let mut user_stream =
            BinanceUserDataStream::connect(listen_key, cli.testnet, is_spot).await?;
        info!("Listening for live execution reports (Press Ctrl+C to exit)...");

        while let Some(update) = user_stream.next_update().await {
            info!(
                order_id = update.order_id,
                symbol = %update.symbol,
                side = ?update.side,
                status = %update.status,
                last_price = %update.last_filled_price,
                last_qty = %update.last_filled_qty,
                "Received Execution Update"
            );
        }
    }

    Ok(())
}
