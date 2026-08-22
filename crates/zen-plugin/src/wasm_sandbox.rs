use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tracing::warn;
use wasmtime::{Config, Engine, ResourceLimiter};
use wasmtime_wasi::{DirPerms, FilePerms};

use zen_core::sandbox::{is_env_file, is_metadata_path};

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

/// Declared filesystem capabilities for a plugin (manifest `permissions`).
///
/// Since TODO-011 these booleans are REAL capabilities backed by WASI
/// preopens — no longer "policy forward declarations":
/// - `allow_filesystem_read` → the workspace root is preopened READ-ONLY
///   at guest path `/workspace`.
/// - `allow_filesystem_write` → a per-plugin scratch directory
///   (`.zen/plugin-data/<plugin-id>/`) is preopened READ-WRITE at guest
///   path `/scratch`. The workspace root itself is NEVER writable:
///   `state.db` and `.zen/` config stay structurally unwritable by plugins.
#[derive(Debug, Clone, Default)]
pub struct WasmPermissions {
    pub allow_filesystem_read: bool,
    pub allow_filesystem_write: bool,
    pub allow_network: bool,
    pub allow_system: bool,
}

/// Resolved filesystem capabilities for one plugin invocation (TODO-011).
///
/// Built from the manifest permissions + the workspace root by
/// [`WasmFsContext::resolve`], which canonicalizes every preopen target
/// (FR-024 approach: reject symlink escapes and protected-path landings)
/// and fails closed when a capability is declared but cannot be honored.
///
/// Capability model (review decision 2026-08-19):
///
/// ```text
/// manifest permissions          preopen (guest → host)
/// ─────────────────────         ─────────────────────────────────────
/// (none)                        — zero preopens (argv→stdout only)
/// filesystem_read               /workspace → <root>       (READ-ONLY)
/// filesystem_write              /scratch   → <root>/.zen/plugin-data/<id>/
/// filesystem_read + write       both of the above
///
/// scratch is created host-side; copy-out to the real workspace is
/// host-mediated (TODO: zen_copy_out host function).
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WasmFsContext {
    /// Canonicalized workspace root for the read-only `/workspace` preopen.
    pub workspace_root: Option<PathBuf>,
    /// Canonicalized per-plugin scratch dir for the read-write `/scratch`
    /// preopen. Always inside `<workspace_root>/.zen/plugin-data/<id>/`.
    pub scratch_dir: Option<PathBuf>,
}

impl WasmFsContext {
    /// Resolve manifest permission strings into preopen targets.
    ///
    /// Fails closed when:
    /// - a filesystem capability is declared but no workspace root exists;
    /// - the plugin id contains characters outside `[a-z0-9_-]`
    ///   (defense-in-depth against path traversal — FR-050 also enforces
    ///   this at registry load);
    /// - the workspace root cannot be canonicalized or resolves onto a
    ///   protected metadata/env path (e.g. the root IS `~/.ssh`);
    /// - the scratch path already exists but canonicalizes outside the
    ///   workspace root (symlink escape — FR-024).
    ///
    /// Note: the scratch directory lives inside `.zen/` by design (the
    /// sanctioned writable exception); agent-facing fs.* tools still treat
    /// `.zen/` as protected — the scratch is reachable only through the
    /// plugin's own preopen, never via fs.* tools.
    pub fn resolve(
        plugin_id: &str,
        permissions: &[String],
        workspace_root: Option<&Path>,
    ) -> Result<Self, WasmSandboxError> {
        let allow_read = permissions.iter().any(|p| p == "filesystem_read");
        let allow_write = permissions.iter().any(|p| p == "filesystem_write");

        if !allow_read && !allow_write {
            // Default manifest: zero preopens, exactly the pre-TODO-011
            // argv→stdout compute model.
            return Ok(Self::default());
        }

        // FR-050 charset (defense-in-depth: registry already enforces this).
        if !plugin_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(WasmSandboxError::PermissionDenied {
                resource: format!("invalid plugin id for fs capability: {plugin_id:?}"),
            });
        }

        let Some(root) = workspace_root else {
            return Err(WasmSandboxError::PermissionDenied {
                resource: "filesystem capability declared but no workspace root available"
                    .to_string(),
            });
        };

