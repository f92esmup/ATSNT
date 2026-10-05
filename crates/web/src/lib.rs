//! ATSNT Web Presentation and Telemetry Dashboard Crate.
//!
//! Provides a high-performance, air-gapped read-only web server built with Axum.
//! Serves REST telemetry endpoints, WebSocket streams for real-time paper trading events,
//! and the financial dashboard Single Page Application (SPA).

pub mod handlers;
pub mod mock;
pub mod state;
pub mod ws;

use std::path::{Path, PathBuf};

use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::info;

pub use state::{AppState, PositionInfo, TelemetryState};

/// Builds the complete Axum application router with all REST routes, WebSocket upgrade,
/// and static asset serving.
pub fn create_router(app_state: AppState, static_dir: Option<PathBuf>) -> Router {
    let api_routes = Router::new()
        .route("/health", get(handlers::health_handler))
        .route("/state", get(handlers::get_state_handler))
        .route("/strategies", get(handlers::get_strategies_handler))
        .route("/reports", get(handlers::list_reports_handler))
        .route("/reports/:id", get(handlers::get_report_by_id_handler));

    let mut router = Router::new()
        .nest("/api", api_routes)
        .route("/ws/telemetry", get(ws::ws_handler))
        .with_state(app_state);

    // Resolve static directory
    let resolved_static_dir = resolve_static_dir(static_dir);
    if let Some(static_path) = resolved_static_dir {
        info!(path = %static_path.display(), "Mounting static assets for dashboard SPA");
        let serve_dir = ServeDir::new(&static_path).append_index_html_on_directories(true);
        router = router.fallback_service(serve_dir);
    }

    router
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
}

/// Locates the static assets directory based on candidate locations.
fn resolve_static_dir(custom_path: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = custom_path {
        if path.is_dir() {
            return Some(path);
        }
    }

    // Default candidate locations
    let candidates = [
        Path::new("crates/web/static"),
        Path::new("static"),
        Path::new("../crates/web/static"),
    ];

    for candidate in candidates {
        if candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
    }

    None
}
