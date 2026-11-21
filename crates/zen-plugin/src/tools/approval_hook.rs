use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
};
use rig_compose::registry::KernelError;

use zen_core::sandbox::{ApprovalCallback, ApprovalDecision, SandboxMode};

use crate::tools::confidentiality_hook::{is_cloud_tool, is_mutating_tool};

// Turn affinity for approval routing (SC-007).
//
// Read at `before_invocation` time (inside the turn's task tree) and
// forwarded explicitly across the `spawn_blocking` boundary below,
// which a task-local cannot cross on its own. Hosted turns set it via
// `.scope()`; unset (`None`) preserves the legacy first-free-claim
// behavior for single-context surfaces (TUI/CLI).
tokio::task_local! {
    pub static APPROVAL_TURN: Option<String>;

    /// Background-dispatch marker (W5.2, spec 006 D20 §10 — fail-closed
    /// approvals): set around an agent-future sub-turn running inside
    /// [`crate::task_registry::TaskRegistry::spawn_async`]. Task-locals do
    /// NOT cross `tokio::spawn`, so a background sub-turn has no originating
    /// surface AND no reachable turn affinity — any Ask-mode approval inside
    /// it resolves deny + audit + structured `approval-required` (never the
    /// legacy first-free-claim fallback below, which could render on an
    /// unrelated session's connection — T103 class hazard).
    pub static BACKGROUND_TASK: bool;

    /// W9.5-F3 watchdog-renewal seam (APPROVAL_TURN precedent): the gateway
    /// hosting layer scopes a renewal callback around the hosted turn's
    /// execution; the plan executor fires it after each task settles so the
    /// turn watchdog extends per completed task (never per wave — one wave
    /// can hold a 600s first-token wait). Unset → no-op: non-hosted
    /// surfaces (TUI/CLI direct orchestrator) keep byte-identical behavior.
    /// Task-locals do not cross `tokio::spawn`; the plan wave loop runs in
    /// the hosted turn's task tree (`dispatch_timed` buffered stream +
    /// `join_all` — no spawn between hosting.rs and this read).
    pub static WATCHDOG_RENEWAL: RefCell<Option<WatchdogRenewal>>;
}

/// W9.5-F3 watchdog-renewal callback: hosted turns install a closure that
/// extends the shared watchdog deadline (extend-only, ceiling-capped —
/// enforcement lives in zen-gateway `server::guards::WatchdogDeadline`).
pub type WatchdogRenewal = Arc<dyn Fn() + Send + Sync>;

/// Fires the watchdog-renewal heartbeat from inside the hosted turn's
/// task tree (plan executor, after one task settles). Unset task-local
/// = silent no-op.
pub fn fire_watchdog_renewal() {
    let _ = WATCHDOG_RENEWAL.try_with(|cell| {
        if let Some(renew) = cell.borrow().clone() {
            renew();
        }
    });
}

/// Wall-clock budget for an interactive approval callback. A prompt that does
/// not resolve within this window is treated as `Deny` so a hung TUI/dialog
/// can never wedge the dispatch loop (D26).
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

/// True iff an invocation must be gated by interactive approval.
///
/// Only mutating filesystem tools and cloud tools are gated (D26); read-only
/// local tools bypass approval entirely so `SandboxMode::Ask` stays usable for
/// non-destructive exploration.
fn requires_approval(invocation: &ToolInvocation) -> bool {
    is_mutating_tool(&invocation.name) || is_cloud_tool(&invocation.name)
}

/// Run the (potentially blocking) approval callback off the dispatch thread,
/// bounded by [`APPROVAL_TIMEOUT`]. A callback that exceeds the budget, or one
/// whose join handle fails, resolves to `Deny`.
async fn decide_remotely(
    callback: ApprovalCallback,
    turn: Option<String>,
    invocation: ToolInvocation,
) -> ApprovalDecision {
    let join = tokio::task::spawn_blocking(move || callback(turn, &invocation));
    match tokio::time::timeout(APPROVAL_TIMEOUT, join).await {
        Ok(Ok(decision)) => decision,
        Ok(Err(_join_err)) => ApprovalDecision::Deny,
        Err(_elapsed) => ApprovalDecision::Deny,
    }
}