        // Canonicalize the root (fails closed if missing/unreadable).
        let canonical_root =
            root.canonicalize()
                .map_err(|e| WasmSandboxError::PermissionDenied {
                    resource: format!(
                        "workspace root {} cannot be canonicalized: {}",
                        root.display(),
                        e
                    ),
                })?;

        // The root itself must not BE a protected path (e.g. ~/.ssh, .env).
        if is_metadata_path(&canonical_root) || is_env_file(&canonical_root) {
            return Err(WasmSandboxError::PermissionDenied {
                resource: format!(
                    "workspace root {} is a protected path",
                    canonical_root.display()
                ),
            });
        }

        let workspace_root_field = allow_read.then(|| canonical_root.clone());

        let scratch_dir = if allow_write {
            let scratch = canonical_root
                .join(".zen")
                .join("data")
                .join("plugin")
                .join(plugin_id);
            // If the path already exists, verify it canonicalizes INSIDE
            // the root — a symlinked `.zen/data/plugin/<id>` pointing
            // outside is an escape and must be rejected (FR-024).
            if scratch.exists() {
                let canon =
                    scratch
                        .canonicalize()
                        .map_err(|e| WasmSandboxError::PermissionDenied {
                            resource: format!(
                                "scratch dir {} cannot be canonicalized: {}",
                                scratch.display(),
                                e
                            ),
                        })?;
                if !canon.starts_with(&canonical_root) {
                    return Err(WasmSandboxError::PermissionDenied {
                        resource: format!(
                            "scratch dir {} resolves outside the workspace root (symlink escape)",
                            scratch.display()
                        ),
                    });
                }
            }
            std::fs::create_dir_all(&scratch).map_err(|e| WasmSandboxError::PermissionDenied {
                resource: format!("scratch dir {} cannot be created: {}", scratch.display(), e),
            })?;
            // Re-canonicalize after creation and re-verify containment
            // (guards against the path being swapped mid-resolution).
            let canon = scratch
                .canonicalize()
                .map_err(|e| WasmSandboxError::PermissionDenied {
                    resource: format!(
                        "scratch dir {} cannot be canonicalized: {}",
                        scratch.display(),
                        e
                    ),
                })?;
            if !canon.starts_with(&canonical_root) {
                return Err(WasmSandboxError::PermissionDenied {
                    resource: format!(
                        "scratch dir {} resolves outside the workspace root (symlink escape)",
                        scratch.display()
                    ),
                });
            }
            Some(canon)
        } else {
            None
        };

        Ok(Self {
            workspace_root: workspace_root_field,
            scratch_dir,
        })
    }
}

/// Resource limiter enforcing the sandbox's memory cap (FR-030).
///
/// Tracks the largest memory size requested so `memory_used_bytes` can be
/// reported post-execution without walking the instance's exports.
#[derive(Default)]
struct StoreLimits {
    max_memory_bytes: usize,
    last_desired_bytes: usize,
}

impl StoreLimits {
    fn new(max_memory_bytes: usize) -> Self {
        Self {
            max_memory_bytes,
            last_desired_bytes: 0,
        }
    }
}

impl ResourceLimiter for StoreLimits {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.last_desired_bytes = desired;
        Ok(desired <= self.max_memory_bytes)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true) // tables unrestricted for now
    }
}

/// Store state: the WASI context plus the resource limiter. The limiter must
/// live inside the store's data so `Store::limiter` can reach it.
struct SandboxState {
    wasi: wasmtime_wasi::p1::WasiP1Ctx,
    limits: StoreLimits,
}

