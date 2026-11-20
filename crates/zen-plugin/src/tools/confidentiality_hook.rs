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
    ToolMetadata {
        name: "shell.exec",
        cloud: false,
        mutating: true,
        io: true,
        confidence: 0.6,
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
        // D30 (F1+OV-1): the turn-scoped TurnContext is the sanctioned
        // source on the turn path — a concurrent hosted session can no
        // longer flip this gate's policy through the shared cell. The cell
        // remains the documented non-turn fallback (direct dispatch outside
        // any orchestrator turn scope).
        let sensitivity = zen_core::turn_context::current_sensitivity().unwrap_or_else(|| {
            // W12.9 (T16/N8) fail-closed: a poisoned cell must never skip the
            // confidentiality gate (the old `unwrap_or(Public)` fallback was
            // fail-open — a poisoned Confidential cell would have allowed
            // cloud egress). Mirror the zen-gateway guards::lock_ok posture
            // locally (zen-core has no shared lock_ok helper): warn on
            // poison, recover the guard, and gate on the last-written value.
            match self.sensitivity.lock() {
                Ok(guard) => *guard,
                Err(poisoned) => {
                    tracing::warn!(
                        "confidentiality sensitivity cell poisoned; recovering last value"
                    );
                    *poisoned.into_inner()
                }
            }
        });
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

    /// W11.1/T1 (D30, F1+OV-1) race pin: the cloud-tool gate reads the
    /// turn-scoped TurnContext FIRST — a Confidential turn blocks cloud tools
    /// even when a concurrent session left the shared cell at Public.
    #[tokio::test]
    async fn turn_context_confidential_blocks_cloud_tools_over_public_cell() {
        // The shared cell is Public (a concurrent session's last write).
        let hook = ConfidentialityHook::new(Sensitivity::Public);
        let invocation = ToolInvocation {
            name: "web.search".into(),
            args: serde_json::json!({"query": "secret"}),
        };
        let ctx = zen_core::turn_context::TurnContext {
            sensitivity: Sensitivity::Confidential,
            knowledge: Vec::new(),
            history: Vec::new(),
        };
        let action = zen_core::turn_context::TURN_CONTEXT
            .scope(ctx, async { hook.before_invocation(&invocation).await })
            .await
            .unwrap();
        assert!(
            matches!(action, ToolDispatchAction::Skip { .. }),
            "a Confidential turn must block cloud tools despite the Public cell"
        );
        // Outside the turn scope the Public cell allows the cloud tool.
        let fallback = hook.before_invocation(&invocation).await.unwrap();
        assert!(matches!(fallback, ToolDispatchAction::Continue));
    }

    /// W12.9 (T16/N8): a poisoned sensitivity cell never skips the
    /// confidentiality gate — the recovered value still enforces the
    /// decision (the old Public fallback was fail-open).
    #[tokio::test]
    async fn poisoned_cell_still_enforces_confidentiality() {
        let hook = ConfidentialityHook::new(Sensitivity::Confidential);
        let cell = hook.shared_sensitivity();
        // Poison the cell: panic while holding the lock, suppressed here.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cell.lock().unwrap();
            panic!("poison the sensitivity cell");
        }));
        let invocation = ToolInvocation {
            name: "web.search".into(),
            args: serde_json::json!({"query": "secret"}),
        };
        let action = hook.before_invocation(&invocation).await.unwrap();
        assert!(
            matches!(action, ToolDispatchAction::Skip { .. }),
            "a poisoned Confidential cell must still block cloud tools"
        );
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
