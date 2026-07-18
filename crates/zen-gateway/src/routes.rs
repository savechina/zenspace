use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::{extract::State, response::IntoResponse};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

use zen_agents::{AgentOrchestrator, AgentRegistry};
use zen_core::types::SessionContext;

use crate::HttpConfig;
use crate::inference_gateway::InferenceGateway;

#[derive(Clone)]
pub struct GatewayState {
    pub agents: Arc<Mutex<dyn AgentRegistry + Send>>,
    pub config: HttpConfig,
    pub orchestrator: Option<Arc<AgentOrchestrator>>,
    pub sessions: Arc<Mutex<HashMap<String, SessionContext>>>,
    pub inference_gateway: Arc<InferenceGateway>,
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

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<GatewayState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: GatewayState) {
    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(Mutex::new(sender));

    tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            let text = match msg {
                axum::extract::ws::Message::Text(t) => t,
                axum::extract::ws::Message::Close(_) => break,
                _ => continue,
            };

            info!("ws received: {}", text);

            #[derive(Deserialize)]
            struct WsRequest {
                message: String,
                session_id: Option<String>,
            }

            let req: WsRequest = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(_) => {
                    let reply = serde_json::json!({
                        "type": "error",
                        "content": "Invalid JSON. Expected {\"message\": \"...\", \"session_id\": \"...\"}"
                    });
                    let mut s = sender.lock().await;
                    if let Err(e) = s
                        .send(axum::extract::ws::Message::Text(reply.to_string().into()))
                        .await
                    {
                        warn!("ws send error on invalid JSON: {}", e);
                    }
                    continue;
                }
            };

            if req.message.is_empty() {
                let reply = serde_json::json!({
                    "type": "error",
                    "content": "Empty message"
                });
                let mut s = sender.lock().await;
                if let Err(e) = s
                    .send(axum::extract::ws::Message::Text(reply.to_string().into()))
                    .await
                {
                    warn!("ws send error on empty message: {}", e);
                }
                continue;
            }

            let orchestrator = match &state.orchestrator {
                Some(o) => Arc::clone(o),
                None => {
                    let reply = serde_json::json!({
                        "type": "error",
                        "content": "No LLM provider configured"
                    });
                    let mut s = sender.lock().await;
                    if let Err(e) = s
                        .send(axum::extract::ws::Message::Text(reply.to_string().into()))
                        .await
                    {
                        warn!("ws send error on missing provider: {}", e);
                    }
                    continue;
                }
            };

            let session_key = req
                .session_id
                .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
            let mut sessions = state.sessions.lock().await;
            let session = sessions
                .entry(session_key)
                .or_insert_with(|| SessionContext::new("gateway".to_string(), String::new()));

            let (token_tx, mut token_rx) = mpsc::unbounded_channel::<String>();
            let send_sender = Arc::clone(&sender);

            let send_task = tokio::spawn(async move {
                while let Some(chunk) = token_rx.recv().await {
                    let msg = serde_json::json!({
                        "type": "token",
                        "content": chunk
                    });
                    let mut s = send_sender.lock().await;
                    if let Err(e) = s
                        .send(axum::extract::ws::Message::Text(msg.to_string().into()))
                        .await
                    {
                        warn!("ws send error: {}", e);
                        return;
                    }
                }
            });

            let result = orchestrator
                .execute_stream(&mut *session, &req.message, |token| {
                    if let Err(e) = token_tx.send(token.to_string()) {
                        warn!("token channel closed during stream: {}", e);
                    }
                })
                .await;

            drop(token_tx);
            if let Err(e) = send_task.await {
                warn!("ws send task panicked: {}", e);
            }

            let mut s = sender.lock().await;
            match result {
                Ok(response) => {
                    let done = serde_json::json!({
                        "type": "done",
                        "content": response
                    });
                    if let Err(e) = s
                        .send(axum::extract::ws::Message::Text(done.to_string().into()))
                        .await
                    {
                        warn!("ws send error: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    let err = serde_json::json!({
                        "type": "error",
                        "content": format!("Execution error: {}", e)
                    });
                    if let Err(e) = s
                        .send(axum::extract::ws::Message::Text(err.to_string().into()))
                        .await
                    {
                        warn!("ws send error on execution error: {}", e);
                    }
                }
            }
        }
        info!("ws connection closed");
    });
}
