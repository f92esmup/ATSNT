//! Portfolio telemetry state and strategy catalog handlers.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::state::{AppState, TelemetryState};

/// Strategy configuration metadata returned to the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct StrategyMetadata {
    /// Strategy unique identifier.
    pub id: &'static str,
    /// Human-readable strategy name.
    pub name: &'static str,
    /// Associated trading asset pair.
    pub symbol: &'static str,
    /// Lifecycle status: ACTIVE | PAUSED | CANDIDATE.
    pub status: &'static str,
    /// Quantitative description of the methodology.
    pub description: &'static str,
}

/// GET /api/state - Returns current real-time telemetry snapshot.
pub async fn get_state_handler(State(app_state): State<AppState>) -> Json<TelemetryState> {
    let state = app_state.state.read().await;
    Json(state.clone())
}

/// GET /api/strategies - Returns active algorithmic strategy catalog.
pub async fn get_strategies_handler() -> Json<Vec<StrategyMetadata>> {
    Json(vec![StrategyMetadata {
        id: "dollar_bars_cusum",
        name: "Dollar Bars Symmetric CUSUM Breakout",
        symbol: "BTCUSDT",
        status: "ACTIVE",
        description: "Information-driven volume sampling with symmetric CUSUM filter and Triple Barrier exits",
    }])
}