#[derive(Clone)]
pub struct AskApprovalHook {
    mode: SandboxMode,
    callback: Option<ApprovalCallback>,
}

impl AskApprovalHook {
    pub fn new(mode: SandboxMode) -> Self {
        Self {
            mode,
            callback: None,
        }
    }

    pub fn with_callback(mut self, callback: ApprovalCallback) -> Self {
        self.callback = Some(callback);
        self
    }

    pub fn set_callback(&mut self, callback: ApprovalCallback) {
        self.callback = Some(callback);
    }
}

impl Default for AskApprovalHook {
    fn default() -> Self {
        Self::new(SandboxMode::default())
    }
}

#[async_trait]
impl ToolDispatchHook for AskApprovalHook {
    async fn before_invocation(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        // Only Ask mode is approval-gated; other modes delegate to seatbelt.
        if self.mode != SandboxMode::Ask {
            return Ok(ToolDispatchAction::Continue);
        }

        // Gate only mutating/cloud tools (D26). Read-only local tools run direct.
        if !requires_approval(invocation) {
            return Ok(ToolDispatchAction::Continue);
        }

        // Fail-closed approvals (W5.2 D20 §10 + W12.28/A2): a sub-turn
        // running inside the task registry AND any MCP-surface invocation
        // (HookedMcpTool scopes MCP_CALL) have no originating approval UX —
        // an interactive prompt could only hang the MCP client or render on
        // an unrelated session's connection (T103 class hazard). Deny BEFORE
        // any callback/legacy-fallback path can claim a free route; the
        // audit line lands via the audit hook that observes the denial
        // action.
        let background = BACKGROUND_TASK.try_get().unwrap_or(false);
        let mcp = crate::task_registry::MCP_CALL.try_get().unwrap_or(false);
        if background || mcp {
            let origin = match (mcp, background) {
                (true, true) => "mcp+background",
                (true, false) => "mcp",
                (false, true) => "background",
                (false, false) => unreachable!("denied branch requires one marker set"),
            };
            let context = if mcp { "no approval UX" } else { "no surface" };
            return Ok(ToolDispatchAction::Terminate {
                reason: format!(
                    "approval denied ({origin}: {context}) — approval-required: {}",
                    invocation.name
                ),
            });
        }

        // No callback wired → behave as "direct": allow the invocation without
        // prompting (D29). Prompting happens only when the CLI/TUI registers a
        // callback (e.g. over a oneshot channel consumed inside the closure).
        let Some(callback) = self.callback.clone() else {
            return Ok(ToolDispatchAction::Continue);
        };

        // Turn affinity (SC-007): read inside the dispatch task, where the
        // hosted turn's scope is visible, and carry the value across the
        // spawn_blocking boundary explicitly. Unset outside hosted turns.
        let turn: Option<String> = APPROVAL_TURN.try_get().ok().flatten();
        let decision = decide_remotely(callback, turn, invocation.clone()).await;
        match decision {
            ApprovalDecision::Allow => Ok(ToolDispatchAction::Continue),
            ApprovalDecision::Deny => Ok(ToolDispatchAction::Terminate {
                reason: format!("user denied: {}", invocation.name),
            }),
        }
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
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rig_compose::normalizer::dispatch_tool_invocations_with_hooks;
    use rig_compose::registry::ToolRegistry;

    use crate::tools::audit_hook::ToolAuditHook;

    /// G-37 / FR-035 / SC-019: what a surface callback enqueues, in order —
    /// `(originating turn, tool name)` per would-be approval request.
    type ParentApprovalLog = Arc<Mutex<Vec<(Option<String>, String)>>>;

    fn make_invocation(name: &str) -> ToolInvocation {
        ToolInvocation {
            name: name.to_string(),
            args: serde_json::json!({}),
        }
    }

    /// PURPOSE: Drive ONE invocation through the real dispatch kernel with
    ///          the two stages this module owns wired in their production
    ///          order — audit immediately before approval (wiring.rs:733-738;
    ///          G-37 / FR-035 / SC-019). The empty registry is deliberate:
    ///          the deny path must never reach tool lookup, so a `Continue`
    ///          leak surfaces as a registry NotFound instead of a silent pass.
    /// USAGE: The three `background_denial_*` pins below; callers scope the
    ///        BACKGROUND_TASK / APPROVAL_TURN task-locals around the call.
    /// EXPECTED: `Err(KernelError::ToolDispatchTerminated(reason))` with the
    ///           structured `approval-required` reason.
    /// ERRORS: Any non-terminate error means the deny-first branch was not
    ///         taken; the pins treat that as a failure.
    async fn dispatch_audit_then_approval(
        audit: ToolAuditHook,
        approval: AskApprovalHook,
        invocation: ToolInvocation,
    ) -> Result<Vec<ToolInvocationResult>, KernelError> {
        let tools = ToolRegistry::new();
        let hooks: [&dyn ToolDispatchHook; 2] = [&audit, &approval];
        dispatch_tool_invocations_with_hooks(&tools, &[invocation], &hooks).await
    }

    /// Callback that records how many times it was invoked.
    fn counting_callback(allow: bool) -> (ApprovalCallback, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        let cb: ApprovalCallback = Arc::new(move |_turn, _inv| {
            count_clone.fetch_add(1, Ordering::SeqCst);
            if allow {
                ApprovalDecision::Allow
            } else {
                ApprovalDecision::Deny
            }
        });
        (cb, count)
    }

    #[tokio::test]
    async fn non_mutating_tool_bypasses_approval() {
        let (cb, count) = counting_callback(true);
        let hook = AskApprovalHook::new(SandboxMode::Ask).with_callback(cb);
        let inv = make_invocation("fs.read");
        let action = hook.before_invocation(&inv).await.unwrap();
        assert!(matches!(action, ToolDispatchAction::Continue));
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "read-only tool must not prompt"
        );
    }

