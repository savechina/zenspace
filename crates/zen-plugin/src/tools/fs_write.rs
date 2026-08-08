use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsWriteTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.write";
const DESCRIPTION: &str = "Create or overwrite a file with the given content";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "File path to write" },
            "content": { "type": "string", "description": "Full file content" },
            "create_dirs": { "type": "boolean", "description": "Create parent directories if missing (default true)" }
        },
        "required": ["path", "content"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "bytes_written": { "type": "integer" },
            "created": { "type": "boolean" }
        }
    })
});

impl FsWriteTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

#[async_trait]
impl Tool for FsWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let path_str = args["path"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'path' field".into())
        })?;

        let content = args["content"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'content' field".into())
        })?;

        let create_dirs = args
            .get("create_dirs")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let path = PathBuf::from(path_str);
        let existed = path.exists();

        self.validator
            .validate_path_for_write(&path)
            .map_err(KernelError::ToolFailed)?;

        if create_dirs && let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to create dirs: {}", e)))?;
        }

        let bytes = content.as_bytes();
        let bytes_written = bytes.len();

        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("Failed to write {}: {}", path_str, e)))?;

        Ok(json!({
            "path": path_str,
            "bytes_written": bytes_written,
            "created": !existed
        }))
    }
}
