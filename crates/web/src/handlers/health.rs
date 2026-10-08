//! Health and uptime status handler.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

/// System health probe response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Service health status.
    pub status: &'static str,
    /// Elapsed uptime in seconds.
    pub uptime_secs: u64,
    /// Crate version.
    pub version: &'static str,
    /// Number of actively connected WebSocket clients.
    pub connected_ws_clients: usize,
    /// Configured execution mode ("mock", "paper", or "idle").
    pub execution_mode: &'static str,
}

/// GET /api/health
pub async fn health_handler(State(app_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        uptime_secs: app_state.uptime_secs(),
        version: env!("CARGO_PKG_VERSION"),
        connected_ws_clients: app_state.connected_count(),
        execution_mode: app_state.execution_mode,
    })
}
