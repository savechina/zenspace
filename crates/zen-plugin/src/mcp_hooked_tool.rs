//! HookedMcpTool — MCP-surface dispatch wrapper (W12.28, review A2).
//!
//! # PURPOSE
//! Both MCP carriers dispatch tools straight off the registry
//! (`transport/http.rs` `mcp_handler` and the rig-mcp `serve_stdio`
//! `RegistryServer`), bypassing the 5-hook pipeline the agent rounds run
//! (confidentiality → budget → seatbelt → audit → approval). This adapter
//! wraps each exposed tool AT REGISTRY-BUILD TIME so both surfaces are
//! covered with zero changes to `http.rs` or rig-mcp.
//!
//! # USAGE
//! Built only by `ZenWiring::build_mcp_registry` (zen-agents). Each exposed
//! non-Confidential tool is registered as `HookedMcpTool::new(inner, …)`
//! with the tool's DECLARED sensitivity (never caller claims) and one
//! shared per-registry [`AtomicBudget`] (unit cost 20-cap, the same
//! hardcoded concurrency-limiter shape as the agent pipeline — D22: no new
//! config keys).
//!
//! # EXPECTED
//! `invoke` builds a one-tool registry and dispatches through
//! rig-compose's `dispatch_tool_invocations_with_hooks`, then returns the
//! inner tool's output. `schema`/`name` delegate byte-identical, so
//! `tools/list` output and the Confidential filter are unchanged.
//! Ask mode + mutating/cloud tool ⇒ structured `approval-required` deny (MCP
//! clients have no approval UX), mirroring the W5.2 background ruling.
//!
//! # ERRORS
//! Hook `Terminate`/`Skip`/budget exhaustion surface as the rig-compose
//! `KernelError` from the dispatch loop (`ToolDispatchTerminated` for
//! approvals), which MCP carriers already translate into `isError`
//! responses. The `MCP_CALL` task-local is scoped around the whole
//! dispatch so the approval hook and audit records see the MCP origin.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rig_compose::budget::{AtomicBudget, DispatchBudgetHook};
use rig_compose::normalizer::{
    ToolDispatchHook, ToolInvocation, dispatch_tool_invocations_with_hooks,
};
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use zen_core::sandbox::{SandboxMode, SeatbeltHook, SeatbeltPolicy};
use zen_core::types::Sensitivity;

use crate::task_registry::MCP_CALL;
use crate::tools::approval_hook::AskApprovalHook;
use crate::tools::audit_hook::ToolAuditHook;
use crate::tools::confidentiality_hook::ConfidentialityHook;

/// Audit origin marker written by the wrapped [`ToolAuditHook`].
pub const MCP_AUDIT_ORIGIN: &str = "mcp";

/// Adapter implementing the same [`Tool`] trait as its inner tool; every
/// `invoke` traverses the 5-hook dispatch pipeline before the tool body.
pub struct HookedMcpTool {
    inner: Arc<dyn Tool>,
    declared_sensitivity: Sensitivity,
    budget: Arc<AtomicBudget>,
    policy: SeatbeltPolicy,
    audit_log_path: PathBuf,
    mode: SandboxMode,
}

impl HookedMcpTool {
    /// Wrap `inner` for MCP-surface dispatch.
    ///
    /// - `declared_sensitivity`: the tool's sensitivity from the wiring's
    ///   `tool_sensitivity` map — the confidentiality gate's cell value,
    ///   never a caller-provided claim. Confidential tools are already
    ///   excluded by `build_mcp_registry`, so this is always
    ///   Public/Private and the gate's happy path is unchanged.
    /// - `budget`: ONE shared `AtomicBudget` per MCP registry (built once
    ///   in `build_mcp_registry`) — unit-cost concurrency limiter, same
    ///   shape as the agent pipeline's `build_sandbox_hooks`.
    /// - `policy`: the wiring's seatbelt policy (sandbox mode + workspace
    ///   roots + FR-035 arg registry).
    /// - `audit_log_path`: the shared audit trail (`logs/audit.jsonl`);
    ///   records written by the wrapped hook carry `"origin": "mcp"`.
    /// - `mode`: the wiring's sandbox mode, driving [`AskApprovalHook`].
    pub fn new(
        inner: Arc<dyn Tool>,
        declared_sensitivity: Sensitivity,
        budget: Arc<AtomicBudget>,
        policy: SeatbeltPolicy,
        audit_log_path: PathBuf,
        mode: SandboxMode,
    ) -> Self {
        Self {
            inner,
            declared_sensitivity,
            budget,
            policy,
            audit_log_path,
            mode,
        }
    }

    /// The hook pipeline for one invocation. Fresh instances per invoke:
    /// `ToolAuditHook`'s duration cell and `ConfidentialityHook`'s
    /// sensitivity cell are per-invocation state that concurrent MCP
    /// calls must not share.
    fn hooks(&self) -> Vec<Box<dyn ToolDispatchHook>> {
        vec![
            Box::new(ConfidentialityHook::new(self.declared_sensitivity)),
            Box::new(DispatchBudgetHook::new(Arc::clone(&self.budget), 1)),
            Box::new(SeatbeltHook::new(self.policy.clone())),
            Box::new(ToolAuditHook::new(self.audit_log_path.clone()).with_origin(MCP_AUDIT_ORIGIN)),
            Box::new(AskApprovalHook::new(self.mode)),
        ]
    }
}

impl std::fmt::Debug for HookedMcpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookedMcpTool")
            .field("tool", &self.inner.name())
            .field("declared_sensitivity", &self.declared_sensitivity)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for HookedMcpTool {
    /// Delegates unchanged: `tools/list` on both MCP carriers sees the
    /// same name/description/schema as before the wrapper existed.
    fn schema(&self) -> ToolSchema {
        self.inner.schema()
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let registry = ToolRegistry::new();
        registry.register(Arc::clone(&self.inner));
        let invocation = ToolInvocation {
            name: self.inner.name(),
            args,
        };
        let hooks = self.hooks();
        let hook_refs: Vec<&dyn ToolDispatchHook> = hooks
            .iter()
            .map(|h| h.as_ref() as &dyn ToolDispatchHook)
            .collect();

        // Scope the MCP marker around the whole dispatch: the approval hook
        // denies mutating/cloud tools with the structured `approval-required`
        // error and the audit hook tags records with origin "mcp".
        let mut results = MCP_CALL
            .scope(
                true,
                dispatch_tool_invocations_with_hooks(&registry, &[invocation], &hook_refs),
            )
            .await?;

        let output = results.pop().map(|r| r.output).unwrap_or_else(|| json!({}));
        Ok(output)
    }
}
