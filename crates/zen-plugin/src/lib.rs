pub mod hook_isolation;
pub mod platform;
pub mod plugin_wasm;
pub mod registry;
pub mod retry;
pub mod sandbox_launcher;
pub mod tools;
pub mod wasm_sandbox;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rig_compose::normalizer::ToolDispatchHook;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::Value;
use thiserror::Error;

use hook_isolation::IsolatedPluginHook;
pub use hook_isolation::{DENIED_BY_PLUGIN_HOOK, ToolSensitivityMap};
pub use platform::{Platform, detect_platform};
pub use plugin_wasm::{WasmPlugin, WasmPluginTool};
pub use registry::{
    Lifecycle, Manifest, PluginEntry, PluginKind, PluginRegistry, PluginRegistryError,
    RESERVED_NAMESPACE_PREFIXES,
};
pub use wasm_sandbox::{
    ExecutionOutput, ResourceLimits, WasmPermissions, WasmSandbox, WasmSandboxError,
};

/// Error type for plugin activation and lifecycle operations.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin activation failed: {0}")]
    Activation(String),

    #[error("plugin error: {0}")]
    Other(String),
}

/// A loadable plugin. Implementations register tools and hooks against the
/// [`PluginApi`] handed to them during [`Plugin::activate`].
pub trait Plugin: Send + Sync {
    fn id(&self) -> &str;
    fn activate(&self, api: &mut PluginApi<'_>) -> Result<(), PluginError>;
}

/// Handle a plugin uses to register tools and hooks into the agent kernel.
pub struct PluginApi<'a> {
    plugin_id: String,
    tools: &'a mut ToolRegistry,
    hooks: &'a mut Vec<Box<dyn ToolDispatchHook>>,
    workspace_root: &'a Path,
    builtin_tool_names: Vec<String>,
    hook_sensitivity: ToolSensitivityMap,
    audit_log_path: PathBuf,
}

impl<'a> PluginApi<'a> {
    pub fn new(
        plugin_id: &str,
        tools: &'a mut ToolRegistry,
        hooks: &'a mut Vec<Box<dyn ToolDispatchHook>>,
        workspace_root: &'a Path,
    ) -> Self {
        Self {
            plugin_id: plugin_id.to_string(),
            tools,
            hooks,
            workspace_root,
            builtin_tool_names: Vec::new(),
            hook_sensitivity: Arc::new(HashMap::new()),
            audit_log_path: PathBuf::from("logs/audit.jsonl"),
        }
    }

    /// T099/FR-050b: arm the registration collision guard with the builtin
    /// tool-name list (namespaced plugin tool names are checked against it).
    #[must_use]
    pub fn with_builtin_tool_names(mut self, names: Vec<String>) -> Self {
        self.builtin_tool_names = names;
        self
    }

    /// T101–T103/FR-048: configure hook isolation — the sensitivity table
    /// that hides `Confidential` invocations from plugin hooks, and the
    /// `logs/audit.jsonl` target for denial records.
    #[must_use]
    pub fn with_isolation(
        mut self,
        sensitivity: ToolSensitivityMap,
        audit_log_path: PathBuf,
    ) -> Self {
        self.hook_sensitivity = sensitivity;
        self.audit_log_path = audit_log_path;
        self
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Register a tool under the `{plugin_id}.{tool_name}` namespace (T090)
    /// so plugin tools cannot collide with builtin tools or each other.
    ///
    /// T099/FR-050b: a registration whose top-level namespace is a
    /// [`RESERVED_NAMESPACE_PREFIXES`] member, or whose namespaced name
    /// collides exactly with a builtin tool name, is rejected with a
    /// single-tool `warn` — the plugin's remaining tools still register.
    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        let namespaced = format!("{}.{}", self.plugin_id, tool.name());
        if RESERVED_NAMESPACE_PREFIXES.contains(&self.plugin_id.as_str()) {
            tracing::warn!(
                plugin = %self.plugin_id,
                tool = %namespaced,
                "plugin tool registration rejected: top-level namespace is reserved (FR-050b)"
            );
            return;
        }
        if self.builtin_tool_names.contains(&namespaced) {
            tracing::warn!(
                plugin = %self.plugin_id,
                tool = %namespaced,
                "plugin tool registration rejected: namespaced name collides with a builtin tool (FR-050b)"
            );
            return;
        }
        self.tools
            .register(Arc::new(NamespacedTool::new(self.plugin_id.clone(), tool)));
    }

