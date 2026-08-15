//! Plugin hook isolation (FR-048, Lane C T101–T103).
//!
//! rig-compose runs dispatch hooks sequentially and aborts the ENTIRE
//! dispatch round on the first hook `Err` (`normalizer.rs` `dispatch_inner`).
//! An erroring plugin hook must not kill every other tool call in the
//! round, so every hook registered through
//! [`crate::PluginApi::register_hook`] is wrapped in
//! [`IsolatedPluginHook`]:
//!
//! - **T101 (FR-048a)** — a wrapped-hook `Err` is translated into a deny
//!   for THAT invocation only (fail-closed per-call) using the same
//!   mechanism as the builtin confidentiality hook: a
//!   [`ToolDispatchAction::Skip`] carrying an error payload. The `Err`
//!   never propagates upward, so sibling invocations in the same round
//!   keep dispatching.
//! - **T102 (FR-048b)** — plugin hooks never observe
//!   [`Sensitivity::Confidential`] invocations (e.g. `shell.exec`): the
//!   wrapped hook's callbacks are simply not called and the adapter
//!   returns the neutral decision of a no-op hook.
//! - **T103 (FR-048c)** — denials append an audit-correlated JSONL record
//!   to `logs/audit.jsonl` (same append path as the builtin audit hook)
//!   with `outcome: "denied_by_plugin_hook"`.
//!
//! Known accepted limitation (spec FR-048): a hook `panic!` is NOT caught;
//! a panicking hook fails loudly process-wide, never silently.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationOutcome,
    ToolInvocationResult,
};
use rig_compose::registry::KernelError;
use serde_json::{Value, json};
use tracing::warn;
use zen_core::types::Sensitivity;

use crate::tools::audit_hook::{append_record, arg_keys};

/// Shared tool-name → [`Sensitivity`] table injected by `ZenWiring` (T102).
///
/// Lookups default to [`Sensitivity::Private`] — the same default as
/// `ZenWiring::tool_sensitivity` — so unknown and plugin-registered tools
/// stay visible to plugin hooks; only explicitly `Confidential` tools are
/// hidden.
pub type ToolSensitivityMap = Arc<HashMap<String, Sensitivity>>;

/// Audit outcome tag for invocations denied by an erroring plugin hook.
pub const DENIED_BY_PLUGIN_HOOK: &str = "denied_by_plugin_hook";

/// Isolation adapter wrapped around every plugin-registered dispatch hook
/// at [`crate::PluginApi::register_hook`] exit (FR-048).
pub struct IsolatedPluginHook {
    plugin_id: String,
    inner: Box<dyn ToolDispatchHook>,
    sensitivity: ToolSensitivityMap,
    audit_log_path: PathBuf,
}

impl IsolatedPluginHook {
    /// Wrap `inner` (a plugin-supplied hook) for plugin `plugin_id`.
    ///
    /// `sensitivity` is the tool-sensitivity table consulted per
    /// invocation; `audit_log_path` is the `logs/audit.jsonl` target the
    /// denial records are appended to (same file the builtin audit hook
    /// writes).
    pub fn new(
        plugin_id: impl Into<String>,
        inner: Box<dyn ToolDispatchHook>,
        sensitivity: ToolSensitivityMap,
        audit_log_path: PathBuf,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            inner,
            sensitivity,
            audit_log_path,
        }
    }

    fn is_confidential(&self, invocation: &ToolInvocation) -> bool {
        self.sensitivity
            .get(&invocation.name)
            .copied()
            .unwrap_or(Sensitivity::Private)
            == Sensitivity::Confidential
    }

    /// Translate a wrapped-hook `Err` into a per-invocation deny (T101) and
    /// append the audit-correlated denial record (T103).
    fn deny(&self, invocation: &ToolInvocation, error: &KernelError) -> ToolDispatchAction {
        let message = format!(
            "invocation of '{}' denied by plugin hook '{}': {}",
            invocation.name, self.plugin_id, error
        );
        warn!(
            plugin = %self.plugin_id,
            tool = %invocation.name,
            error = %error,
            "plugin hook failed; denying this invocation only (FR-048)"
        );
        append_record(&self.denial_record(invocation, error), &self.audit_log_path);
        // Deny mechanism mirrors the builtin confidentiality hook: Skip
        // with an error payload denies THIS invocation while the dispatch
        // round continues for other tools.
        ToolDispatchAction::Skip {
            output: json!({ "error": message }),
            reason: Some(message),
        }
    }

    /// JSONL denial record in the same field style as the builtin audit
    /// hook records (`timestamp` / `tool` / `args_summary` / `success` /
    /// `error`), correlated with the invocation and the offending plugin
    /// hook. The hook trait exposes no invocation id, so correlation rides
    /// on `tool` + `args_summary` + `timestamp`, exactly like the builtin
    /// audit records written for the same round.
    fn denial_record(&self, invocation: &ToolInvocation, error: &KernelError) -> Value {
        json!({
            "timestamp": Utc::now().to_rfc3339(),
            "tool": invocation.name,
            "args_summary": arg_keys(&invocation.args),
            "success": false,
            "outcome": DENIED_BY_PLUGIN_HOOK,
            "plugin": self.plugin_id,
            "error": error.to_string(),
        })
    }
}

