use std::time::Duration;

use async_trait::async_trait;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
};
use rig_compose::registry::KernelError;

use zen_core::sandbox::{ApprovalCallback, ApprovalDecision, SandboxMode};

use crate::tools::confidentiality_hook::{is_cloud_tool, is_mutating_tool};

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
    invocation: ToolInvocation,
) -> ApprovalDecision {
    let join = tokio::task::spawn_blocking(move || callback(&invocation));
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

        // No callback wired → behave as "direct": allow the invocation without
        // prompting (D29). Prompting happens only when the CLI/TUI registers a
        // callback (e.g. over a oneshot channel consumed inside the closure).
        let Some(callback) = self.callback.clone() else {
            return Ok(ToolDispatchAction::Continue);
        };

        let decision = decide_remotely(callback, invocation.clone()).await;
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_invocation(name: &str) -> ToolInvocation {
        ToolInvocation {
            name: name.to_string(),
            args: serde_json::json!({}),
        }
    }

    /// Callback that records how many times it was invoked.
    fn counting_callback(allow: bool) -> (ApprovalCallback, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        let cb: ApprovalCallback = Arc::new(move |_inv| {
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
}
