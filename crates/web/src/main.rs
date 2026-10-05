//! ATSNT Web Telemetry Server binary entrypoint.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use tokio::sync::broadcast;
use tracing::info;
use web::{create_router, mock, AppState};

/// Command line arguments for configuring the ATSNT web telemetry server.
#[derive(Parser, Debug)]
#[command(
    name = "atsnt-web",
    author = "ATSNT Authors",
    version,
    about = "ATSNT Real-Time Web Telemetry Server & Dashboard"
)]
struct Cli {
    /// Network host interface to bind on (strict localhost for Cloudflare Zero-Trust).
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// TCP port to listen on.
    #[arg(short, long, default_value_t = 3000)]
    port: u16,

    /// Directory containing static frontend SPA assets (HTML, CSS, JS).
    #[arg(long)]
    static_dir: Option<PathBuf>,

    /// Directory containing historical JSON audit reports.
    #[arg(long, default_value = "storage/reports")]
    reports_dir: PathBuf,

    /// Run synthetic market ticker generator for development and UI demonstration.
    #[arg(long, default_value_t = false)]
    mock: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,web=debug,tower_http=debug".into()),
        )
        .init();

    let cli = Cli::parse();
    let (event_sender, _) = broadcast::channel(10_000);

    // If mock flag is enabled, spawn synthetic market data ticker
    if cli.mock {
        mock::spawn_mock_ticker(event_sender.clone());
    }

    let app_state = AppState::new(event_sender, cli.reports_dir);
    let app = create_router(app_state, cli.static_dir);

    let addr_str = format!("{}:{}", cli.host, cli.port);
    let addr: SocketAddr = addr_str.parse()?;

    info!(
        host = %cli.host,
        port = cli.port,
        mock_mode = cli.mock,
        "ATSNT Web Telemetry Dashboard starting on http://{addr_str}"
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("ATSNT Web Telemetry server gracefully shut down");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