impl wasmtime_wasi::WasiView for SandboxState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        self.wasi.ctx()
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
    policy: WasmPermissions,
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

        Self {
            engine,
            limits,
            policy: WasmPermissions::default(),
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    /// Configure the permission policy this sandbox enforces (FR-029).
    /// Defaults to deny-all; invocations may only declare permissions the
    /// policy grants.
    pub fn with_policy(mut self, policy: WasmPermissions) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> &WasmPermissions {
        &self.policy
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

    /// Enumerate the exported functions of a core module that are callable
    /// through [`Self::execute`] (T089). Excludes `_start` and other
    /// underscore-prefixed internal exports, and keeps only `() -> ()`
    /// signatures — the sole shape the `execute` path invokes (arguments
    /// reach the module as WASI argv, not wasm params).
    pub fn exported_functions(&self, wasm_bytes: &[u8]) -> Result<Vec<String>, WasmSandboxError> {
        let module = wasmtime::Module::new(&self.engine, wasm_bytes).map_err(|e| {
            WasmSandboxError::ModuleLoad(format!("failed to compile core module: {}", e))
        })?;

        let mut names = Vec::new();
        for export in module.exports() {
            if let wasmtime::ExternType::Func(func_type) = export.ty()
                && !export.name().starts_with('_')
                && func_type.params().next().is_none()
                && func_type.results().next().is_none()
            {
                names.push(export.name().to_string());
            }
        }
        Ok(names)
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
                self.execute_core(module, version, func_name, args, &WasmFsContext::default())
                    .await
            }
            CompiledWasm::Component { component } => {
                self.execute_component(component, func_name, args).await
            }
        }
    }

    /// Execute a function from an already-compiled core module (FR-051).
    ///
    /// `wasmtime::Module` compilation costs ~10-100ms; callers that invoke
    /// the same module repeatedly (e.g. [`crate::plugin_wasm::WasmPluginTool`],
    /// which caches the `Module` in a `OnceLock` per tool instance) reuse
    /// this entry point so compilation happens once per tool instance
    /// instead of once per invocation. `Module` is cheaply cloneable
    /// (Arc-backed internally).
    pub async fn execute_module(
        &self,
        module: &wasmtime::Module,
        func_name: &str,
        args: &[String],
    ) -> Result<ExecutionOutput, WasmSandboxError> {
        self.execute_module_with_fs(module, func_name, args, &WasmFsContext::default())
            .await
    }

    /// [`Self::execute_module`] with filesystem capabilities (TODO-011):
    /// `fs` carries the preopen targets resolved (and validated) by
    /// [`WasmFsContext::resolve`]. An empty context yields zero preopens —
    /// identical to [`Self::execute_module`].
    pub async fn execute_module_with_fs(
        &self,
        module: &wasmtime::Module,
        func_name: &str,
        args: &[String],
        fs: &WasmFsContext,
    ) -> Result<ExecutionOutput, WasmSandboxError> {
        let version = detect_core_version(module);
        self.execute_core(module.clone(), version, func_name, args, fs)
            .await
    }

    // ── core module execution (p1 / p2) ──────────────────────────────────

    async fn execute_core(
        &self,
        module: wasmtime::Module,
        _version: WasiVersion,
        func_name: &str,
        args: &[String],
        fs: &WasmFsContext,
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
        // TODO-011: filesystem capabilities as WASI preopens. Targets were
        // canonicalized by `WasmFsContext::resolve`; cap-std (wasmtime's
        // preopen backend) additionally confines path resolution inside each
        // preopened dir, so in-sandbox symlinks cannot escape either.
        if let Some(root) = &fs.workspace_root {
            wasi_builder
                .preopened_dir(root, "/workspace", DirPerms::READ, FilePerms::READ)
                .map_err(|e| {
                    WasmSandboxError::Execution(format!("workspace preopen failed: {}", e))
                })?;
        }
        if let Some(scratch) = &fs.scratch_dir {
            wasi_builder
                .preopened_dir(
                    scratch,
                    "/scratch",
                    DirPerms::READ | DirPerms::MUTATE,
                    FilePerms::READ | FilePerms::WRITE,
                )
                .map_err(|e| {
                    WasmSandboxError::Execution(format!("scratch preopen failed: {}", e))
                })?;
        }
        let wasi_ctx = wasi_builder.build_p1();

        let mut store = wasmtime::Store::new(
            &self.engine,
            SandboxState {
                wasi: wasi_ctx,
                limits: StoreLimits::new(self.limits.max_memory_bytes as usize),
            },
        );
        store.limiter(|state| &mut state.limits);
        let fuel_budget = self
            .limits
            .max_execution_time_ms
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
        wasmtime_wasi::p1::add_to_linker_async::<SandboxState>(&mut linker, |s| &mut s.wasi)
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

        let stdout = String::from_utf8_lossy(&stdout_pipe.contents()).into_owned();
        let mut stderr = String::from_utf8_lossy(&stderr_pipe.contents()).into_owned();

        let exit_code = match &result {
            Ok(_) => 0,
            Err(e) => {
                if let Some(i32_exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                    i32_exit.0
                } else {
                    use std::fmt::Write as _;
                    for cause in e.chain() {
                        let _ = write!(stderr, "\n[cause] {cause}");
                    }
                    let _ = write!(stderr, "{e}");
                    1
                }
            }
        };

        let memory_used_bytes = instance
            .get_memory(&mut store, "memory")
            .map(|m| m.data_size(&store) as u64)
            .unwrap_or_else(|| store.data().limits.last_desired_bytes as u64);

        Ok(ExecutionOutput {
            stdout,
            stderr,
            exit_code,
            execution_time_ms: elapsed,
            memory_used_bytes,
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

        let mut store = wasmtime::Store::new(
            &self.engine,
            SandboxState {
                wasi: wasi_ctx,
                limits: StoreLimits::new(self.limits.max_memory_bytes as usize),
            },
        );
        store.limiter(|state| &mut state.limits);
        let fuel_budget = self
            .limits
            .max_execution_time_ms
            .saturating_mul(1_000_000)
            .max(10_000_000);
        store
            .set_fuel(fuel_budget)
            .map_err(|e| WasmSandboxError::Wasmtime(e.to_string()))?;

        let mut linker = wasmtime::component::Linker::new(&self.engine);
        wasmtime_wasi::p3::add_to_linker::<SandboxState>(&mut linker)
            .map_err(|e| WasmSandboxError::Wasmtime(e.to_string()))?;

        let instance = linker
            .instantiate_async(&mut store, &component)
            .await
            .map_err(|e| {
                WasmSandboxError::Execution(format!("component instantiate failed: {}", e))
            })?;

        let run_func = instance.get_func(&mut store, func_name).ok_or_else(|| {
            WasmSandboxError::Execution(format!("function '{}' not found in component", func_name))
        })?;

        let result = run_func.call_async(&mut store, &[], &mut []).await;
        let elapsed = start.elapsed().as_millis() as u64;

        let stdout = String::from_utf8_lossy(&stdout_pipe.contents()).into_owned();
        let mut stderr = String::from_utf8_lossy(&stderr_pipe.contents()).into_owned();

        let exit_code = match &result {
            Ok(_) => 0,
            Err(e) => {
                // Component model doesn't use I32Exit in the same way.
                use std::fmt::Write as _;
                let _ = write!(stderr, "{e}");
                1
            }
        };

        let memory_used_bytes = store.data().limits.last_desired_bytes as u64;

        Ok(ExecutionOutput {
            stdout,
            stderr,
            exit_code,
            execution_time_ms: elapsed,
            memory_used_bytes,
        })
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

/// `plugin.wasm_sandbox` — execute an arbitrary `.wasm` file under the
/// sandbox policy.
///
/// Deliberately grants ZERO preopens regardless of the permissions in the
/// invocation args: filesystem capabilities are plugin-manifest-driven only
/// (they require a workspace root, which this generic tool does not have).
/// Fail-closed by construction; use plugin entries for fs capabilities.
pub struct WasmSandboxTool {
    sandbox: Arc<tokio::sync::Mutex<WasmSandbox>>,
}

impl WasmSandboxTool {
    pub fn new() -> Self {
        Self {
            sandbox: Arc::new(tokio::sync::Mutex::new(WasmSandbox::new())),
        }
    }

    pub fn with_sandbox(sandbox: WasmSandbox) -> Self {
        Self {
            sandbox: Arc::new(tokio::sync::Mutex::new(sandbox)),
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

        // FR-029: declared permissions must be a subset of the sandbox policy.
        // check_permission is the load-bearing gate on every invoke.
        for (resource, declared) in [
            ("filesystem:read", permissions.allow_filesystem_read),
            ("filesystem:write", permissions.allow_filesystem_write),
            ("network", permissions.allow_network),
            ("system", permissions.allow_system),
        ] {
            if declared {
                sandbox
                    .check_permission(sandbox.policy(), resource)
                    .map_err(|e| {
                        KernelError::ToolFailed(format!("WASM permission denied: {}", e))
                    })?;
            }
        }

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
pub(crate) mod tests {
    use super::*;

    /// Test wat module: opens `path` (CREAT) relative to preopen fd
    /// `preopen_fd` and writes "hi\n" to it. On open failure the process
    /// exits with the path_open errno so test failures are diagnosable;
    /// callers additionally assert on filesystem state (did the write
    /// LAND where policy allows).
    #[cfg(test)]
    pub(crate) fn write_file_wat(preopen_fd: i32, path: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"
            (module
                (import "wasi_snapshot_preview1" "path_open"
                    (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
                (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                (import "wasi_snapshot_preview1" "proc_exit"
                    (func $proc_exit (param i32)))
                (memory (export "memory") 1)
                (data (i32.const 8) "{path}")
                (data (i32.const 64) "hi\n")
                (func (export "run")
                    (local $errno i32)
                    (i32.store (i32.const 96) (i32.const 64))
                    (i32.store (i32.const 100) (i32.const 3))
                    (local.set $errno (call $path_open
                        (i32.const {preopen_fd})
                        (i32.const 0)
                        (i32.const 8)
                        (i32.const {path_len})
                        (i32.const 1)
                        (i64.const 64)
                        (i64.const 0)
                        (i32.const 0)
                        (i32.const 200)))
                    (if (i32.ne (local.get $errno) (i32.const 0))
                        (then (call $proc_exit (local.get $errno))))
                    (drop (call $fd_write
                        (i32.load (i32.const 200))
                        (i32.const 96)
                        (i32.const 1)
                        (i32.const 104)))
                )
            )
            "#,
            path_len = path.len()
        ))
        .unwrap()
    }

    // ── WasmFsContext::resolve (TODO-011) ────────────────────────────────

    #[test]
    fn fs_context_no_permissions_yields_zero_preopens() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = WasmFsContext::resolve("demo", &[], Some(dir.path())).unwrap();
        assert_eq!(ctx, WasmFsContext::default());
    }

    #[test]
    fn fs_context_capability_without_root_fails_closed() {
        let err =
            WasmFsContext::resolve("demo", &["filesystem_read".to_string()], None).unwrap_err();
        assert!(err.to_string().contains("no workspace root"), "got: {err}");
    }

    #[test]
    fn fs_context_read_permission_grants_ro_root_only() {
        let dir = tempfile::tempdir().unwrap();
        let ctx =
            WasmFsContext::resolve("demo", &["filesystem_read".to_string()], Some(dir.path()))
                .unwrap();
        assert_eq!(ctx.workspace_root, Some(dir.path().canonicalize().unwrap()));
        assert!(ctx.scratch_dir.is_none());
    }

    #[test]
    fn fs_context_write_permission_grants_scratch_only() {
        let dir = tempfile::tempdir().unwrap();
        let ctx =
            WasmFsContext::resolve("demo", &["filesystem_write".to_string()], Some(dir.path()))
                .unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        assert_eq!(
            ctx.scratch_dir,
            Some(
                canonical
                    .join(".zen")
                    .join("data")
                    .join("plugin")
                    .join("demo")
            )
        );
        assert!(ctx.workspace_root.is_none());
    }

    #[test]
    fn fs_context_rejects_invalid_plugin_id() {
        let dir = tempfile::tempdir().unwrap();
        let err = WasmFsContext::resolve(
            "evil/../..",
            &["filesystem_write".to_string()],
            Some(dir.path()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid plugin id"), "got: {err}");
    }

    #[test]
    fn fs_context_rejects_protected_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        let err = WasmFsContext::resolve("demo", &["filesystem_read".to_string()], Some(&ssh))
            .unwrap_err();
        assert!(err.to_string().contains("protected path"), "got: {err}");
    }

    #[test]
    fn fs_context_rejects_scratch_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let plugin_data = root.path().join(".zen").join("data").join("plugin");
        std::fs::create_dir_all(&plugin_data).unwrap();
        std::os::unix::fs::symlink(outside.path(), plugin_data.join("demo")).unwrap();
        let err =
            WasmFsContext::resolve("demo", &["filesystem_write".to_string()], Some(root.path()))
                .unwrap_err();
        assert!(err.to_string().contains("symlink escape"), "got: {err}");
    }

    #[test]
    fn fs_context_scratch_isolated_per_plugin() {
        let root = tempfile::tempdir().unwrap();
        let a = WasmFsContext::resolve(
            "plug-a",
            &["filesystem_write".to_string()],
            Some(root.path()),
        )
        .unwrap();
        let b = WasmFsContext::resolve(
            "plug-b",
            &["filesystem_write".to_string()],
            Some(root.path()),
        )
        .unwrap();
        assert_ne!(a.scratch_dir, b.scratch_dir);
    }

    // ── WASI-level preopen enforcement (TODO-011) ────────────────────────

    #[tokio::test]
    async fn fs_write_capability_creates_file_in_scratch() {
        let root = tempfile::tempdir().unwrap();
        let ctx =
            WasmFsContext::resolve("demo", &["filesystem_write".to_string()], Some(root.path()))
                .unwrap();
        let sandbox = WasmSandbox::new();
        let wasm = write_file_wat(3, "out.txt");
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();

        sandbox
            .execute_module_with_fs(&module, "run", &[], &ctx)
            .await
            .unwrap();

        let written =
            std::fs::read_to_string(ctx.scratch_dir.as_ref().unwrap().join("out.txt")).unwrap();
        assert_eq!(written, "hi\n");
    }

    #[tokio::test]
    async fn read_only_preopen_blocks_write() {
        let root = tempfile::tempdir().unwrap();
        let ctx =
            WasmFsContext::resolve("demo", &["filesystem_read".to_string()], Some(root.path()))
                .unwrap();
        let sandbox = WasmSandbox::new();
        let wasm = write_file_wat(3, "out.txt");
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();

        sandbox
            .execute_module_with_fs(&module, "run", &[], &ctx)
            .await
            .unwrap();

        assert!(
            !root.path().join("out.txt").exists(),
            "read-only preopen must block file creation"
        );
    }

    #[tokio::test]
    async fn scratch_symlink_escape_blocked_at_wasi_level() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let ctx =
            WasmFsContext::resolve("demo", &["filesystem_write".to_string()], Some(root.path()))
                .unwrap();
        let scratch = ctx.scratch_dir.as_ref().unwrap().clone();
        std::os::unix::fs::symlink(outside.path().join("escaped.txt"), scratch.join("evil.txt"))
            .unwrap();
        let sandbox = WasmSandbox::new();
        let wasm = write_file_wat(3, "evil.txt");
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();

        sandbox
            .execute_module_with_fs(&module, "run", &[], &ctx)
            .await
            .unwrap();

        assert!(
            !outside.path().join("escaped.txt").exists(),
            "symlink escape through the scratch preopen must be blocked by cap-std"
        );
    }

    #[tokio::test]
    async fn read_plus_write_grants_workspace_fd3_then_scratch_fd4() {
        let root = tempfile::tempdir().unwrap();
        let ctx = WasmFsContext::resolve(
            "demo",
            &[
                "filesystem_read".to_string(),
                "filesystem_write".to_string(),
            ],
            Some(root.path()),
        )
        .unwrap();
        let sandbox = WasmSandbox::new();
        let wasm = write_file_wat(4, "out.txt");
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();

        sandbox
            .execute_module_with_fs(&module, "run", &[], &ctx)
            .await
            .unwrap();

        let written =
            std::fs::read_to_string(ctx.scratch_dir.as_ref().unwrap().join("out.txt")).unwrap();
        assert_eq!(written, "hi\n");
        assert!(
            !root.path().join("out.txt").exists(),
            "workspace preopen is read-only even when scratch is writable"
        );
    }

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
        assert!(
            sandbox
                .validate_module(&wat::parse_str("(module)").unwrap())
                .is_ok()
        );
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
        assert!(
            sandbox
                .check_permission(&perms, "filesystem:write")
                .is_err()
        );
        assert!(sandbox.check_permission(&perms, "system").is_err());
    }

    #[test]
    fn validate_module_rejects_garbage() {
        let sandbox = WasmSandbox::new();
        assert!(
            sandbox
                .validate_module(&[0x00, 0x61, 0x73, 0x6d, 0xff])
                .is_err()
        );
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
    fn exported_functions_filters_internal_exports_and_signatures() {
        let sandbox = WasmSandbox::new();
        let wasm = wat::parse_str(
            r#"
            (module
                (func (export "_start"))
                (func (export "_internal"))
                (func (export "ping"))
                (func (export "with_result") (result i32)
                    i32.const 1
                )
                (func (export "with_param") (param i32))
                (memory (export "memory") 1)
            )
            "#,
        )
        .unwrap();

        let funcs = sandbox.exported_functions(&wasm).unwrap();
        assert_eq!(funcs, vec!["ping".to_string()]);
    }

    #[test]
    fn exported_functions_rejects_garbage_bytes() {
        let sandbox = WasmSandbox::new();
        assert!(
            sandbox
                .exported_functions(&[0x00, 0x61, 0x73, 0x6d, 0xff])
                .is_err()
        );
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

    #[tokio::test]
    async fn permission_gate_rejects_undeclared() {
        let tool = WasmSandboxTool::new(); // default policy: deny-all
        let dir = tempfile::tempdir().unwrap();
        let wasm_path = dir.path().join("empty.wasm");
        std::fs::write(&wasm_path, wat::parse_str("(module)").unwrap()).unwrap();

        let result = tool
            .invoke(json!({
                "wasm_path": wasm_path.to_str().unwrap(),
                "func_name": "_start",
                "permissions": { "network": true }
            }))
            .await;

        let err = result.unwrap_err().to_string();
        assert!(err.contains("permission denied"), "got: {err}");
        assert!(err.contains("network"), "got: {err}");
    }

    #[tokio::test]
    async fn permission_gate_allows_declared() {
        let sandbox = WasmSandbox::new().with_policy(WasmPermissions {
            allow_network: true,
            ..Default::default()
        });
        let tool = WasmSandboxTool::with_sandbox(sandbox);
        let dir = tempfile::tempdir().unwrap();
        let wasm_path = dir.path().join("empty.wasm");
        std::fs::write(
            &wasm_path,
            wat::parse_str(r#"(module (func (export "_start")))"#).unwrap(),
        )
        .unwrap();

        let result = tool
            .invoke(json!({
                "wasm_path": wasm_path.to_str().unwrap(),
                "func_name": "_start",
                "permissions": { "network": true }
            }))
            .await;
        assert!(result.is_ok(), "got: {result:?}");
    }

    #[tokio::test]
    async fn memory_limit_traps_oversized_module() {
        let limits = ResourceLimits {
            max_memory_bytes: 64 * 1024 * 1024,
            ..ResourceLimits::default()
        };
        let sandbox = WasmSandbox::with_limits(limits);
        // 65536 pages × 64 KiB = 4 GiB minimum memory
        let wasm = wat::parse_str(r#"(module (memory 65536) (func (export "_start")))"#).unwrap();

        let result = sandbox.execute(&wasm, "_start", &[]).await;
        assert!(result.is_err(), "oversized module should be rejected");
    }

    #[test]
    fn store_limits_denies_oversized_growth() {
        let mut limits = StoreLimits::new(64 * 1024 * 1024);
        assert!(limits.memory_growing(0, 1024 * 1024, None).unwrap());
        assert!(
            !limits
                .memory_growing(0, 4 * 1024 * 1024 * 1024, None)
                .unwrap()
        );
        assert!(limits.table_growing(0, 100, None).unwrap());
    }

    #[test]
    fn fr052_plugin_scratch_uses_data_plugin_path() {
        let root = PathBuf::from("/workspace");
        let plugin_id = "test-plugin";
        let scratch = root
            .join(".zen")
            .join("data")
            .join("plugin")
            .join(plugin_id);
        assert_eq!(
            scratch,
            PathBuf::from("/workspace/.zen/data/plugin/test-plugin")
        );
        assert!(!scratch.to_string_lossy().contains("plugin-data"));
    }
}
