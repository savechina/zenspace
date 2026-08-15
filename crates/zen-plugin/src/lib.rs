pub mod platform;
pub mod registry;
pub mod tools;
pub mod wasm_sandbox;

use std::path::Path;
use std::sync::Arc;

use rig_compose::normalizer::ToolDispatchHook;
use rig_compose::registry::ToolRegistry;
use rig_compose::tool::Tool;
use thiserror::Error;

pub use platform::{Platform, detect_platform};
pub use registry::{
    Lifecycle, Manifest, PluginEntry, PluginKind, PluginRegistry, PluginRegistryError,
};
pub use wasm_sandbox::{ExecutionOutput, ResourceLimits, WasmSandbox, WasmSandboxError};

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
    tools: &'a mut ToolRegistry,
    hooks: &'a mut Vec<Box<dyn ToolDispatchHook>>,
    workspace_root: &'a Path,
}

impl<'a> PluginApi<'a> {
    pub fn new(
        tools: &'a mut ToolRegistry,
        hooks: &'a mut Vec<Box<dyn ToolDispatchHook>>,
        workspace_root: &'a Path,
    ) -> Self {
        Self {
            tools,
            hooks,
            workspace_root,
        }
    }

    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        // Namespace: if tool name doesn't already contain '.', we can't rename
        // (name is on the Tool impl). Just register directly — namespacing
        // happens at plugin instantiation time (T065).
        self.tools.register(tool);
    }

    pub fn register_hook(&mut self, hook: Box<dyn ToolDispatchHook>) {
        self.hooks.push(hook);
    }

    pub fn workspace_root(&self) -> &Path {
        self.workspace_root
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
        let mut api = PluginApi::new(&mut tools, &mut hooks, root);

        assert!(plugin.activate(&mut api).is_ok());
    }

    #[test]
    fn workspace_root_returns_passed_root() {
        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let api = PluginApi::new(&mut tools, &mut hooks, root);

        assert_eq!(api.workspace_root(), root);
    }

    #[test]
    fn register_hook_appends_to_hooks() {
        let mut tools = ToolRegistry::new();
        let mut hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let root = Path::new("/tmp/ws");
        let mut api = PluginApi::new(&mut tools, &mut hooks, root);

        api.register_hook(Box::new(DummyHook));
        api.register_hook(Box::new(DummyHook));

        assert_eq!(hooks.len(), 2);
    }
}