    #[tokio::test]
    async fn mutating_tool_is_gated_and_can_be_denied() {
        let (cb, count) = counting_callback(false);
        let hook = AskApprovalHook::new(SandboxMode::Ask).with_callback(cb);
        let inv = make_invocation("fs.write");
        let action = hook.before_invocation(&inv).await.unwrap();
        assert!(matches!(action, ToolDispatchAction::Terminate { .. }));
        assert_eq!(count.load(Ordering::SeqCst), 1, "mutating tool must prompt");
    }

    #[tokio::test]
    async fn cloud_tool_is_gated() {
        let (cb, count) = counting_callback(false);
        let hook = AskApprovalHook::new(SandboxMode::Ask).with_callback(cb);
        let inv = make_invocation("web.search");
        let action = hook.before_invocation(&inv).await.unwrap();
        assert!(matches!(action, ToolDispatchAction::Terminate { .. }));
        assert_eq!(count.load(Ordering::SeqCst), 1, "cloud tool must prompt");
    }

    #[tokio::test]
    async fn ask_mode_without_callback_allows_mutating_directly() {
        let hook = AskApprovalHook::new(SandboxMode::Ask);
        let inv = make_invocation("fs.write");
        let action = hook.before_invocation(&inv).await.unwrap();
        assert!(
            matches!(action, ToolDispatchAction::Continue),
            "Ask mode without callback must default to direct (allow)"
        );
    }

