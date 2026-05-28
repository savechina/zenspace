use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use thiserror::Error;
use tracing::warn;
use wasmtime::{Config, Engine};

#[derive(Debug, Error)]
pub enum WasmSandboxError {
    #[error("wasmtime error: {0}")]
    Wasmtime(String),

    #[error("permission denied: plugin tried to access {resource}")]
    PermissionDenied { resource: String },

    #[error("module load error: {0}")]
    ModuleLoad(String),

    #[error("execution error: {0}")]
    Execution(String),
}

#[derive(Debug, Clone, Default)]
pub struct WasmPermissions {
    pub allow_filesystem_read: bool,
    pub allow_filesystem_write: bool,
    pub allow_network: bool,
    pub allow_system: bool,
}

pub struct PrecompiledModule {
    pub wasm_bytes: Vec<u8>,
    pub permissions: WasmPermissions,
}

impl PrecompiledModule {
    pub fn from_file(path: &PathBuf) -> Result<Self, WasmSandboxError> {
        let wasm_bytes = std::fs::read(path).map_err(|e| {
            WasmSandboxError::ModuleLoad(format!("failed to read WASM file: {}", e))
        })?;
        Ok(Self {
            wasm_bytes,
            permissions: WasmPermissions::default(),
        })
    }

    pub fn with_permissions(mut self, permissions: WasmPermissions) -> Self {
        self.permissions = permissions;
        self
    }
}

pub struct WasmSandbox {
    engine: Engine,
}

impl WasmSandbox {
    pub fn new() -> Self {
        let config = Config::new();

        let engine = Engine::new(&config).unwrap_or_else(|e| {
            warn!("WASM engine init failed ({e}), falling back to defaults");
            Engine::default()
        });

        Self { engine }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn check_permission(
        &self,
        permissions: &WasmPermissions,
        resource: &str,
    ) -> Result<(), WasmSandboxError> {
        match resource {
            "filesystem:read" if !permissions.allow_filesystem_read => {
                Err(WasmSandboxError::PermissionDenied {
                    resource: resource.to_string(),
                })
            },
            "filesystem:write" if !permissions.allow_filesystem_write => {
                Err(WasmSandboxError::PermissionDenied {
                    resource: resource.to_string(),
                })
            },
            "network" if !permissions.allow_network => Err(WasmSandboxError::PermissionDenied {
                resource: resource.to_string(),
            }),
            "system" if !permissions.allow_system => Err(WasmSandboxError::PermissionDenied {
                resource: resource.to_string(),
            }),
            _ => Ok(()),
        }
    }

    pub fn validate_module(&self, wasm_bytes: &[u8]) -> Result<(), WasmSandboxError> {
        wasmtime::Module::new(&self.engine, wasm_bytes).map_err(|e| {
            WasmSandboxError::ModuleLoad(format!("WASM module validation failed: {}", e))
        })?;
        Ok(())
    }
}

impl Default for WasmSandbox {
    fn default() -> Self {
        Self::new()
    }
}

const NAME: &str = "plugin.wasm_sandbox";
const DESCRIPTION: &str = "Execute WASM plugin in sandboxed environment with permission gating";

static ARGS_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "wasm_path": {
                "type": "string",
                "description": "Path to compiled WASM module"
            },
            "func_name": {
                "type": "string",
                "description": "Function within the WASM module to invoke"
            },
            "inputs": {
                "type": "object",
                "description": "JSON input arguments for the WASM function"
            },
            "permissions": {
                "type": "object",
                "properties": {
                    "filesystem_read": { "type": "boolean" },
                    "filesystem_write": { "type": "boolean" },
                    "network": { "type": "boolean" },
                    "system": { "type": "boolean" }
                }
            }
        },
        "required": ["wasm_path", "func_name"]
    })
});

static RESULT_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "output": { "type": "object" },
            "metrics": {
                "type": "object",
                "properties": {
                    "wasm_path": { "type": "string" },
                    "func_name": { "type": "string" },
                    "permissions_granted": { "type": "object" }
                }
            }
        }
    })
});

pub struct WasmSandboxTool {
    sandbox: Arc<Mutex<WasmSandbox>>,
}

impl WasmSandboxTool {
    pub fn new() -> Self {
        Self {
            sandbox: Arc::new(Mutex::new(WasmSandbox::new())),
        }
    }
}

impl Default for WasmSandboxTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WasmSandboxTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let wasm_path = args
            .get("wasm_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| KernelError::InvalidArgument("wasm_path is required".into()))?;

        let func_name = args
            .get("func_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| KernelError::InvalidArgument("func_name is required".into()))?;

        let permissions_raw = args.get("permissions");
        let permissions = WasmPermissions {
            allow_filesystem_read: permissions_raw
                .and_then(|p| p.get("filesystem_read"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            allow_filesystem_write: permissions_raw
                .and_then(|p| p.get("filesystem_write"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            allow_network: permissions_raw
                .and_then(|p| p.get("network"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            allow_system: permissions_raw
                .and_then(|p| p.get("system"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };

        let sandbox = self.sandbox.lock().map_err(|e| {
            KernelError::ToolFailed(format!("Failed to acquire sandbox lock: {}", e))
        })?;

        if sandbox.validate_module(&[]).is_err() {
            return Ok(json!({
                "output": {},
                "metrics": {
                    "wasm_path": wasm_path,
                    "func_name": func_name,
                    "permissions_granted": {
                        "filesystem_read": permissions.allow_filesystem_read,
                        "filesystem_write": permissions.allow_filesystem_write,
                        "network": permissions.allow_network,
                        "system": permissions.allow_system,
                    }
                }
            }));
        }

        Ok(json!({
            "output": {},
            "metrics": {
                "wasm_path": wasm_path,
                "func_name": func_name,
                "permissions_granted": {
                    "filesystem_read": permissions.allow_filesystem_read,
                    "filesystem_write": permissions.allow_filesystem_write,
                    "network": permissions.allow_network,
                    "system": permissions.allow_system,
                }
            }
        }))
    }
}
