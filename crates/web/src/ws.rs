//! WebSocket upgrade and real-time event streaming handler.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use tracing::{debug, error, info};

use crate::state::AppState;

/// GET /ws/telemetry - Upgrades HTTP connection to WebSocket and streams real-time events.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(app_state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, app_state))
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

    // 1. Send initial telemetry snapshot immediately upon connection
    let initial_snapshot = {
        let st = app_state.state.read().await;
        serde_json::json!({
            "event_type": "InitialSnapshot",
            "payload": &*st,
        })
    };

    if let Ok(msg_text) = serde_json::to_string(&initial_snapshot) {
        if let Err(e) = ws_sender.send(Message::Text(msg_text)).await {
            error!(error = %e, "Failed to send initial snapshot to WebSocket client");
            app_state.client_disconnected();
            return;
        }
    }

    // 2. Event forwarding loop
    loop {
        tokio::select! {
            // Outbound event from paper trading broadcast channel
            event_result = rx.recv() => {
                match event_result {
                    Ok(event) => {
                        let json_msg = serde_json::json!({
                            "event_type": format!("{:?}", event).split('{').next().unwrap_or("Event").split('(').next().unwrap_or("Event").trim(),
                            "payload": event,
                        });

                        if let Ok(text) = serde_json::to_string(&json_msg) {
                            if let Err(e) = ws_sender.send(Message::Text(text)).await {
                                debug!(error = %e, "Client disconnected or failed to receive WebSocket message");
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(skipped_count = skipped, "Slow WebSocket client lagged behind event stream");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("Event broadcast channel closed; terminating client WebSocket");
                        break;
                    }
                }
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
