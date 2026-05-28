use std::sync::Arc;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::{extract::State, response::IntoResponse};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

use zen_agents::AgentRegistry;

use crate::HttpConfig;

#[derive(Clone)]
pub struct GatewayState {
    pub agents: Arc<Mutex<dyn AgentRegistry + Send>>,
    pub config: HttpConfig,
}

/// Request body for POST /api/v1/chat.
#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    pub agent: Option<String>,
    pub session_id: Option<String>,
}

/// Response body for POST /api/v1/chat.
#[derive(Serialize)]
pub struct ChatResponse {
    pub reply: String,
    pub agent: Option<String>,
}

/// Public agent info for GET /api/v1/agents.
#[derive(Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub role: String,
    pub capabilities: Vec<String>,
}

/// WebSocket upgrade handler for /api/v1/ws.
///
/// Upgrades the HTTP connection to WebSocket and spawns a message loop
/// that echoes messages back (Phase 1). Phase 2 will route through the
/// selected agent and stream LLM tokens.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(_state): State<GatewayState>,
) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();

    tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            let text = match msg {
                axum::extract::ws::Message::Text(t) => t,
                axum::extract::ws::Message::Close(_) => break,
                _ => continue,
            };

            info!("ws received: {}", text);

            let reply = serde_json::json!({
                "type": "reply",
                "content": format!("Echo: {}", text),
                "status": "ok"
            });

            if let Err(e) = sender
                .send(axum::extract::ws::Message::Text(reply.to_string().into()))
                .await
            {
                warn!("ws send error: {}", e);
                break;
            }
        }
        info!("ws connection closed");
    });
}
