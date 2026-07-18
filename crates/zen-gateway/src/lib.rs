pub use gateway_error::GatewayError;
pub use gateway_trait::{Gateway, GatewayStatus};

mod daemon;
mod gateway_error;
mod gateway_trait;
mod inference_gateway;
mod routes;
pub mod subconscious;

pub mod cron;
pub mod mcp_server;
pub mod qqbot;

pub use daemon::{HttpConfig, HttpGateway, read_pid, remove_pid, write_pid};
pub use inference_gateway::{
    BatchedRequest, CompletionRequest, CompletionResponse, ContinuousBatcher, GatewayStats,
    InferenceGateway, PromptTrieNode,
};
pub use mcp_server::{McpConfig, McpServer, McpServerError};
pub use routes::{AgentInfo, ChatRequest, ChatResponse, GatewayState};
pub use subconscious::{MicroAction, SubconsciousTick};
