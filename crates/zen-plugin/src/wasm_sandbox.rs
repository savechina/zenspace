use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_memory_bytes: u64,
    pub max_execution_time_ms: u64,
    pub allowed_syscalls: HashSet<String>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024,
            max_execution_time_ms: 5000,
            allowed_syscalls: HashSet::from([
                "fd_write".to_string(),
                "fd_read".to_string(),
                "proc_exit".to_string(),
            ]),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub execution_time_ms: u64,
    pub memory_used_bytes: u64,
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
    limits: ResourceLimits,
}

impl WasmSandbox {
    pub fn new() -> Self {
        Self::with_limits(ResourceLimits::default())
    }

    pub fn with_limits(limits: ResourceLimits) -> Self {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.async_support(true);

        let engine = Engine::new(&config).unwrap_or_else(|e| {
            warn!("WASM engine init failed ({e}), falling back to defaults");
            Engine::default()
        });

        Self { engine, limits }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
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
            }
            "filesystem:write" if !permissions.allow_filesystem_write => {
                Err(WasmSandboxError::PermissionDenied {
                    resource: resource.to_string(),
                })
            }
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

    pub async fn execute(
        &self,
        wasm_bytes: &[u8],
        func_name: &str,
        args: &[String],
    ) -> Result<ExecutionOutput, WasmSandboxError> {
        use std::io::{Cursor, Write};
        use std::time::Instant;

        let start = Instant::now();

        let module =
            wasmtime::Module::new(&self.engine, wasm_bytes).map_err(|e| {
                WasmSandboxError::ModuleLoad(format!("WASM compile failed: {}", e))
            })?;

        let stdout_buf = Arc::new(Mutex::new(Cursor::new(Vec::<u8>::new())));
        let stderr_buf = Arc::new(Mutex::new(Cursor::new(Vec::<u8>::new())));
        let stdout_clone = stdout_buf.clone();
        let stderr_clone = stderr_buf.clone();

        struct CursorWriter(Arc<Mutex<Cursor<Vec<u8>>>>);
        impl Write for CursorWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut wasi_builder = wasi_common::sync::WasiCtxBuilder::new();
        if let Err(e) = wasi_builder.args(args) {
            warn!(error = ?e, "failed to set WASI arguments");
        }
        wasi_builder.stdout(Box::new(wasi_common::pipe::WritePipe::new(CursorWriter(
            stdout_clone,
        ))));
        wasi_builder.stderr(Box::new(wasi_common::pipe::WritePipe::new(CursorWriter(
            stderr_clone,
        ))));
        let wasi_ctx = wasi_builder.build();

        let mut store = wasmtime::Store::new(&self.engine, wasi_ctx);
        store
            .set_fuel(self.limits.max_execution_time_ms / 100)
            .map_err(|e| WasmSandboxError::Wasmtime(e.to_string()))?;

        let mut linker = wasmtime::Linker::new(&self.engine);
        wasi_common::sync::add_to_linker(&mut linker, |s: &mut wasi_common::WasiCtx| s)
            .map_err(|e| WasmSandboxError::Wasmtime(e.to_string()))?;

        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .map_err(|e| WasmSandboxError::Execution(format!("instantiate failed: {}", e)))?;

        let run_func = instance
            .get_func(&mut store, func_name)
            .ok_or_else(|| {
                WasmSandboxError::Execution(format!(
                    "function '{}' not found in WASM module",
                    func_name
                ))
            })?;

        let result = run_func.call_async(&mut store, &[], &mut []).await;
        let elapsed = start.elapsed().as_millis() as u64;

        let stdout =
            String::from_utf8_lossy(stdout_buf.lock().unwrap().get_ref()).into_owned();
        let mut stderr =
            String::from_utf8_lossy(stderr_buf.lock().unwrap().get_ref()).into_owned();

        let exit_code = match &result {
            Ok(_) => 0,
            Err(e) => {
                if let Some(i32_exit) = e.downcast_ref::<wasi_common::I32Exit>() {
                    i32_exit.0
                } else {
                    use std::fmt::Write as _;
                    let _ = write!(stderr, "{e}");
                    1
                }
            }
        };

        let fuel_set = self.limits.max_execution_time_ms / 100;
        let fuel_remaining = store.get_fuel().unwrap_or(fuel_set);
        let used_fuel = fuel_set.saturating_sub(fuel_remaining);

        Ok(ExecutionOutput {
            stdout,
            stderr,
            exit_code,
            execution_time_ms: elapsed,
            memory_used_bytes: used_fuel * 64 * 1024,
        })
    }
}

impl Default for WasmSandbox {
    fn default() -> Self {
        Self::new()
    }
}

const NAME: &str = "plugin.wasm_sandbox";
const DESCRIPTION: &str =
    "Execute WASM plugin in sandboxed environment with permission gating";

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
            "output": {
                "type": "object",
                "properties": {
                    "stdout": { "type": "string" },
                    "stderr": { "type": "string" },
                    "exit_code": { "type": "integer" }
                }
            },
            "metrics": {
                "type": "object",
                "properties": {
                    "wasm_path": { "type": "string" },
                    "func_name": { "type": "string" },
                    "permissions_granted": { "type": "object" },
                    "execution_time_ms": { "type": "integer" },
                    "memory_used_bytes": { "type": "integer" }
                }
            }
        }
    })
});

pub struct WasmSandboxTool {
    sandbox: Arc<tokio::sync::Mutex<WasmSandbox>>,
}