#[async_trait]
impl ToolDispatchHook for IsolatedPluginHook {
    async fn before_invocation(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        if self.is_confidential(invocation) {
            // FR-048b: plugin hooks never observe Confidential invocations.
            // Neutral no-op decision; the wrapped hook is not called.
            return Ok(ToolDispatchAction::Continue);
        }
        match self.inner.before_invocation(invocation).await {
            Ok(action) => Ok(action),
            Err(error) => Ok(self.deny(invocation, &error)),
        }
    }

    async fn after_invocation(&self, result: &ToolInvocationResult) -> Result<(), KernelError> {
        if self.is_confidential(&result.invocation) {
            return Ok(());
        }
        if let Err(error) = self.inner.after_invocation(result).await {
            warn!(
                plugin = %self.plugin_id,
                tool = %result.invocation.name,
                error = %error,
                "plugin hook after_invocation failed; result kept, error swallowed (FR-048)"
            );
        }
        Ok(())
    }

    async fn after_invocation_with_outcome(
        &self,
        result: &ToolInvocationResult,
        outcome: &ToolInvocationOutcome,
    ) -> Result<(), KernelError> {
        if self.is_confidential(&result.invocation) {
            return Ok(());
        }
        if let Err(error) = self
            .inner
            .after_invocation_with_outcome(result, outcome)
            .await
        {
            warn!(
                plugin = %self.plugin_id,
                tool = %result.invocation.name,
                error = %error,
                "plugin hook after_invocation_with_outcome failed; result kept, error swallowed (FR-048)"
            );
        }
        Ok(())
    }