    /// Register a dispatch hook. Every plugin hook is wrapped in
    /// [`IsolatedPluginHook`] (FR-048a): a hook `Err` denies only its own
    /// invocation instead of aborting the whole dispatch round, and
    /// `Confidential` invocations are invisible to the wrapped hook.
    pub fn register_hook(&mut self, hook: Box<dyn ToolDispatchHook>) {
        self.hooks.push(Box::new(IsolatedPluginHook::new(
            self.plugin_id.clone(),
            hook,
            Arc::clone(&self.hook_sensitivity),
            self.audit_log_path.clone(),
        )));
    }

    pub fn workspace_root(&self) -> &Path {
        self.workspace_root
    }
}

/// Registration adapter prefixing an inner tool's name with its plugin id.
/// `ToolRegistry` keys entries off `schema().name`, so the namespaced name
/// must surface in the schema itself, not just `name()`.
struct NamespacedTool {
    prefix: String,
    inner: Arc<dyn Tool>,
}

impl NamespacedTool {
    fn new(prefix: String, inner: Arc<dyn Tool>) -> Self {
        Self { prefix, inner }
    }

    fn namespaced_name(&self) -> String {
        format!("{}.{}", self.prefix, self.inner.name())
    }
}

#[async_trait]
impl Tool for NamespacedTool {
    fn schema(&self) -> ToolSchema {
        let mut schema = self.inner.schema();
        schema.name = self.namespaced_name();
        schema
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        self.inner.invoke(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_compose::normalizer::{ToolDispatchAction, ToolInvocation};
    use rig_compose::registry::KernelError;

    struct EchoPlugin;

    impl Plugin for EchoPlugin {
        fn id(&self) -> &str {
            "echo"
        }

        fn activate(&self, _api: &mut PluginApi<'_>) -> Result<(), PluginError> {
            Ok(())
        }
    }

    struct DummyHook;

    #[async_trait::async_trait]
    impl ToolDispatchHook for DummyHook {
        async fn before_invocation(
            &self,
            _invocation: &ToolInvocation,
        ) -> Result<ToolDispatchAction, KernelError> {
            Ok(ToolDispatchAction::Continue)
        }
    }

    #[test]
    fn mock_plugin_activates_ok() {
        let plugin = EchoPlugin;
        assert_eq!(plugin.id(), "echo");

        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let mut api = PluginApi::new("echo", &mut tools, &mut hooks, root);

        assert!(plugin.activate(&mut api).is_ok());
    }

    #[test]
    fn plugin_api_exposes_plugin_id() {
        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let api = PluginApi::new("echo", &mut tools, &mut hooks, root);

        assert_eq!(api.plugin_id(), "echo");
        assert_eq!(api.workspace_root(), root);
    }

    #[test]
    fn register_hook_appends_to_hooks() {
        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let mut api = PluginApi::new("echo", &mut tools, &mut hooks, root);

        api.register_hook(Box::new(DummyHook));
        api.register_hook(Box::new(DummyHook));

        assert_eq!(hooks.len(), 2);
    }

    struct GreetTool;

    #[async_trait::async_trait]
    impl Tool for GreetTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "greet".to_string(),
                description: "greet".to_string(),
                args_schema: serde_json::json!({}),
                result_schema: serde_json::json!({}),
            }
        }

