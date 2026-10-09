//! Binance Live Execution Gateway CLI runner.
//!
//! Provides command-line utilities to test account balances, verify HMAC signatures,
//! and stream normalized private account events on Binance Testnet or Production.

use std::env;
use std::sync::Arc;

use adapters::{BinanceAuth, BinanceGateway, BinanceGatewayConfig, BinancePrivateUserDataStream};
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

    /// Connect to the private User Data Stream for normalized account and order events.
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

    adapters::load_dotenv();

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

    let stream_auth =
        (cli.listen && is_spot).then(|| BinanceAuth::new(api_key.clone(), secret_key.clone()));
    let config = BinanceGatewayConfig {
        api_key,
        secret_key,
        testnet: cli.testnet,
        spot: is_spot,
        recv_window_ms: 5000,
        risk_policy: domain::RiskPolicy::default(),
    };

    let gateway = Arc::new(BinanceGateway::new(config)?);

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
        let mut user_stream = if is_spot {
            let auth = stream_auth
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Spot stream credentials are unavailable"))?;
            BinancePrivateUserDataStream::connect_spot(auth, cli.testnet).await?
        } else {
            let listen_key = gateway.create_listen_key().await?;
            BinancePrivateUserDataStream::connect_futures(listen_key, cli.testnet, gateway.clone())
                .await?
        };
        info!("Listening for private account events (Press Ctrl+C to exit)...");

        let shutdown_signal = tokio::signal::ctrl_c();
        tokio::pin!(shutdown_signal);
        loop {
            tokio::select! {
                signal_result = &mut shutdown_signal => {
                    if let Err(error) = signal_result {
                        user_stream.shutdown().await?;
                        return Err(error.into());
                    }
                    user_stream.shutdown().await?;
                    info!("Private account stream shut down; account reconciliation was not performed.");
                    break;
                }
                event = user_stream.next_event() => {
                    match event {
                        Some(Ok(event)) => info!(event = ?event, "Received private account event"),
                        Some(Err(error)) => {
                            error!(error = %error, "Private account stream failed; reconciliation is required");
                            user_stream.shutdown().await?;
                            return Err(error.into());
                        }
                        None => {
                            user_stream.shutdown().await?;
                            anyhow::bail!("Private account stream ended without an explicit reconciliation result");
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