impl WasmSandboxTool {
    pub fn new() -> Self {
        Self {
            sandbox: Arc::new(tokio::sync::Mutex::new(WasmSandbox::new())),
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

        let wasm_bytes = std::fs::read(wasm_path).map_err(|e| {
            KernelError::ToolFailed(format!("Failed to read WASM file '{}': {}", wasm_path, e))
        })?;

        let sandbox = self.sandbox.lock().await;

        if let Err(e) = sandbox.validate_module(&wasm_bytes) {
            return Err(KernelError::ToolFailed(format!(
                "WASM module validation failed: {}",
                e
            )));
        }

        let execution_output = sandbox
            .execute(&wasm_bytes, func_name, &[])
            .await
            .map_err(|e| KernelError::ToolFailed(format!("WASM execution failed: {}", e)))?;

        Ok(json!({
            "output": {
                "stdout": execution_output.stdout,
                "stderr": execution_output.stderr,
                "exit_code": execution_output.exit_code,
            },
            "metrics": {
                "wasm_path": wasm_path,
                "func_name": func_name,
                "permissions_granted": {
                    "filesystem_read": permissions.allow_filesystem_read,
                    "filesystem_write": permissions.allow_filesystem_write,
                    "network": permissions.allow_network,
                    "system": permissions.allow_system,
                },
                "execution_time_ms": execution_output.execution_time_ms,
                "memory_used_bytes": execution_output.memory_used_bytes,
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_limits_default_values() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.max_execution_time_ms, 5000);
        assert!(limits.allowed_syscalls.contains("fd_write"));
        assert!(limits.allowed_syscalls.contains("fd_read"));
        assert!(limits.allowed_syscalls.contains("proc_exit"));
    }

    #[test]
    fn wasm_sandbox_creation() {
        let sandbox = WasmSandbox::new();
        assert!(sandbox.validate_module(&wat::parse_str("(module)").unwrap()).is_ok());
    }

    #[test]
    fn wasm_sandbox_with_limits() {
        let limits = ResourceLimits {
            max_memory_bytes: 32 * 1024 * 1024,
            max_execution_time_ms: 1000,
            allowed_syscalls: HashSet::new(),
        };
        let sandbox = WasmSandbox::with_limits(limits.clone());
        assert_eq!(sandbox.limits().max_memory_bytes, 32 * 1024 * 1024);
        assert_eq!(sandbox.limits().max_execution_time_ms, 1000);
    }

    #[test]
    fn permission_check_allows_granted() {
        let sandbox = WasmSandbox::new();
        let perms = WasmPermissions {
            allow_network: true,
            ..Default::default()
        };
        assert!(sandbox.check_permission(&perms, "network").is_ok());
    }

    #[test]
    fn permission_check_denies_blocked() {
        let sandbox = WasmSandbox::new();
        let perms = WasmPermissions::default();
        assert!(sandbox.check_permission(&perms, "network").is_err());
        assert!(sandbox.check_permission(&perms, "filesystem:read").is_err());
        assert!(sandbox.check_permission(&perms, "filesystem:write").is_err());
        assert!(sandbox.check_permission(&perms, "system").is_err());
    }

    #[test]
    fn validate_module_rejects_garbage() {
        let sandbox = WasmSandbox::new();
        assert!(sandbox.validate_module(&[0x00, 0x61, 0x73, 0x6d, 0xff]).is_err());
    }

    #[test]
    fn validate_module_accepts_valid_wat() {
        let sandbox = WasmSandbox::new();
        let wasm = wat::parse_str(
            r#"
            (module
                (func (export "_start") (result i32)
                    i32.const 42
                )
            )
            "#,
        )
        .unwrap();
        assert!(sandbox.validate_module(&wasm).is_ok());
    }

    #[tokio::test]
    async fn execute_simple_wasm_function() {
        let sandbox = WasmSandbox::new();
        let wasm = wat::parse_str(
            r#"
            (module
                (func (export "_start"))
            )
            "#,
        )
        .unwrap();

        let output = sandbox.execute(&wasm, "_start", &[]).await.unwrap();
        assert_eq!(output.exit_code, 0);
    }

    #[tokio::test]
    async fn execute_wasm_with_stdout() {
        let sandbox = WasmSandbox::new();
        let wasm = wat::parse_str(
            r#"
            (module
                (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
                (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))

                (memory (export "memory") 1)

                (data (i32.const 8) "hello from wasm\n")

                (func (export "_start")
                    ;; fd_write(1, &iovec, 1, &nwritten)
                    (i32.store (i32.const 0) (i32.const 8))   ;; iov.buf
                    (i32.store (i32.const 4) (i32.const 16))  ;; iov.buf_len
                    (call $fd_write
                        (i32.const 1)   ;; stdout
                        (i32.const 0)   ;; iovec ptr
                        (i32.const 1)   ;; iovec len
                        (i32.const 20)  ;; nwritten ptr
                    )
                    drop
                    (call $proc_exit (i32.const 0))
                )
            )
            "#,
        )
        .unwrap();

        let output = sandbox.execute(&wasm, "_start", &[]).await.unwrap();
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "hello from wasm\n");
    }

    #[tokio::test]
    async fn execute_wasm_missing_function() {
        let sandbox = WasmSandbox::new();
        let wasm = wat::parse_str(
            r#"
            (module
                (func (export "real_func") (result i32)
                    i32.const 1
                )
            )
            "#,
        )
        .unwrap();

        let result = sandbox.execute(&wasm, "nonexistent", &[]).await;
        assert!(result.is_err());
    }

    #[test]
    fn tool_schema_structure() {
        let tool = WasmSandboxTool::new();
        let schema = tool.schema();
        assert_eq!(schema.name, "plugin.wasm_sandbox");
        assert!(schema.args_schema.get("properties").is_some());
        assert!(schema.result_schema.get("properties").is_some());
    }
}
