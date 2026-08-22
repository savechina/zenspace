use std::sync::Arc;

use zen_core::sandbox::{ApprovalCallback, ApprovalDecision};

pub fn create_approval_callback() -> ApprovalCallback {
    Arc::new(move |invocation| {
        tracing::warn!(
            tool = invocation.name,
            "approval requested (non-interactive: denying by default)"
        );
        ApprovalDecision::Deny
    })
}