    async fn on_invocation_error(
        &self,
        invocation: &ToolInvocation,
        error: &KernelError,
    ) -> Result<(), KernelError> {
        if self.is_confidential(invocation) {
            // The wrapped hook never observed this invocation, so it has
            // nothing to clean up.
            return Ok(());
        }
        if let Err(cleanup_error) = self.inner.on_invocation_error(invocation, error).await {
            warn!(
                plugin = %self.plugin_id,
                tool = %invocation.name,
                error = %cleanup_error,
                "plugin hook on_invocation_error failed; error swallowed (FR-048)"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Spy hook that records every callback it receives and can be made to
    /// fail on demand.
    struct SpyHook {
        observed: Arc<Mutex<Vec<String>>>,
        fail_before: bool,
        fail_after: bool,
        fail_on_error: bool,
    }

    impl SpyHook {
        fn new(observed: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                observed,
                fail_before: false,
                fail_after: false,
                fail_on_error: false,
            }
        }

        fn failing(mut self) -> Self {
            self.fail_before = true;
            self
        }
    }

    #[async_trait]
    impl ToolDispatchHook for SpyHook {
        async fn before_invocation(
            &self,
            invocation: &ToolInvocation,
        ) -> Result<ToolDispatchAction, KernelError> {
            if self.fail_before {
                return Err(KernelError::ToolFailed("plugin hook exploded".to_string()));
            }
            self.observed
                .lock()
                .unwrap()
                .push(format!("before:{}", invocation.name));
            Ok(ToolDispatchAction::Continue)
        }

        async fn after_invocation(&self, result: &ToolInvocationResult) -> Result<(), KernelError> {
            if self.fail_after {
                return Err(KernelError::ToolFailed("after blew up".to_string()));
            }
            self.observed
                .lock()
                .unwrap()
                .push(format!("after:{}", result.invocation.name));
            Ok(())
        }

        async fn on_invocation_error(
            &self,
            invocation: &ToolInvocation,
            _error: &KernelError,
        ) -> Result<(), KernelError> {
            if self.fail_on_error {
                return Err(KernelError::ToolFailed("cleanup blew up".to_string()));
            }
            self.observed
                .lock()
                .unwrap()
                .push(format!("on_error:{}", invocation.name));
            Ok(())
        }
    }

    fn invocation(name: &str) -> ToolInvocation {
        ToolInvocation {
            name: name.to_string(),
            args: json!({ "path": "/ws/notes.md" }),
        }
    }

    fn result_of(name: &str) -> ToolInvocationResult {
        ToolInvocationResult {
            invocation: invocation(name),
            output: json!({ "ok": true }),
        }
    }

    fn sensitivity_map(entries: &[(&str, Sensitivity)]) -> ToolSensitivityMap {
        Arc::new(
            entries
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect::<HashMap<_, _>>(),
        )
    }

    fn adapter(inner: SpyHook, audit: &std::path::Path) -> IsolatedPluginHook {
        IsolatedPluginHook::new(
            "glitchy",
            Box::new(inner),
            sensitivity_map(&[("shell.exec", Sensitivity::Confidential)]),
            audit.to_path_buf(),
        )
    }

    // ── T101 (FR-048a): Err → deny THIS invocation, never propagate ──────

    #[tokio::test]
    async fn erring_hook_before_is_translated_to_deny() {
        let dir = tempfile::tempdir().unwrap();
        let audit = dir.path().join("audit.jsonl");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hook = adapter(SpyHook::new(observed.clone()).failing(), &audit);

        match hook.before_invocation(&invocation("fs.read")).await {
            Ok(ToolDispatchAction::Skip { output, reason }) => {
                let msg = output["error"].as_str().unwrap();
                assert!(msg.contains("denied by plugin hook 'glitchy'"), "{msg}");
                assert!(reason.unwrap().contains("fs.read"));
            }
            other => panic!("hook Err must become Skip deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn healthy_hook_actions_pass_through_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let audit = dir.path().join("audit.jsonl");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hook = adapter(SpyHook::new(observed), &audit);

        assert!(matches!(
            hook.before_invocation(&invocation("fs.read"))
                .await
                .unwrap(),
            ToolDispatchAction::Continue
        ));
    }

    #[tokio::test]
    async fn after_phase_err_is_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let audit = dir.path().join("audit.jsonl");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut spy = SpyHook::new(observed);
        spy.fail_after = true;
        let hook = adapter(spy, &audit);

        assert!(hook.after_invocation(&result_of("fs.read")).await.is_ok());
    }

    #[tokio::test]
    async fn on_invocation_error_err_is_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let audit = dir.path().join("audit.jsonl");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut spy = SpyHook::new(observed);
        spy.fail_on_error = true;
        let hook = adapter(spy, &audit);

        assert!(
            hook.on_invocation_error(
                &invocation("fs.read"),
                &KernelError::ToolDispatchTerminated("stop".to_string())
            )
            .await
            .is_ok()
        );
    }

    // ── T102 (FR-048b): Confidential invocations are invisible ───────────

    #[tokio::test]
    async fn confidential_invocation_is_invisible_to_plugin_hook() {
        let dir = tempfile::tempdir().unwrap();
        let audit = dir.path().join("audit.jsonl");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hook = adapter(SpyHook::new(observed.clone()), &audit);

        // shell.exec is Confidential in the injected table: no callback
        // fires and before returns the neutral no-op decision.
        assert!(matches!(
            hook.before_invocation(&invocation("shell.exec"))
                .await
                .unwrap(),
            ToolDispatchAction::Continue
        ));
        hook.after_invocation(&result_of("shell.exec"))
            .await
            .unwrap();
        hook.on_invocation_error(
            &invocation("shell.exec"),
            &KernelError::ToolDispatchTerminated("stop".to_string()),
        )
        .await
        .unwrap();

        assert!(
            observed.lock().unwrap().is_empty(),
            "spy must observe nothing for a Confidential invocation"
        );

        // Non-confidential invocations are fully visible.
        hook.before_invocation(&invocation("fs.read"))
            .await
            .unwrap();
        hook.after_invocation(&result_of("fs.read")).await.unwrap();
        assert_eq!(
            *observed.lock().unwrap(),
            vec!["before:fs.read", "after:fs.read"]
        );
    }

    // ── T103 (FR-048c): audit-correlated denial records ──────────────────

    #[tokio::test]
    async fn denial_writes_audit_record() {
        let dir = tempfile::tempdir().unwrap();
        let audit = dir.path().join("logs").join("audit.jsonl");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hook = adapter(SpyHook::new(observed).failing(), &audit);

        hook.before_invocation(&invocation("fs.read"))
            .await
            .unwrap();

        let text = std::fs::read_to_string(&audit).unwrap();
        let record: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(record["tool"], "fs.read");
        assert_eq!(record["outcome"], DENIED_BY_PLUGIN_HOOK);
        assert_eq!(record["plugin"], "glitchy");
        assert_eq!(record["success"], false);
        assert_eq!(record["args_summary"], json!(["path"]));
        assert!(record["timestamp"].as_str().is_some_and(|t| !t.is_empty()));
        assert!(record["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
}