    #[tokio::test]
    async fn non_ask_mode_never_gates() {
        let (cb, count) = counting_callback(false);
        let hook = AskApprovalHook::new(SandboxMode::WorkspaceWrite).with_callback(cb);
        let inv = make_invocation("fs.write");
        let action = hook.before_invocation(&inv).await.unwrap();
        assert!(matches!(action, ToolDispatchAction::Continue));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    /// W12.28 (A2): under the MCP-surface marker, an Ask-mode mutating
    /// invocation resolves deny + structured `approval-required` — never the
    /// callback-less direct-allow fallback (MCP clients have no approval UX).
    #[tokio::test]
    async fn mcp_call_marker_denies_mutating_without_callback() {
        let hook = AskApprovalHook::new(SandboxMode::Ask);
        let inv = make_invocation("fs.write");
        let action = crate::task_registry::MCP_CALL
            .scope(true, async { hook.before_invocation(&inv).await })
            .await
            .unwrap();
        let ToolDispatchAction::Terminate { reason } = action else {
            panic!("MCP-call mutating invocation must deny, got {action:?}");
        };
        assert!(reason.contains("approval-required"), "reason: {reason}");
        assert!(reason.contains("mcp:"), "reason: {reason}");
    }

    /// W12.28 (A2) happy path: read-only tools stay ungated under the MCP
    /// marker, and the legacy background message keeps its exact wording.
    #[tokio::test]
    async fn mcp_call_marker_leaves_readonly_ungated_and_background_wording_intact() {
        let hook = AskApprovalHook::new(SandboxMode::Ask);
        let read = make_invocation("fs.read");
        let action = crate::task_registry::MCP_CALL
            .scope(true, async { hook.before_invocation(&read).await })
            .await
            .unwrap();
        assert!(matches!(action, ToolDispatchAction::Continue));

        let write = make_invocation("fs.write");
        let background_only = BACKGROUND_TASK
            .scope(true, async { hook.before_invocation(&write).await })
            .await
            .unwrap();
        let ToolDispatchAction::Terminate { reason } = background_only else {
            panic!("background mutating invocation must deny, got {background_only:?}");
        };
        assert_eq!(
            reason, "approval denied (background: no surface) — approval-required: fs.write",
            "legacy background deny message must stay byte-identical"
        );
    }

    /// PURPOSE: Pin G-37 sub-claim (a) — a background-denied Ask approval
    ///          leaves an AUDIT record for the invocation (FR-035 / SC-019).
    ///          The audit stage precedes approval in the pipeline, and the
    ///          kernel's terminate path notifies every hook via
    ///          `on_invocation_error`, so the denial line is the existing
    ///          audit-hook record — asserted here end-to-end, not assumed.
    /// USAGE: Run under nextest (process-per-test: the ZEN_HOME env mutation
    ///        below is process-global).
    /// EXPECTED: Exactly one JSONL line: tool `fs.write`, `success: false`,
    ///           error embedding the same `approval-required` deny reason.
    /// ERRORS: Zero lines = the denial escaped the audit trail (feature gap).
    #[tokio::test]
    async fn background_denial_produces_an_audit_line() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: nextest runs one test per process; no other reader races
        // this process-global env mutation. The audit path is passed
        // explicitly below, so the hook never touches the real ~/.zen.
        unsafe {
            std::env::set_var("ZEN_HOME", dir.path());
        }
        zen_core::config::invalidate_config_cache();

        let audit_path = dir.path().join("logs").join("audit.jsonl");
        let audit = ToolAuditHook::new(audit_path.clone());
        let approval = AskApprovalHook::new(SandboxMode::Ask);

        let outcome = BACKGROUND_TASK
            .scope(
                true,
                dispatch_audit_then_approval(audit, approval, make_invocation("fs.write")),
            )
            .await;

        let Err(KernelError::ToolDispatchTerminated(reason)) = outcome else {
            panic!("background approval must terminate with the deny reason, got {outcome:?}");
        };
        assert!(reason.contains("approval-required"), "reason: {reason}");

        let written = std::fs::read_to_string(&audit_path).unwrap();
        let lines: Vec<&str> = written.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "exactly one audit line per denied invocation: {written}"
        );
        let record: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(record["tool"], "fs.write");
        assert_eq!(record["success"], false);
        let audited_error = record["error"].as_str().unwrap();
        assert!(
            audited_error.contains(&reason),
            "audit line must carry the denial reason: {audited_error}"
        );

