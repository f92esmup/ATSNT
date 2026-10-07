//! WebSocket upgrade and real-time event streaming handler.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use backtest::{PaperTradingEvent, TelemetryEnvelope};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use crate::state::AppState;

/// GET /ws/telemetry - Upgrades HTTP connection to WebSocket and streams real-time events.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(app_state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, app_state))
}

/// Receives a client stream message, replacing a detected gap with a stale
/// last-known snapshot. This does not change the shared aggregator's certainty.
pub async fn receive_client_message(
    rx: &mut broadcast::Receiver<TelemetryEnvelope<PaperTradingEvent>>,
    app_state: &AppState,
) -> Result<serde_json::Value, broadcast::error::RecvError> {
    match rx.recv().await {
        Ok(event) => Ok(serde_json::json!(event)),
        Err(broadcast::error::RecvError::Lagged(skipped)) => {
            debug!(
                skipped_count = skipped,
                "Slow WebSocket client lagged behind event stream"
            );
            let mut snapshot = app_state.state.read().await.clone();
            snapshot.stale = true;
            // Discard the pre-snapshot backlog. Future events remain uncertain,
            // but cannot replay older buffered values over this projection.
            *rx = rx.resubscribe();
            Ok(serde_json::json!(TelemetryEnvelope {
                timestamp: snapshot.timestamp,
                strategy_id: snapshot.active_strategy.clone(),
                symbol: snapshot.active_symbol.clone(),
                event_type: "InitialSnapshot".to_string(),
                payload: snapshot,
            }))
        }
        Err(error) => Err(error),
    }
}

/// Manages an individual active WebSocket connection lifecycle.
async fn handle_socket(socket: WebSocket, app_state: AppState) {
    app_state.client_connected();
    let current_clients = app_state.connected_count();
    info!(
        connected_clients = current_clients,
        "New WebSocket client connected to /ws/telemetry"
    );

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let mut rx = app_state.event_sender.subscribe();
    let mut uncertainty = app_state.uncertainty.clone();

    // 1. Send initial telemetry snapshot immediately upon connection
    let initial_snapshot = {
        let st = app_state.state.read().await;
        TelemetryEnvelope {
            timestamp: st.timestamp,
            strategy_id: st.active_strategy.clone(),
            symbol: st.active_symbol.clone(),
            event_type: "InitialSnapshot".to_string(),
            payload: st.clone(),
        }
    };

    if let Ok(msg_text) = serde_json::to_string(&initial_snapshot) {
        if let Err(e) = ws_sender.send(Message::Text(msg_text)).await {
            error!(error = %e, "Failed to send initial snapshot to WebSocket client");
            app_state.client_disconnected();
            return;
        }
    }

    // 2. Event forwarding loop
    let mut uncertainty_sent = initial_snapshot.payload.stale;
    loop {
        tokio::select! {
            // Outbound event from paper trading broadcast channel
            event_result = receive_client_message(&mut rx, &app_state) => {
                match event_result {
                    Ok(event) => {
                        if let Ok(text) = serde_json::to_string(&event) {
                            if let Err(e) = ws_sender.send(Message::Text(text)).await {
                                debug!(error = %e, "Client disconnected or failed to receive WebSocket message");
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(skipped_count = skipped, "Client stream gap");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("Event broadcast channel closed; terminating client WebSocket");
                        break;
                    }
                }
            }

            // Wake even when the producer stops immediately after a gap.
            changed = uncertainty.changed(), if !uncertainty_sent => {
                if changed.is_err() {
                    uncertainty_sent = true;
                    continue;
                }
                if !*uncertainty.borrow_and_update() {
                    continue;
                }
                let snapshot = app_state.state.read().await.clone();
                // The notice replaces the projection; do not replay its backlog.
                rx = rx.resubscribe();
                let notice = TelemetryEnvelope {
                    timestamp: snapshot.timestamp,
                    strategy_id: snapshot.active_strategy.clone(),
                    symbol: snapshot.active_symbol.clone(),
                    event_type: "InitialSnapshot".to_string(),
                    payload: snapshot,
                };
                if ws_sender.send(Message::Text(serde_json::json!(notice).to_string())).await.is_err() {
                    break;
                }
                uncertainty_sent = true;
            }

            // Inbound messages from the client (e.g. Ping or Close)
            inbound = ws_receiver.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => {
                        debug!("WebSocket client initiated close or socket dropped");
                        break;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(e) = ws_sender.send(Message::Pong(payload)).await {
                            debug!(error = %e, "Failed sending Pong to client");
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        debug!(error = %e, "WebSocket reception error; disconnecting client");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    app_state.client_disconnected();
    let remaining = app_state.connected_count();
    info!(
        connected_clients = remaining,
        "WebSocket client disconnected from /ws/telemetry"
    );
}
