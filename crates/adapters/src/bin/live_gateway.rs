//! Binance Live Execution Gateway CLI runner.
//!
//! Provides command-line utilities to test account balances, verify HMAC signatures,
//! simulate order placement via POST /api/v3/order/test, and stream normalized private
//! account events on Binance Testnet or Production.

use std::sync::Arc;

use adapters::{
    BinanceAuth, BinanceGateway, BinanceGatewayConfig, BinancePrivateUserDataStream,
    GcpSecretManager,
};
use clap::Parser;
use rust_decimal::Decimal;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(
    name = "live_gateway",
    author = "ATSNT Authors",
    version,
    about = "ATSNT Live Exchange Execution Gateway (Binance Testnet & Production)"
)]
struct Cli {
    /// Binance API Key (can also be read from BINANCE_API_KEY env var or GCP Secret Manager).
    #[arg(long)]
    api_key: Option<String>,

    /// Binance Secret Key (can also be read from BINANCE_SECRET_KEY env var or GCP Secret Manager).
    #[arg(long)]
    secret_key: Option<String>,

    /// Use official Binance Testnet (defaults to false for production execution).
    #[arg(long, default_value_t = false)]
    testnet: bool,

    /// Target USD-M Perpetual Futures instead of Spot.
    #[arg(long, default_value_t = false)]
    futures: bool,

    /// Query and print current available account balance.
    #[arg(long, default_value_t = false)]
    check_balance: bool,

    /// Specific currency asset to query balance for.
    #[arg(long, default_value = "USDT")]
    asset: String,

    /// Test a simulated order against Binance Spot test endpoint (POST /api/v3/order/test).
    #[arg(long)]
    test_order: Option<String>,

    /// Quantity for test simulated order (e.g. 0.0001 for BTCUSDT or 2.0 for NEARUSDT).
    #[arg(long)]
    test_quantity: Option<Decimal>,

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

    let secret_manager = GcpSecretManager::from_env();

    let api_key = match cli.api_key {
        Some(k) if !k.trim().is_empty() => k,
        _ => secret_manager
            .resolve_secret("BINANCE_API_KEY")
            .await
            .unwrap_or_default(),
    };

    let secret_key = match cli.secret_key {
        Some(s) if !s.trim().is_empty() => s,
        _ => secret_manager
            .resolve_secret("BINANCE_SECRET_KEY")
            .await
            .unwrap_or_default(),
    };

    let api_key = api_key
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();
    let secret_key = secret_key
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();

    if api_key.len() != 64 {
        error!(
            current_len = api_key.len(),
            expected_len = 64,
            "BINANCE_API_KEY has an invalid length: standard Binance HMAC API keys are exactly 64 characters long. Check if an extra character was accidentally copied at the beginning or end."
        );
    }

    if api_key.is_empty() || secret_key.is_empty() {
        error!("API key or Secret key not provided. Store BINANCE_API_KEY and BINANCE_SECRET_KEY in GCP Secret Manager, .env, or pass --api-key and --secret-key");
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
        risk_per_trade_pct: rust_decimal::Decimal::new(1, 2),
        allow_foreign_collateral: false,
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

        if is_spot {
            match gateway.fetch_spot_balances().await {
                Ok(balances) => {
                    info!("All Spot Balances with non-zero funds:");
                    let mut any = false;
                    for (asset, free) in &balances {
                        if *free > Decimal::ZERO {
                            info!("  • {asset}: {free}");
                            any = true;
                        }
                    }
                    if !any {
                        info!("  (All Spot asset balances are zero / not yet funded)");
                    }
                }
                Err(e) => error!(error = %e, "Failed to fetch all spot balances"),
            }
        }
    }

    if let Some(symbol) = &cli.test_order {
        let sym_upper = symbol.to_ascii_uppercase();
        let qty = cli.test_quantity.unwrap_or(match sym_upper.as_str() {
            "BTCUSDT" => rust_decimal_macros::dec!(0.0001),
            "ETHUSDT" => rust_decimal_macros::dec!(0.005),
            "SOLUSDT" => rust_decimal_macros::dec!(0.05),
            "BNBUSDT" => rust_decimal_macros::dec!(0.01),
            "AVAXUSDT" => rust_decimal_macros::dec!(0.5),
            "NEARUSDT" => rust_decimal_macros::dec!(2.0),
            _ => rust_decimal_macros::dec!(1.0),
        });

        info!(
            symbol = %sym_upper,
            quantity = %qty,
            "Validating simulated order against Binance Spot (POST /api/v3/order/test)..."
        );
        match gateway
            .test_spot_order(&sym_upper, domain::Side::Buy, "MARKET", qty, None)
            .await
        {
            Ok(()) => info!(
                symbol = %sym_upper,
                "✅ Test order accepted by Binance Spot API! Valid HMAC signature, lot size, and trading permissions."
            ),
            Err(e) => error!(
                symbol = %sym_upper,
                error = %e,
                "❌ Test order rejected by Binance Spot API"
            ),
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
