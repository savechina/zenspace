use async_trait::async_trait;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
};
use rig_compose::registry::KernelError;
use serde_json::json;

use std::sync::{Arc, Mutex};

use zen_core::types::Sensitivity;

// ---------------------------------------------------------------------------
// Tool metadata manifest (D28)
// ---------------------------------------------------------------------------
//
// Replaces the previous fragile tool-name string matching for confidentiality
// (and approval) classification. A static manifest is the simplest fit for this
// codebase: the tool set is fixed at compile time (see `wiring.rs` tool
// catalogue) and the classification fields (`cloud`, `mutating`, `io`,
// `confidence`) are stable. Dynamic per-invocation fields (`path`, `args`,
// `model_meta`) are intentionally not part of the static manifest — they are
// already carried by `ToolInvocation` itself and consulted at runtime.
//
// Both `ConfidentialityHook` (cloud gating, FR-009) and `AskApprovalHook`
// (mutating/cloud gating, FR-019) consume this table via the
// [`tool_metadata`] / [`is_cloud_tool`] / [`is_mutating_tool`] helpers.

/// Static classification metadata for a registered tool.
///
/// Fields mirror the D28 spec shape, restricted to what is statically knowable
/// and consumed by the dispatch hooks (dynamic `path`/`args`/`model_meta` are
/// per-invocation and live on `ToolInvocation`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolMetadata {
    /// Canonical tool name (matches the `ToolRegistry` key).
    pub name: &'static str,
    /// Reaches an external/cloud service (web.search, web.fetch, ...).
    pub cloud: bool,
    /// Mutates filesystem or external state (fs.write/edit/delete/move/copy).
    pub mutating: bool,
    /// Performs I/O that may leak content beyond the process boundary.
    pub io: bool,
    /// Baseline confidence weight for routing heuristics (0.0..=1.0).
    pub confidence: f32,
}

/// Static manifest of tool classification. Tools not listed here fall back to
/// conservative heuristics in [`is_cloud_tool`] / [`is_mutating_tool`] so
/// unknown `web.*` / `http*` tools are still treated as cloud.
pub static TOOL_METADATA: &[ToolMetadata] = &[
    ToolMetadata {
        name: "fs.read",
        cloud: false,
        mutating: false,
        io: false,
        confidence: 0.9,
    },
    ToolMetadata {
        name: "fs.list",
        cloud: false,
        mutating: false,
        io: false,
        confidence: 0.9,
    },
    ToolMetadata {
        name: "fs.grep",
        cloud: false,
        mutating: false,
        io: false,
        confidence: 0.9,
    },
    ToolMetadata {
        name: "fs.glob",
        cloud: false,
        mutating: false,
        io: false,
        confidence: 0.9,
    },
    ToolMetadata {
        name: "fs.write",
        cloud: false,
        mutating: true,
        io: false,
        confidence: 0.8,
    },
    ToolMetadata {
        name: "fs.edit",
        cloud: false,
        mutating: true,
        io: false,
        confidence: 0.8,
    },
    ToolMetadata {
        name: "fs.delete",
        cloud: false,
        mutating: true,
        io: false,
        confidence: 0.8,
    },
    ToolMetadata {
        name: "fs.move",
        cloud: false,
        mutating: true,
        io: false,
        confidence: 0.8,
    },
    ToolMetadata {
        name: "fs.copy",
        cloud: false,
        mutating: true,
        io: false,
        confidence: 0.8,
    },
    ToolMetadata {
        name: "web.fetch",
        cloud: true,
        mutating: false,
        io: true,
        confidence: 0.7,
    },
    ToolMetadata {
        name: "web.search",
        cloud: true,
        mutating: false,
        io: true,
        confidence: 0.7,
    },
];

/// Look up the static metadata for a registered tool.
pub fn tool_metadata(name: &str) -> Option<&'static ToolMetadata> {
    TOOL_METADATA.iter().find(|m| m.name == name)
}

/// Whether a tool reaches an external/cloud service.
///
/// Metadata-driven for known tools; falls back to a conservative name
/// heuristic for tools absent from [`TOOL_METADATA`] so unknown `web.*` /
/// `http*` / `*network*` tools are still classified as cloud.
pub fn is_cloud_tool(tool_name: &str) -> bool {
    if let Some(m) = tool_metadata(tool_name) {
        return m.cloud;
    }
    let lower = tool_name.to_lowercase();
    lower.starts_with("web.")
        || lower.contains("http")
        || lower.contains("network")
        || lower.contains("cloud")
}

/// Whether a tool mutates filesystem or external state.
///
/// Metadata-driven for known tools; unknown tools default to non-mutating
/// (read-only) so they are not needlessly gated by the approval hook.
pub fn is_mutating_tool(tool_name: &str) -> bool {
    match tool_metadata(tool_name) {
        Some(m) => m.mutating,
        None => false,
    }
}

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
