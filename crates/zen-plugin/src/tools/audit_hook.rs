use async_trait::async_trait;
use chrono::Utc;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
};
use rig_compose::registry::KernelError;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Clone)]
pub struct ToolAuditHook {
    log_path: PathBuf,
    writer: Arc<Mutex<()>>,
    start_time: Arc<Mutex<Option<Instant>>>,
}

impl ToolAuditHook {
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            log_path,
            writer: Arc::new(Mutex::new(())),
            start_time: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for ToolAuditHook {
    fn default() -> Self {
        Self::new(PathBuf::from("logs/audit.jsonl"))
    }
}

// FR-048c: the hook-isolation adapter (hook_isolation.rs) must write denial
// records through this same append path so all audit lines stay uniform.
pub(crate) fn arg_keys(args: &Value) -> Vec<String> {
    match args {
        Value::Object(map) => map.keys().cloned().collect(),
        _ => vec![],
    }
}

/// Build a JSONL audit record. `duration_ms` is `None` when no matching
/// `before_invocation` start was captured (e.g. an error path that bypassed
/// the before hook); the field is omitted from the JSON in that case to keep
/// the shape backward-compatible with older readers.
fn build_record(
    invocation: &ToolInvocation,
    success: bool,
    error: Option<&KernelError>,
    duration_ms: Option<u64>,
) -> Value {
    let mut record = json!({
        "timestamp": Utc::now().to_rfc3339(),
        "tool": invocation.name,
        "args_summary": arg_keys(&invocation.args),
        "success": success,
        "error": error.map(|e| e.to_string()),
    });
    if let Some(ms) = duration_ms {
        record["duration_ms"] = json!(ms);
    }
    record
}

// FR-048c: shared with the hook-isolation adapter for uniform audit appends.
pub(crate) fn append_record(record: &Value, log_path: &PathBuf) {
    let line = match serde_json::to_string(record) {
        Ok(mut s) => {
            s.push('\n');
            s
        }
        Err(_) => return,
    };
    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(log_path)
    {
        Ok(mut file) => {
            use std::io::Write;
            if let Err(e) = file.write_all(line.as_bytes()) {
                warn!(error = %e, "audit: failed to write record");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let created = log_path
                .parent()
                .map(std::fs::create_dir_all)
                .is_some_and(|r| r.is_ok());
            if created
                && let Ok(mut file) = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(log_path)
            {
                use std::io::Write;
                let _ = file.write_all(line.as_bytes());
            }
        }
        Err(e) => {
            warn!(error = %e, path = %log_path.display(), "audit: failed to open log file");
        }
    }
}

#[async_trait]
impl ToolDispatchHook for ToolAuditHook {
    async fn before_invocation(
        &self,
        _invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        let mut guard = self.start_time.lock().await;
        *guard = Some(Instant::now());
        Ok(ToolDispatchAction::Continue)
    }

    async fn after_invocation(&self, result: &ToolInvocationResult) -> Result<(), KernelError> {
        let duration_ms = self.take_elapsed().await.map(|e| e.as_millis() as u64);
        let record = build_record(&result.invocation, true, None, duration_ms);
        let _guard = self.writer.lock().await;
        append_record(&record, &self.log_path);
        Ok(())
    }

    async fn on_invocation_error(
        &self,
        invocation: &ToolInvocation,
        error: &KernelError,
    ) -> Result<(), KernelError> {
        let duration_ms = self.take_elapsed().await.map(|e| e.as_millis() as u64);
        let record = build_record(invocation, false, Some(error), duration_ms);
        let _guard = self.writer.lock().await;
        append_record(&record, &self.log_path);
        Ok(())
    }
}

impl ToolAuditHook {
    async fn take_elapsed(&self) -> Option<Duration> {
        let mut guard = self.start_time.lock().await;
        guard
            .take()
            .map(|start| Instant::now().saturating_duration_since(start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_invocation(name: &str) -> ToolInvocation {
        ToolInvocation {
            name: name.to_string(),
            args: json!({"path": "/workspace/notes.md"}),
        }
    }

    #[tokio::test]
    async fn record_includes_duration_ms_after_normal_dispatch() {
        let dir = std::env::temp_dir().join(format!(
            "zen_audit_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("audit.jsonl");
        let hook = ToolAuditHook::new(log_path.clone());

        let inv = make_invocation("fs.write");
        hook.before_invocation(&inv).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let result = ToolInvocationResult {
            invocation: inv,
            output: json!({"bytes_written": 4}),
        };
        hook.after_invocation(&result).await.unwrap();

        let written = std::fs::read_to_string(&log_path).unwrap();
        assert!(written.contains("\"duration_ms\""), "line: {written}");
        let parsed: Value = serde_json::from_str(written.trim()).unwrap();
        assert_eq!(parsed["tool"], "fs.write");
        let ms = parsed["duration_ms"].as_u64();
        assert!(
            ms.is_some_and(|m| m < 60_000),
            "duration_ms out of range: {ms:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn record_omits_duration_when_before_did_not_fire() {
        let dir = std::env::temp_dir().join(format!(
            "zen_audit_nostart_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("audit.jsonl");
        let hook = ToolAuditHook::new(log_path.clone());

        let inv = make_invocation("fs.read");
        let result = ToolInvocationResult {
            invocation: inv,
            output: json!({}),
        };
        hook.after_invocation(&result).await.unwrap();

        let written = std::fs::read_to_string(&log_path).unwrap();
        let parsed: Value = serde_json::from_str(written.trim()).unwrap();
        assert!(parsed.get("duration_ms").is_none(), "line: {written}");
        assert_eq!(parsed["tool"], "fs.read");
        std::fs::remove_dir_all(&dir).ok();
    }
}