        std::fs::remove_dir_all(dir.path()).ok();
    }

    /// PURPOSE: Pin G-37 sub-claim (b) — the background deny fires BEFORE any
    ///          approval callback, so it can never claim a free route
    ///          (FR-035 / SC-019). The pre-existing pin wires no callback,
    ///          which proves nothing about invocation; this one wires a
    ///          counting callback and asserts zero calls.
    /// USAGE: Same isolated dispatch path as pin (a).
    /// EXPECTED: Structured deny AND `callback_count == 0`.
    /// ERRORS: A callback count of 1 means the deny-first branch regressed
    ///         into the interactive path (the T103 hazard).
    #[tokio::test]
    async fn background_denial_never_invokes_the_callback() {
        let (callback, callback_count) = counting_callback(true);
        let dir = tempfile::tempdir().unwrap();
        let audit = ToolAuditHook::new(dir.path().join("audit.jsonl"));
        let approval = AskApprovalHook::new(SandboxMode::Ask).with_callback(callback);

        let outcome = BACKGROUND_TASK
            .scope(
                true,
                dispatch_audit_then_approval(audit, approval, make_invocation("fs.write")),
            )
            .await;

        let Err(KernelError::ToolDispatchTerminated(reason)) = outcome else {
            panic!("background approval must terminate with the deny reason, got {outcome:?}");
        };
        assert!(reason.contains("approval-required"), "reason: {reason}");
        assert_eq!(
            callback_count.load(Ordering::SeqCst),
            0,
            "background denial must never invoke the approval callback"
        );

        std::fs::remove_dir_all(dir.path()).ok();
    }

    /// PURPOSE: Pin G-37 sub-claim (c) — a background approval never enqueues
    ///          a request toward the parent/originating surface, so the parent
    ///          turn is neither cancelled nor blocked (FR-035 / SC-019).
    ///          Coverage is exactly this: the callback closure is the hook's
    ///          ONLY emission path to a surface, so a populated emission log
    ///          would mean a pending parent approval; the control half proves
    ///          the observer is live by enqueueing on the non-background path.
    /// USAGE: Same isolated dispatch path; APPROVAL_TURN models the parent's
    ///        turn affinity.
    /// EXPECTED: Background half — structured deny, emission log empty.
    ///           Control half — same callback/turn without the background
    ///           marker records one emission carrying the parent turn.
    /// ERRORS: A non-empty background emission log means the deny path leaked
    ///         an interactive request to the parent surface.
    #[tokio::test]
    async fn background_denial_leaves_no_parent_approval_pending() {
        let emitted: ParentApprovalLog = Arc::new(Mutex::new(Vec::new()));
        let emitted_by_marker = emitted.clone();
        let callback: ApprovalCallback = Arc::new(move |turn, invocation| {
            emitted_by_marker
                .lock()
                .unwrap()
                .push((turn, invocation.name.clone()));
            ApprovalDecision::Allow
        });

        let dir = tempfile::tempdir().unwrap();
        let audit = ToolAuditHook::new(dir.path().join("audit.jsonl"));
        let approval = AskApprovalHook::new(SandboxMode::Ask).with_callback(callback.clone());

        let outcome = BACKGROUND_TASK
            .scope(
                true,
                APPROVAL_TURN.scope(
                    Some("parent-turn".to_string()),
                    dispatch_audit_then_approval(audit, approval, make_invocation("fs.write")),
                ),
            )
            .await;

        let Err(KernelError::ToolDispatchTerminated(reason)) = outcome else {
            panic!("background approval must terminate with the deny reason, got {outcome:?}");
        };
        assert!(reason.contains("approval-required"), "reason: {reason}");
        assert!(
            emitted.lock().unwrap().is_empty(),
            "background denial must enqueue no approval request toward the parent surface"
        );

        let control_audit = ToolAuditHook::new(dir.path().join("audit-control.jsonl"));
        let control_approval =
            AskApprovalHook::new(SandboxMode::Ask).with_callback(callback.clone());
        let control = APPROVAL_TURN
            .scope(
                Some("parent-turn".to_string()),
                dispatch_audit_then_approval(
                    control_audit,
                    control_approval,
                    make_invocation("fs.write"),
                ),
            )
            .await;
        assert!(
            control.is_err(),
            "control path runs the callback then hits the empty registry"
        );
        assert_eq!(
            *emitted.lock().unwrap(),
            vec![(Some("parent-turn".to_string()), "fs.write".to_string())],
            "control half must prove the emission observer is live"
        );

        std::fs::remove_dir_all(dir.path()).ok();
    }
}
