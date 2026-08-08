use async_trait::async_trait;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
};
use rig_compose::registry::KernelError;
use serde_json::json;

use std::sync::{Arc, Mutex};

use zen_core::types::Sensitivity;

/// Blocks cloud tools (web search, web fetch) when the active session is
/// tagged `Confidential` (FR-009).
///
/// Sensitivity is shared via `Arc<Mutex<Sensitivity>>` so the orchestrator
/// can update it per-session at runtime (the hook pipeline is built once per
/// `ZenWiring`). Placed first in the dispatch pipeline: a blocked call never
/// reaches the budget/seatbelt/audit hooks, but `after_invocation` still runs
/// on all hooks so the audit log records the skip.
#[derive(Clone)]
pub struct ConfidentialityHook {
    sensitivity: Arc<Mutex<Sensitivity>>,
}

fn is_cloud_tool(tool_name: &str) -> bool {
    let lower = tool_name.to_lowercase();
    lower.starts_with("web.")
        || lower.contains("http")
        || lower.contains("network")
        || lower.contains("cloud")
}

impl ConfidentialityHook {
    pub fn new(sensitivity: Sensitivity) -> Self {
        Self {
            sensitivity: Arc::new(Mutex::new(sensitivity)),
        }
    }

    pub fn shared_sensitivity(&self) -> Arc<Mutex<Sensitivity>> {
        self.sensitivity.clone()
    }
}

#[async_trait]
impl ToolDispatchHook for ConfidentialityHook {
    async fn before_invocation(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        let sensitivity = self
            .sensitivity
            .lock()
            .map(|guard| *guard)
            .unwrap_or(Sensitivity::Public);
        if sensitivity == Sensitivity::Confidential && is_cloud_tool(&invocation.name) {
            return Ok(ToolDispatchAction::Skip {
                output: json!({
                    "error": format!(
                        "cloud tool '{}' disabled for confidential content",
                        invocation.name
                    )
                }),
                reason: Some(format!(
                    "cloud tool '{}' disabled for confidential content",
                    invocation.name
                )),
            });
        }
        Ok(ToolDispatchAction::Continue)
    }

    async fn after_invocation(&self, _result: &ToolInvocationResult) -> Result<(), KernelError> {
        Ok(())
    }

    async fn on_invocation_error(
        &self,
        _invocation: &ToolInvocation,
        _error: &KernelError,
    ) -> Result<(), KernelError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloud_tool_detection() {
        assert!(is_cloud_tool("web.search"));
        assert!(is_cloud_tool("web.fetch"));
        assert!(is_cloud_tool("http_request"));
        assert!(is_cloud_tool("network_call"));
        assert!(!is_cloud_tool("fs.read"));
        assert!(!is_cloud_tool("tier2_search"));
    }

    #[tokio::test]
    async fn test_confidential_session_blocks_cloud_tools() {
        let hook = ConfidentialityHook::new(Sensitivity::Confidential);
        let invocation = ToolInvocation {
            name: "web.search".into(),
            args: serde_json::json!({"query": "test"}),
        };
        match hook.before_invocation(&invocation).await.unwrap() {
            ToolDispatchAction::Skip { output, reason } => {
                assert!(output["error"].is_string());
                assert!(reason.is_some());
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_confidential_session_allows_local_tools() {
        let hook = ConfidentialityHook::new(Sensitivity::Confidential);
        let invocation = ToolInvocation {
            name: "fs.read".into(),
            args: serde_json::json!({"path": "notes/foo.md"}),
        };
        assert!(matches!(
            hook.before_invocation(&invocation).await.unwrap(),
            ToolDispatchAction::Continue
        ));
    }

    #[tokio::test]
    async fn test_private_session_allows_cloud_tools() {
        let hook = ConfidentialityHook::new(Sensitivity::Private);
        let invocation = ToolInvocation {
            name: "web.search".into(),
            args: serde_json::json!({"query": "test"}),
        };
        assert!(matches!(
            hook.before_invocation(&invocation).await.unwrap(),
            ToolDispatchAction::Continue
        ));
    }
}
