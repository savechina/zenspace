//! WASM plugin adapter (T089, FR-033): loads a `.wasm` plugin entry into a
//! [`Plugin`] whose activation registers every exported function as a tool
//! namespaced `{plugin_id}.{func_name}` via [`PluginApi`](crate::PluginApi).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use crate::PluginApi;
use crate::registry::PluginEntry;
use crate::wasm_sandbox::{ResourceLimits, WasmFsContext, WasmPermissions, WasmSandbox};
use crate::{Plugin, PluginError};

/// Map a manifest permission string to the resource string
/// [`WasmSandbox::check_permission`] expects. Unknown permission strings
/// pass through unchanged (check_permission allows unknown resources).
fn permission_resource(permission: &str) -> &str {
    match permission {
        "filesystem_read" => "filesystem:read",
        "filesystem_write" => "filesystem:write",
        other => other,
    }
}

/// A `.wasm` plugin backed by the shared [`WasmSandbox`].
pub struct WasmPlugin {
    id: String,
    wasm_bytes: Arc<Vec<u8>>,
    sandbox: Arc<WasmSandbox>,
    permissions: Vec<String>,
}

impl WasmPlugin {
    /// Load a `.wasm` plugin from a registry entry under the given policy.
    ///
    /// Fails when the entry file is missing/unreadable, fails WASM
    /// validation, or when ANY manifest-declared permission is denied by
    /// the policy (FR-029 load-time gate; callers log + isolate).
    pub fn from_entry(entry: &PluginEntry, policy: &WasmPermissions) -> Result<Self, String> {
        let Some(entry_file) = &entry.manifest.entry else {
            return Err("plugin has no entry file".to_string());
        };

        let entry_path = entry.dir.join(entry_file);
        let wasm_bytes = std::fs::read(&entry_path).map_err(|e| {
            format!(
                "failed to read plugin entry {}: {}",
                entry_path.display(),
                e
            )
        })?;

        let sandbox = Arc::new(
            WasmSandbox::with_limits(ResourceLimits::default()).with_policy(policy.clone()),
        );

        sandbox
            .validate_module(&wasm_bytes)
            .map_err(|e| format!("wasm validation failed: {}", e))?;

        for permission in &entry.manifest.permissions {
            let resource = permission_resource(permission);
            if sandbox
                .check_permission(sandbox.policy(), resource)
                .is_err()
            {
                return Err(format!("permission denied by policy: {resource}"));
            }
        }

        Ok(Self {
            id: entry.manifest.id.clone(),
            wasm_bytes: Arc::new(wasm_bytes),
            sandbox,
            permissions: entry.manifest.permissions.clone(),
        })
    }
}

impl Plugin for WasmPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn activate(&self, api: &mut PluginApi<'_>) -> Result<(), PluginError> {
        let funcs = self
            .sandbox
            .exported_functions(&self.wasm_bytes)
            .map_err(|e| PluginError::Activation(format!("failed to enumerate exports: {}", e)))?;

        for func in funcs {
            api.register_tool(Arc::new(WasmPluginTool::new(
                self.id.clone(),
                func,
                Arc::clone(&self.sandbox),
                Arc::clone(&self.wasm_bytes),
                self.permissions.clone(),
                Some(api.workspace_root().to_path_buf()),
            )));
        }
        Ok(())
    }
}

static WASM_PLUGIN_RESULT_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
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
                    "plugin": { "type": "string" },
                    "func_name": { "type": "string" },
                    "execution_time_ms": { "type": "integer" },
                    "memory_used_bytes": { "type": "integer" }
                }
            }
        }
    })
});

/// One exported wasm function of a [`WasmPlugin`], exposed as a tool.
///
/// `name()` is the bare function name; namespacing to
/// `{plugin_id}.{func_name}` happens at registration (T090).
///
/// The wasmtime [`Module`](wasmtime::Module) is compiled lazily on first
/// invoke and cached in a [`OnceLock`] (FR-051): compilation costs
/// ~10-100ms, so concurrent first invocations compile exactly once and
/// later invocations reuse the cached module. The wasm bytes are held as
/// `Arc<Vec<u8>>` so sibling tools of the same plugin share one copy.
pub struct WasmPluginTool {
    plugin_id: String,
    func_name: String,
    sandbox: Arc<WasmSandbox>,
    wasm_bytes: Arc<Vec<u8>>,
    module: OnceLock<Result<wasmtime::Module, String>>,
    permissions: Vec<String>,
    workspace_root: Option<PathBuf>,
    #[cfg(test)]
    compile_count: Arc<AtomicUsize>,
}

