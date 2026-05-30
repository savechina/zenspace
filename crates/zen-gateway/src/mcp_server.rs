use rig_compose::registry::ToolRegistry;
use rig_mcp::{LoopbackTransport, McpTransport};
use thiserror::Error;
use tracing::{info, warn};

#[derive(Debug, Error)]
pub enum McpServerError {
    #[error("failed to start MCP stdio server: {0}")]
    Start(String),

    #[error("transport error: {0}")]
    Transport(String),
}

/// MCP server configuration for stdio transport.
#[derive(Default)]
pub struct McpConfig {
    pub loopback: bool,
}

/// MCP server that exposes a rig-compose ToolRegistry via the MCP protocol.
#[allow(dead_code)]
pub struct McpServer {
    #[allow(dead_code)]
    config: McpConfig,
    registry: ToolRegistry,
}

impl McpServer {
    pub fn new(config: McpConfig) -> Self {
        Self {
            config,
            registry: ToolRegistry::new(),
        }
    }

    pub fn with_registry(config: McpConfig, registry: ToolRegistry) -> Self {
        Self { config, registry }
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.registry
    }

    /// Start the MCP server using stdio transport (for external MCP clients).
    pub async fn start_stdio(&self) -> Result<(), McpServerError> {
        use rig_mcp::serve_stdio;

        info!("Starting MCP server (stdio transport)");

        serve_stdio(self.registry.clone()).await.map_err(|e| {
            warn!("MCP stdio server failed: {}", e);
            McpServerError::Start(e.to_string())
        })
    }

    /// Create a LoopbackTransport for integration testing against this server's registry.
    pub fn create_loopback(&self, endpoint: impl Into<String>) -> LoopbackTransport {
        LoopbackTransport::new(endpoint, self.registry.clone())
    }

    /// Validate MCP server connectivity via loopback transport.
    pub async fn health_check(&self) -> Result<String, McpServerError> {
        let transport = self.create_loopback("zen:test:health");
        let _tools = transport
            .list_tools()
            .await
            .map_err(|e| McpServerError::Transport(e.to_string()))?;
        Ok("MCP server healthy".to_string())
    }
}
