use std::net::SocketAddr;
use std::path::Path;
use std::process;
use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
};
use serde::Serialize;
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::gateway_error::GatewayError;
use crate::gateway_trait::{Gateway, GatewayStatus};
use crate::routes::{AgentInfo, ChatRequest, ChatResponse, GatewayState, ws_handler};
use zen_agents::{AgentRegistry, DefaultAgentRegistry};

/// HTTP configuration for the gateway daemon.
#[derive(Clone)]
pub struct HttpConfig {
    pub bind_addr: String,
    pub port: u16,
    pub jwt_secret: Option<String>,
    pub rate_limit_rpm: u32,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1".to_string(),
            port: 9876,
            jwt_secret: None,
            rate_limit_rpm: 100,
        }
    }
}

/// Internal state tracking whether the gateway is running.
#[derive(Clone)]
struct RunningState {
    status: Arc<Mutex<GatewayStatus>>,
}

impl RunningState {
    fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(GatewayStatus::Stopped)),
        }
    }

    async fn set(&self, status: GatewayStatus) {
        *self.status.lock().await = status;
    }

    async fn get(&self) -> GatewayStatus {
        self.status.lock().await.clone()
    }
}

/// HTTP gateway daemon with axum server.
pub struct HttpGateway {
    pub config: HttpConfig,
    state: RunningState,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HttpGateway {
    pub fn new(config: HttpConfig) -> Self {
        Self {
            config,
            state: RunningState::new(),
            shutdown_tx: None,
        }
    }

    fn build_router(&self, state: GatewayState) -> Router {
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        Router::new()
            .route("/health", get(health_check))
            .route("/api/v1/chat", post(chat_handler))
            .route("/api/v1/ws", get(ws_handler))
            .route("/api/v1/agents", get(list_agents_handler))
            .layer(cors)
            .layer(TraceLayer::new_for_http())
            .with_state(state)
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
    agents: usize,
}

async fn health_check(State(state): State<GatewayState>) -> Json<HealthResponse> {
    let agent_count = state.agents.lock().await.list_all().len();
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        agents: agent_count,
    })
}

#[derive(Serialize)]
struct AgentListResponse {
    agents: Vec<AgentInfo>,
}

async fn list_agents_handler(State(state): State<GatewayState>) -> Json<AgentListResponse> {
    let guard = state.agents.lock().await;
    let agents = guard.list_all();
    let agent_infos: Vec<AgentInfo> = agents
        .iter()
        .map(|a| AgentInfo {
            name: a.name.clone(),
            role: format!("{:?}", a.role),
            capabilities: a.capabilities.iter().map(|c| format!("{:?}", c)).collect(),
        })
        .collect();
    Json(AgentListResponse {
        agents: agent_infos,
    })
}

async fn chat_handler(
    State(state): State<GatewayState>,
    Json(req): Json<ChatRequest>,
) -> (StatusCode, Json<ChatResponse>) {
    if req.message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ChatResponse {
                reply: "Empty message".to_string(),
                agent: None,
            }),
        );
    }

    let agent_name = req
        .agent
        .clone()
        .unwrap_or_else(|| "Sisyphus-Junior".to_string());
    let agents = state.agents.lock().await;
    let agent = agents.find_by_name(&agent_name);

    match agent {
        Ok(_) => {
            drop(agents);
            let reply = format!("[{}] Processing: {}", agent_name, req.message);
            (
                StatusCode::OK,
                Json(ChatResponse {
                    reply,
                    agent: Some(agent_name),
                }),
            )
        },
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(ChatResponse {
                reply: format!("Agent '{}' not found", agent_name),
                agent: None,
            }),
        ),
    }
}

impl Gateway for HttpGateway {
    fn start(&mut self, port: u16) -> Result<(), GatewayError> {
        let config = self.config.clone();
        let state = self.state.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        let agents: Arc<Mutex<dyn AgentRegistry + Send>> =
            Arc::new(Mutex::new(DefaultAgentRegistry::new()));

        let gateway_state = GatewayState {
            agents,
            config: config.clone(),
        };

        let router = self.build_router(gateway_state);

        let addr: SocketAddr = format!("{}:{}", config.bind_addr, port)
            .parse()
            .map_err(|e: std::net::AddrParseError| GatewayError::Bind(e.to_string()))?;

        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&addr).await {
                Ok(l) => l,
                Err(e) => {
                    warn!("Failed to bind gateway on {}: {}", addr, e);
                    state.set(GatewayStatus::Error).await;
                    return;
                },
            };

            info!("Gateway listening on http://{}", addr);
            state.set(GatewayStatus::Running).await;

            let server = axum::serve(listener, router.into_make_service()).with_graceful_shutdown(
                async move {
                    let _ = shutdown_rx.await;
                },
            );

            if let Err(e) = server.await {
                warn!("Gateway server error: {}", e);
                state.set(GatewayStatus::Error).await;
            }
        });

        Ok(())
    }

    fn stop(&mut self) -> Result<(), GatewayError> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            info!("Gateway shutdown signal sent");
        }
        Ok(())
    }

    fn health_check(&self) -> Result<String, GatewayError> {
        Ok(format!(
            "zen-gateway v{} (stub health)",
            env!("CARGO_PKG_VERSION")
        ))
    }

    fn status(&self) -> GatewayStatus {
        GatewayStatus::Running
    }
}

/// Write the current process PID to daemon.pid.
pub fn write_pid<P: AsRef<Path>>(path: P) -> Result<(), GatewayError> {
    let pid = process::id().to_string();
    std::fs::write(&path, &pid)
        .map_err(GatewayError::from)
        .inspect(
            |_| tracing::debug!(pid = pid, path = %path.as_ref().display(), "PID file written"),
        )
}

/// Read and parse the PID from an existing pid file.
pub fn read_pid<P: AsRef<Path>>(path: P) -> Result<u32, GatewayError> {
    let content = std::fs::read_to_string(&path).map_err(GatewayError::from)?;
    content
        .trim()
        .parse::<u32>()
        .map_err(|_| GatewayError::NotImplemented)
}

pub fn remove_pid<P: AsRef<Path>>(path: P) -> Result<(), GatewayError> {
    std::fs::remove_file(&path)
        .map_err(GatewayError::from)
        .inspect(|_| tracing::debug!(path = %path.as_ref().display(), "PID file removed"))
}