impl WasmPluginTool {
    pub fn new(
        plugin_id: String,
        func_name: String,
        sandbox: Arc<WasmSandbox>,
        wasm_bytes: Arc<Vec<u8>>,
        permissions: Vec<String>,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self {
            plugin_id,
            func_name,
            sandbox,
            wasm_bytes,
            module: OnceLock::new(),
            permissions,
            workspace_root,
            #[cfg(test)]
            compile_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Successful module compilations so far (FR-051 test hook).
    #[cfg(test)]
    pub(crate) fn compile_count(&self) -> usize {
        self.compile_count.load(Ordering::SeqCst)
    }
}

/// Enumerate files written under the plugin's scratch dir (relative paths,
/// sorted). Copy-out to the real workspace stays host-mediated: the agent
/// reads `scratch_outputs` and decides what (if anything) to move via
/// fs.write. TODO: a `zen_copy_out` host function to let plugins request
/// validated, audit-logged copies directly.
fn list_scratch_files(scratch: &Path, max_depth: usize) -> Vec<String> {
    fn walk(dir: &Path, scratch: &Path, depth_left: usize, out: &mut Vec<String>) {
        if depth_left == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // symlink_metadata: never descend into symlinked dirs a plugin
            // may have planted in its own scratch.
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                walk(&path, scratch, depth_left - 1, out);
            } else if let Ok(rel) = path.strip_prefix(scratch) {
                out.push(rel.display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(scratch, scratch, max_depth, &mut out);
    out.sort();
    out.dedup();
    out
}

#[async_trait]
impl Tool for WasmPluginTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.func_name.clone(),
            description: format!("WASM plugin tool {}.{}", self.plugin_id, self.func_name),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "inputs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Argument strings passed to the function as WASI argv"
                    }
                }
            }),
            result_schema: WASM_PLUGIN_RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let inputs: Vec<String> = args
            .get("inputs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Defense-in-depth: re-check every manifest-declared permission on
        // each invoke (same per-invoke gate as WasmSandboxTool, FR-029).
        // The gate deliberately precedes the FR-051 compile cache: a denied
        // invocation must never compile or execute the module.
        for permission in &self.permissions {
            let resource = permission_resource(permission);
            if let Err(e) = self
                .sandbox
                .check_permission(self.sandbox.policy(), resource)
            {
                return Err(KernelError::ToolFailed(format!(
                    "WASM permission denied: {}",
                    e
                )));
            }
        }

        // TODO-011: resolve manifest filesystem capabilities into preopen
        // targets. Fail closed — an unresolvable capability rejects the
        // invocation before compilation.
        let fs_ctx = WasmFsContext::resolve(
            &self.plugin_id,
            &self.permissions,
            self.workspace_root.as_deref(),
        )
        .map_err(|e| KernelError::ToolFailed(format!("WASM fs capability denied: {}", e)))?;

        // FR-051: compile once per tool instance. `get_or_init` runs the
        // closure exactly once even under concurrent first invocations; the
        // Result is cached because the bytes are immutable, so a compile
        // failure is deterministic and re-reporting it per invoke preserves
        // the pre-cache behavior.
        let module = self
            .module
            .get_or_init(|| {
                let compiled =
                    wasmtime::Module::new(self.sandbox.engine(), self.wasm_bytes.as_slice())
                        .map_err(|e| e.to_string());
                #[cfg(test)]
                {
                    if compiled.is_ok() {
                        self.compile_count.fetch_add(1, Ordering::SeqCst);
                    }
                }
                compiled
            })
            .as_ref()
            .map_err(|e| KernelError::ToolFailed(format!("WASM compilation failed: {}", e)))?;

        let output = self
            .sandbox
            .execute_module_with_fs(module, &self.func_name, &inputs, &fs_ctx)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("WASM execution failed: {}", e)))?;

        let scratch_outputs = fs_ctx
            .scratch_dir
            .as_deref()
            .map(|scratch| list_scratch_files(scratch, 8))
            .unwrap_or_default();

        Ok(json!({
            "output": {
                "stdout": output.stdout,
                "stderr": output.stderr,
                "exit_code": output.exit_code,
            },
            "scratch_outputs": scratch_outputs,
            "metrics": {
                "plugin": self.plugin_id,
                "func_name": self.func_name,
                "fs_capabilities": {
                    "workspace_read": fs_ctx.workspace_root.is_some(),
                    "scratch_write": fs_ctx.scratch_dir.is_some(),
                },
                "execution_time_ms": output.execution_time_ms,
                "memory_used_bytes": output.memory_used_bytes,
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Manifest, PluginKind};
    use rig_compose::registry::ToolRegistry;

    fn wasm_entry(dir: &std::path::Path, permissions: Vec<String>) -> PluginEntry {
        let wasm =
            wat::parse_str(r#"(module (func (export "ping")) (func (export "_start")))"#).unwrap();
        std::fs::write(dir.join("plugin.wasm"), wasm).unwrap();
        PluginEntry::new(
            Manifest {
                id: "demo".to_string(),
                name: "Demo".to_string(),
                version: "0.1.0".to_string(),
                kind: PluginKind::Tool,
                permissions,
                config_schema: None,
                entry: Some("plugin.wasm".to_string()),
                sha256: None,
            },
            dir.to_path_buf(),
        )
    }

    #[test]
    fn from_entry_rejects_permission_denied_by_policy() {
        let dir = tempfile::tempdir().unwrap();
        let entry = wasm_entry(dir.path(), vec!["network".to_string()]);

        let err = WasmPlugin::from_entry(&entry, &WasmPermissions::default())
            .err()
            .unwrap();
        assert!(
            err.contains("permission denied by policy: network"),
            "got: {err}"
        );
    }

    #[test]
    fn from_entry_allows_permission_granted_by_policy() {
        let dir = tempfile::tempdir().unwrap();
        let entry = wasm_entry(dir.path(), vec!["network".to_string()]);

        let policy = WasmPermissions {
            allow_network: true,
            ..WasmPermissions::default()
        };
        let plugin = WasmPlugin::from_entry(&entry, &policy).unwrap();
        assert_eq!(plugin.id(), "demo");
    }

    #[test]
    fn from_entry_fails_on_missing_entry_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut entry = wasm_entry(dir.path(), vec![]);
        entry.manifest.entry = Some("absent.wasm".to_string());

        let err = WasmPlugin::from_entry(&entry, &WasmPermissions::default())
            .err()
            .unwrap();
        assert!(err.contains("failed to read plugin entry"), "got: {err}");
    }

    #[test]
    fn from_entry_fails_on_invalid_wasm() {
        let dir = tempfile::tempdir().unwrap();
        let entry = wasm_entry(dir.path(), vec![]);
        std::fs::write(
            dir.path().join("plugin.wasm"),
            [0x00u8, 0x61, 0x73, 0x6d, 0xff],
        )
        .unwrap();

        let err = WasmPlugin::from_entry(&entry, &WasmPermissions::default())
            .err()
            .unwrap();
        assert!(err.contains("wasm validation failed"), "got: {err}");
    }

    #[test]
    fn activate_registers_namespaced_tools() {
        let dir = tempfile::tempdir().unwrap();
        let entry = wasm_entry(dir.path(), vec![]);
        let plugin = WasmPlugin::from_entry(&entry, &WasmPermissions::default()).unwrap();

        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn rig_compose::normalizer::ToolDispatchHook>> = Vec::new();
        let root = std::path::Path::new("/tmp/ws");
        let mut api = PluginApi::new("demo", &mut tools, &mut hooks, root);

        plugin.activate(&mut api).unwrap();

        assert!(tools.get("demo.ping").is_ok(), "namespaced tool missing");
        assert!(
            tools.get("ping").is_err(),
            "bare name must not be registered"
        );
        assert!(
            tools.get("demo._start").is_err(),
            "_start must never be registered"
        );
    }

    #[tokio::test]
    async fn wasm_plugin_tool_invokes_exported_func() {
        let tool = WasmPluginTool::new(
            "demo".to_string(),
            "ping".to_string(),
            Arc::new(WasmSandbox::new()),
            Arc::new(wat::parse_str(r#"(module (func (export "ping")))"#).unwrap()),
            vec![],
            None,
        );

        let schema = tool.schema();
        assert_eq!(schema.name, "ping");
        assert!(schema.args_schema.get("properties").is_some());

        let result = tool
            .invoke(json!({ "inputs": ["arg1", "arg2"] }))
            .await
            .unwrap();
        assert_eq!(result["output"]["exit_code"], 0);
        assert_eq!(result["metrics"]["plugin"], "demo");
        assert_eq!(result["metrics"]["func_name"], "ping");
    }

    #[tokio::test]
    async fn wasm_plugin_tool_denies_revoked_permission_on_invoke() {
        // Load-time gate passed (policy granted network), but the sandbox's
        // policy denies it at invoke time → defense-in-depth rejection.
        let sandbox = Arc::new(WasmSandbox::new()); // deny-all policy
        let tool = WasmPluginTool::new(
            "demo".to_string(),
            "ping".to_string(),
            sandbox,
            Arc::new(wat::parse_str(r#"(module (func (export "ping")))"#).unwrap()),
            vec!["network".to_string()],
            None,
        );

        let err = tool.invoke(json!({})).await.unwrap_err().to_string();
        assert!(err.contains("permission denied"), "got: {err}");
        assert!(err.contains("network"), "got: {err}");
    }

    #[tokio::test]
    async fn wasm_plugin_tool_compiles_module_exactly_once() {
        let tool = WasmPluginTool::new(
            "demo".to_string(),
            "ping".to_string(),
            Arc::new(WasmSandbox::new()),
            Arc::new(wat::parse_str(r#"(module (func (export "ping")))"#).unwrap()),
            vec![],
            None,
        );

        let first = tool.invoke(json!({ "inputs": [] })).await.unwrap();
        assert_eq!(first["output"]["exit_code"], 0);
        assert_eq!(tool.compile_count(), 1, "first invoke must compile once");

        let second = tool.invoke(json!({ "inputs": ["again"] })).await.unwrap();
        assert_eq!(second["output"]["exit_code"], 0);
        assert_eq!(
            tool.compile_count(),
            1,
            "second invoke must reuse the cached module"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wasm_plugin_tool_concurrent_first_invokes_compile_once() {
        let tool = Arc::new(WasmPluginTool::new(
            "demo".to_string(),
            "ping".to_string(),
            Arc::new(WasmSandbox::new()),
            Arc::new(wat::parse_str(r#"(module (func (export "ping")))"#).unwrap()),
            vec![],
            None,
        ));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let tool = Arc::clone(&tool);
            handles.push(tokio::spawn(async move {
                tool.invoke(json!({ "inputs": [] })).await
            }));
        }

        for handle in handles {
            let result = handle
                .await
                .unwrap()
                .expect("concurrent invoke must succeed");
            assert_eq!(result["output"]["exit_code"], 0);
        }
        assert_eq!(
            tool.compile_count(),
            1,
            "concurrent first invokes must compile exactly once"
        );
    }

    #[tokio::test]
    async fn wasm_plugin_tool_denied_invoke_never_compiles() {
        // FR-029 gate precedes the FR-051 compile cache: repeated denied
        // invocations must trap PermissionDenied without ever compiling.
        let tool = WasmPluginTool::new(
            "demo".to_string(),
            "ping".to_string(),
            Arc::new(WasmSandbox::new()), // deny-all policy
            Arc::new(wat::parse_str(r#"(module (func (export "ping")))"#).unwrap()),
            vec!["network".to_string()],
            None,
        );

        for _ in 0..2 {
            let err = tool.invoke(json!({})).await.unwrap_err().to_string();
            assert!(err.contains("WASM permission denied"), "got: {err}");
            assert!(err.contains("network"), "got: {err}");
        }
        assert_eq!(tool.compile_count(), 0, "denied invoke must not compile");
    }

    #[tokio::test]
    async fn wasm_plugin_tool_fs_write_reports_scratch_outputs() {
        use crate::wasm_sandbox::tests::write_file_wat;

        let root = tempfile::tempdir().unwrap();
        let sandbox = WasmSandbox::new().with_policy(WasmPermissions {
            allow_filesystem_write: true,
            ..WasmPermissions::default()
        });
        let tool = WasmPluginTool::new(
            "demo".to_string(),
            "run".to_string(),
            Arc::new(sandbox),
            Arc::new(write_file_wat(3, "out.txt")),
            vec!["filesystem_write".to_string()],
            Some(root.path().to_path_buf()),
        );

        let result = tool.invoke(json!({ "inputs": [] })).await.unwrap();
        let outputs = result["scratch_outputs"].as_array().unwrap();
        assert!(outputs.iter().any(|v| v == "out.txt"), "got: {outputs:?}");
        assert_eq!(result["metrics"]["fs_capabilities"]["scratch_write"], true);
        let scratch = root
            .path()
            .join(".zen")
            .join("data")
            .join("plugin")
            .join("demo")
            .join("out.txt");
        assert_eq!(std::fs::read_to_string(scratch).unwrap(), "hi\n");
    }

    #[tokio::test]
    async fn wasm_plugin_tool_fs_capability_without_root_fails_closed() {
        let sandbox = WasmSandbox::new().with_policy(WasmPermissions {
            allow_filesystem_write: true,
            ..WasmPermissions::default()
        });
        let tool = WasmPluginTool::new(
            "demo".to_string(),
            "run".to_string(),
            Arc::new(sandbox),
            Arc::new(wat::parse_str(r#"(module (func (export "run")))"#).unwrap()),
            vec!["filesystem_write".to_string()],
            None,
        );

        let err = tool.invoke(json!({})).await.unwrap_err().to_string();
        assert!(err.contains("WASM fs capability denied"), "got: {err}");
        assert!(err.contains("no workspace root"), "got: {err}");
    }
}
