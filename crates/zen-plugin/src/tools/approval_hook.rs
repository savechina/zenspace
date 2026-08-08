use async_trait::async_trait;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
};
use rig_compose::registry::KernelError;

use zen_core::sandbox::{ApprovalCallback, ApprovalDecision, SandboxMode};

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
        if self.mode != SandboxMode::Ask {
            return Ok(ToolDispatchAction::Continue);
        }

        match &self.callback {
            Some(cb) => match cb(invocation) {
                ApprovalDecision::Allow => Ok(ToolDispatchAction::Continue),
                ApprovalDecision::Deny => Ok(ToolDispatchAction::Terminate {
                    reason: format!("user denied: {}", invocation.name),
                }),
            },
            None => Ok(ToolDispatchAction::Terminate {
                reason: format!(
                    "ask mode requires approval callback (none set): {}",
                    invocation.name
                ),
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