        async fn invoke(&self, _args: serde_json::Value) -> Result<serde_json::Value, KernelError> {
            Ok(serde_json::json!({ "hello": true }))
        }
    }

    #[test]
    fn register_tool_namespaces_with_plugin_id() {
        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let mut api = PluginApi::new("echo", &mut tools, &mut hooks, root);

        api.register_tool(Arc::new(GreetTool));

        let names: Vec<String> = tools.schemas().iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, vec!["echo.greet".to_string()]);
        assert!(tools.get("echo.greet").is_ok());
        assert!(tools.get("greet").is_err());
    }

    #[tokio::test]
    async fn namespaced_tool_delegates_invoke() {
        let inner: Arc<dyn Tool> = Arc::new(GreetTool);
        let namespaced = NamespacedTool::new("echo".to_string(), inner);

        assert_eq!(namespaced.name(), "echo.greet");
        assert_eq!(namespaced.schema().name, "echo.greet");
        let result = namespaced.invoke(serde_json::json!({})).await.unwrap();
        assert_eq!(result["hello"], true);
    }

    // ── T099/T112 (FR-050b): registration collision guard ──────────────────

    struct NamedTool(&'static str);

    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.0.to_string(),
                description: self.0.to_string(),
                args_schema: serde_json::json!({}),
                result_schema: serde_json::json!({}),
            }
        }

        async fn invoke(&self, _args: serde_json::Value) -> Result<serde_json::Value, KernelError> {
            Ok(serde_json::json!({ "ran": self.0 }))
        }
    }

    #[test]
    fn register_tool_rejects_reserved_namespace_prefix() {
        // A plugin whose id IS a reserved namespace ("fs") cannot register
        // any tool — its namespaced names would spoof the fs.* namespace.
        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let mut api = PluginApi::new("fs", &mut tools, &mut hooks, root);

        api.register_tool(Arc::new(NamedTool("read")));
        api.register_tool(Arc::new(NamedTool("spoof")));

        assert_eq!(
            tools.len(),
            0,
            "reserved-prefix plugin must register nothing"
        );
    }

    #[test]
    fn register_tool_rejects_builtin_name_collision_sibling_survives() {
        // Exact namespaced-name collision with a known builtin → only the
        // colliding tool is rejected; the plugin's other tools register.
        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let mut api = PluginApi::new("echo", &mut tools, &mut hooks, root)
            .with_builtin_tool_names(vec!["echo.greet".to_string()]);

        api.register_tool(Arc::new(NamedTool("greet")));
        api.register_tool(Arc::new(NamedTool("other")));

        assert!(
            tools.get("echo.greet").is_err(),
            "collision must be rejected"
        );
        assert!(tools.get("echo.other").is_ok(), "sibling must register");
        assert_eq!(tools.len(), 1);
    }

    #[tokio::test]
    async fn register_hook_wraps_in_isolation_adapter() {
        use rig_compose::normalizer::{ToolDispatchAction, ToolInvocation};

        // register_hook must hand back an IsolatedPluginHook: an erroring
        // inner hook surfaces as a Skip deny, not an Err.
        struct ExplodingHook;

        #[async_trait::async_trait]
        impl ToolDispatchHook for ExplodingHook {
            async fn before_invocation(
                &self,
                _invocation: &ToolInvocation,
            ) -> Result<ToolDispatchAction, KernelError> {
                Err(KernelError::ToolFailed("boom".to_string()))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let sensitivity: ToolSensitivityMap = std::sync::Arc::new(HashMap::new());
        let mut api = PluginApi::new("glitchy", &mut tools, &mut hooks, root)
            .with_isolation(sensitivity, dir.path().join("audit.jsonl"));

        api.register_hook(Box::new(ExplodingHook));
        assert_eq!(hooks.len(), 1);

        let invocation = ToolInvocation {
            name: "fs.read".to_string(),
            args: serde_json::json!({}),
        };
        match hooks[0].before_invocation(&invocation).await.unwrap() {
            ToolDispatchAction::Skip { output, .. } => {
                assert!(output["error"].as_str().unwrap().contains("glitchy"));
            }
            other => panic!("wrapped hook must deny via Skip, got {other:?}"),
        }
        assert!(
            dir.path().join("audit.jsonl").exists(),
            "deny must append an audit record"
        );
    }
}
