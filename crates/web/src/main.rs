//! ATSNT Web Telemetry Server binary entrypoint.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::Parser;
use tokio::sync::broadcast;
use tracing::info;
use web::{create_router_with_security, mock, AppState, DashboardSecurity};

/// Command line arguments for configuring the ATSNT web telemetry server.
#[derive(Parser, Debug)]
#[command(
    name = "atsnt-web",
    author = "ATSNT Authors",
    version,
    about = "ATSNT Real-Time Web Telemetry Server & Dashboard"
)]
struct Cli {
    /// Literal loopback IP to bind on (hostnames and public interfaces are rejected).
    #[arg(long, default_value = "127.0.0.1", value_parser = web::security::parse_loopback_ip)]
    host: IpAddr,

    /// TCP port to listen on (1-65535; zero is not supported).
    #[arg(short, long, default_value_t = 3000, value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,

    /// Additional browser origin for tunnel access, e.g. https://dashboard.example.com.
    /// Repeat for multiple origins; no paths, wildcards, or credentials are allowed.
    #[arg(long = "allowed-origin")]
    allowed_origins: Vec<String>,

    /// Directory containing static frontend SPA assets (HTML, CSS, JS).
    #[arg(long)]
    static_dir: Option<PathBuf>,

    /// Directory containing historical JSON audit reports.
    #[arg(long, default_value = "storage/reports")]
    reports_dir: PathBuf,

    /// Run synthetic market ticker generator for development and UI demonstration.
    #[arg(long, default_value_t = false)]
    mock: bool,

    /// Own an in-process paper session, waiting for a future trade source.
    #[arg(long, conflicts_with = "mock")]
    paper: bool,
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
    let addr = SocketAddr::new(cli.host, cli.port);
    // Fail closed before spawning tasks, reading reports, or opening the listener.
    let security = DashboardSecurity::new(addr, &cli.allowed_origins)?;
    let (event_sender, _) = broadcast::channel(10_000);

    // Configure identity and subscribe the state updater before starting a producer.
    // Construction leaves the timestamp null until an actual event is observed.
    let app_state = if cli.mock {
        AppState::with_telemetry(
            event_sender.clone(),
            cli.reports_dir,
            mock::demo_telemetry_config(),
        )
    } else {
        AppState::new(event_sender.clone(), cli.reports_dir)
    };

    if cli.mock {
        mock::spawn_mock_ticker(event_sender);
    }
    // Retain the bounded input even without a source so the session waits.
    // Shutdown/finalization coordination is a separate lifecycle work unit.
    let _paper_runtime = if cli.paper {
        let (owner, input) = web::paper::PaperSessionOwner::new(
            backtest::PaperTradingConfig::default(),
            None,
            app_state.event_sender.clone(),
        )?;
        Some((input, tokio::spawn(owner.run())))
    } else {
        None
    };
    let app = create_router_with_security(app_state, cli.static_dir, security);

    info!(
        host = %cli.host,
        port = cli.port,
        mock_mode = cli.mock,
        paper_mode = cli.paper,
        "ATSNT Web Telemetry Dashboard starting on http://{addr}"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_rejects_non_loopback_and_non_literal_hosts() {
        for host in ["0.0.0.0", "::", "192.0.2.1", "localhost", "example.com"] {
            assert!(
                Cli::try_parse_from(["web", "--host", host]).is_err(),
                "{host}"
            );
        }
    }

    #[test]
    fn cli_accepts_explicit_paper_mode() {
        let cli = Cli::try_parse_from(["web", "--paper"]).unwrap();
        assert!(cli.paper);
        assert!(!cli.mock);
    }

    #[test]
    fn cli_rejects_conflicting_mock_and_paper_modes() {
        for args in [["web", "--mock", "--paper"], ["web", "--paper", "--mock"]] {
            let error = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    #[test]
    fn cli_default_and_mock_modes_remain_distinct() {
        let default = Cli::try_parse_from(["web"]).unwrap();
        assert!(!default.mock && !default.paper);
        let mock = Cli::try_parse_from(["web", "--mock"]).unwrap();
        assert!(mock.mock && !mock.paper);
    }

    #[test]
    fn cli_rejects_ephemeral_port_without_a_stable_default_origin() {
        assert!(Cli::try_parse_from(["web", "--port", "0"]).is_err());
    }

    #[test]
    fn cli_accepts_loopback_and_explicit_tunnel_origin() {
        let cli = Cli::try_parse_from([
            "web",
            "--host",
            "::1",
            "--port",
            "3001",
            "--allowed-origin",
            "https://dashboard.example",
            "--allowed-origin",
            "https://dashboard.example:8443",
        ])
        .unwrap();
        assert!(
            DashboardSecurity::new(SocketAddr::new(cli.host, cli.port), &cli.allowed_origins)
                .is_ok()
        );
    }

    #[test]
    fn invalid_cli_origin_fails_startup_configuration() {
        let cli =
            Cli::try_parse_from(["web", "--allowed-origin", "https://dashboard.example/path"])
                .unwrap();
        assert!(
            DashboardSecurity::new(SocketAddr::new(cli.host, cli.port), &cli.allowed_origins)
                .is_err()
        );
    }
}
