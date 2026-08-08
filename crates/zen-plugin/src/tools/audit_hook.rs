use async_trait::async_trait;
use chrono::Utc;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
};
use rig_compose::registry::KernelError;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Clone)]
pub struct ToolAuditHook {
    log_path: PathBuf,
    writer: Arc<Mutex<()>>,
}

impl ToolAuditHook {
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            log_path,
            writer: Arc::new(Mutex::new(())),
        }
    }
}

impl Default for ToolAuditHook {
    fn default() -> Self {
        Self::new(PathBuf::from("logs/audit.jsonl"))
    }
}

fn arg_keys(args: &Value) -> Vec<String> {
    match args {
        Value::Object(map) => map.keys().cloned().collect(),
        _ => vec![],
    }
}

fn build_record(invocation: &ToolInvocation, success: bool, error: Option<&KernelError>) -> Value {
    json!({
        "timestamp": Utc::now().to_rfc3339(),
        "tool": invocation.name,
        "args_summary": arg_keys(&invocation.args),
        "success": success,
        "error": error.map(|e| e.to_string()),
    })
}

fn append_record(record: &Value, log_path: &PathBuf) {
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
        Ok(ToolDispatchAction::Continue)
    }

    async fn after_invocation(&self, result: &ToolInvocationResult) -> Result<(), KernelError> {
        let record = build_record(&result.invocation, true, None);
        let _guard = self.writer.lock().await;
        append_record(&record, &self.log_path);
        Ok(())
    }

    async fn on_invocation_error(
        &self,
        invocation: &ToolInvocation,
        error: &KernelError,
    ) -> Result<(), KernelError> {
        let record = build_record(invocation, false, Some(error));
        let _guard = self.writer.lock().await;
        append_record(&record, &self.log_path);
        Ok(())
    }
}
