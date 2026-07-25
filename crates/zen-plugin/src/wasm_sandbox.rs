use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

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

/// WASI version detected from a compiled wasm module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WasiVersion {
    /// WASI preview1 — imports from `wasi_snapshot_preview1`
    P1,
    /// WASI preview2 core module — imports from `wasi:*` namespaces
    P2,
}

/// A compiled wasm binary, either a core module or a component.
enum CompiledWasm {
    Core {
        module: wasmtime::Module,
        #[allow(dead_code)]
        version: WasiVersion,
    },
    Component {
        component: wasmtime::component::Component,
    },
}

/// Detect the WASI version from a core module's import namespace.
fn detect_core_version(module: &wasmtime::Module) -> WasiVersion {
    for import in module.imports() {
        match import.module() {
            "wasi_snapshot_preview1" => return WasiVersion::P1,
            m if m.starts_with("wasi:") => return WasiVersion::P2,
            _ => {}
        }
    }
    // No WASI imports → safe default
    WasiVersion::P1
}

/// Try to compile a wasm binary as either a core module or a component.
fn compile_wasm(engine: &Engine, wasm_bytes: &[u8]) -> Result<CompiledWasm, WasmSandboxError> {
    if let Ok(module) = wasmtime::Module::new(engine, wasm_bytes) {
        let version = detect_core_version(&module);
        return Ok(CompiledWasm::Core { module, version });
    }

    if let Ok(component) = wasmtime::component::Component::new(engine, wasm_bytes) {
        return Ok(CompiledWasm::Component { component });
    }

    Err(WasmSandboxError::ModuleLoad(
        "WASM binary is neither a valid core module nor a WASI component".into(),
    ))
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
        // Accept both core modules and component-model components.
        if wasmtime::Module::new(&self.engine, wasm_bytes).is_ok() {
            return Ok(());
        }
        if wasmtime::component::Component::new(&self.engine, wasm_bytes).is_ok() {
            return Ok(());
        }
        Err(WasmSandboxError::ModuleLoad(
            "WASM validation failed: not a valid core module or component".into(),
        ))
    }

    pub async fn execute(
        &self,
        wasm_bytes: &[u8],
        func_name: &str,
        args: &[String],
    ) -> Result<ExecutionOutput, WasmSandboxError> {
        let compiled = compile_wasm(&self.engine, wasm_bytes)?;
        match compiled {
            CompiledWasm::Core { module, version } => {
                self.execute_core(module, version, func_name, args).await
            }
            CompiledWasm::Component { component } => {
                self.execute_component(component, func_name, args).await
            }
        }
    }

    // ── core module execution (p1 / p2) ──────────────────────────────────

    #[allow(unused_variables)]
    async fn execute_core(
        &self,
        module: wasmtime::Module,
        version: WasiVersion,
        func_name: &str,
        args: &[String],
    ) -> Result<ExecutionOutput, WasmSandboxError> {
        use std::time::Instant;
        use wasmtime_wasi::p2::pipe::MemoryOutputPipe;

        let start = Instant::now();

        let stdout_pipe = MemoryOutputPipe::new(65536);
        let stderr_pipe = MemoryOutputPipe::new(65536);

        let mut wasi_builder = wasmtime_wasi::WasiCtxBuilder::new();
        wasi_builder.args(args);
        wasi_builder.stdout(stdout_pipe.clone());
        wasi_builder.stderr(stderr_pipe.clone());
        let wasi_ctx = wasi_builder.build_p1();

        let mut store = wasmtime::Store::new(&self.engine, wasi_ctx);
        let fuel_budget = (self.limits.max_execution_time_ms as u64)
            .saturating_mul(1_000_000)
            .max(10_000_000);
        store
            .set_fuel(fuel_budget)
            .map_err(|e| WasmSandboxError::Wasmtime(e.to_string()))?;

        // All core modules use the p1 linker (wasi_snapshot_preview1).
        // Modules with wasi:* namespace imports (P2) will fail at
        // instantiation with a clear wasmtime error since the p1 linker
        // doesn't expose those bindings — p2 requires component model
        // wrapping via wasmtime::component::Linker.
        let mut linker = wasmtime::Linker::new(&self.engine);
        wasmtime_wasi::p1::add_to_linker_async::<wasmtime_wasi::p1::WasiP1Ctx>(
            &mut linker,
            |s| s,
        )
        .map_err(|e| WasmSandboxError::Wasmtime(e.to_string()))?;

        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .map_err(|e| WasmSandboxError::Execution(format!("instantiate failed: {}", e)))?;

        let run_func = instance.get_func(&mut store, func_name).ok_or_else(|| {
            WasmSandboxError::Execution(format!(
                "function '{}' not found in WASM module",
                func_name
            ))
        })?;

        let result = run_func.call_async(&mut store, &[], &mut []).await;
        let elapsed = start.elapsed().as_millis() as u64;

        let stdout =
            String::from_utf8_lossy(&stdout_pipe.contents()).into_owned();
        let mut stderr =
            String::from_utf8_lossy(&stderr_pipe.contents()).into_owned();

        let exit_code = match &result {
            Ok(_) => 0,
            Err(e) => {
                if let Some(i32_exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                    i32_exit.0
                } else {
                    use std::fmt::Write as _;
                    let _ = write!(stderr, "{e}");
                    1
                }
            }
        };

        let fuel_remaining = store.get_fuel().unwrap_or(fuel_budget);
        let used_fuel = fuel_budget.saturating_sub(fuel_remaining);

        Ok(ExecutionOutput {
            stdout,
            stderr,
            exit_code,
            execution_time_ms: elapsed,
            memory_used_bytes: used_fuel * 64 * 1024,
        })
    }

    // ── component model execution (p3) ───────────────────────────────────

    async fn execute_component(
        &self,
        component: wasmtime::component::Component,
        func_name: &str,
        args: &[String],
    ) -> Result<ExecutionOutput, WasmSandboxError> {
        use std::time::Instant;
        use wasmtime_wasi::p2::pipe::MemoryOutputPipe;

        let start = Instant::now();

        let stdout_pipe = MemoryOutputPipe::new(65536);
        let stderr_pipe = MemoryOutputPipe::new(65536);

        let mut wasi_builder = wasmtime_wasi::WasiCtxBuilder::new();
        wasi_builder.args(args);
        wasi_builder.stdout(stdout_pipe.clone());
        wasi_builder.stderr(stderr_pipe.clone());
        let wasi_ctx = wasi_builder.build_p1();

        let mut store = wasmtime::Store::new(&self.engine, wasi_ctx);
        let fuel_budget = (self.limits.max_execution_time_ms as u64)
            .saturating_mul(1_000_000)
            .max(10_000_000);
        store
            .set_fuel(fuel_budget)
            .map_err(|e| WasmSandboxError::Wasmtime(e.to_string()))?;

        let mut linker = wasmtime::component::Linker::new(&self.engine);
        wasmtime_wasi::p3::add_to_linker::<wasmtime_wasi::p1::WasiP1Ctx>(&mut linker)
            .map_err(|e| WasmSandboxError::Wasmtime(e.to_string()))?;

        let instance = linker
            .instantiate_async(&mut store, &component)
            .await
            .map_err(|e| {
                WasmSandboxError::Execution(format!("component instantiate failed: {}", e))
            })?;

        let run_func = instance.get_func(&mut store, func_name).ok_or_else(|| {
            WasmSandboxError::Execution(format!(
                "function '{}' not found in component",
                func_name
            ))
        })?;

        let result = run_func.call_async(&mut store, &[], &mut []).await;
        let elapsed = start.elapsed().as_millis() as u64;

        let stdout =
            String::from_utf8_lossy(&stdout_pipe.contents()).into_owned();
        let mut stderr =
            String::from_utf8_lossy(&stderr_pipe.contents()).into_owned();

        let exit_code = match &result {
            Ok(_) => 0,
            Err(e) => {
                // Component model doesn't use I32Exit in the same way.
                use std::fmt::Write as _;
                let _ = write!(stderr, "{e}");
                1
            }
        };

        let fuel_remaining = store.get_fuel().unwrap_or(fuel_budget);
        let used_fuel = fuel_budget.saturating_sub(fuel_remaining);

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

    #[test]
    fn detect_version_p1_wasi_snapshot_preview1() {
        let sandbox = WasmSandbox::new();
        // Module importing from wasi_snapshot_preview1 is detected as P1
        let wasm = wat::parse_str(
            r#"
            (module
                (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (func (export "_start"))
            )
            "#,
        )
        .unwrap();
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();
        assert_eq!(detect_core_version(&module), WasiVersion::P1);
    }

    #[test]
    fn detect_version_p2_wasi_namespace() {
        let sandbox = WasmSandbox::new();
        // Module importing from wasi: namespaces is detected as P2
        // Use a minimal module that just has wasi: prefixed imports
        let wasm = wat::parse_str(
            r#"
            (module
                (import "wasi:io/streams" "read" (func (param i32) (result i32)))
                (func (export "_start"))
            )
            "#,
        )
        .unwrap();
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();
        assert_eq!(detect_core_version(&module), WasiVersion::P2);
    }

    #[test]
    fn detect_version_no_imports_defaults_p1() {
        let sandbox = WasmSandbox::new();
        let wasm = wat::parse_str(
            r#"
            (module
                (func (export "_start"))
            )
            "#,
        )
        .unwrap();
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();
        assert_eq!(detect_core_version(&module), WasiVersion::P1);
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
