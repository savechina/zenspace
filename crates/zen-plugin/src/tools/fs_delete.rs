use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsDeleteTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.delete";
const DESCRIPTION: &str = "Delete a file or empty directory";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Path to delete" },
            "recursive": { "type": "boolean", "description": "Allow deleting non-empty directories (default false)" }
        },
        "required": ["path"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "deleted": { "type": "boolean" },
            "was_dir": { "type": "boolean" }
        }
    })
});

impl FsDeleteTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

#[async_trait]
impl Tool for FsDeleteTool {
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

        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = PathBuf::from(path_str);
        self.validator
            .validate_path_for_write(&path)
            .map_err(KernelError::ToolFailed)?;

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("Failed to stat {}: {}", path_str, e)))?;

        let was_dir = meta.is_dir();

        if was_dir && !recursive {
            let is_empty = tokio::fs::read_dir(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to read dir: {}", e)))?
                .next_entry()
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to read entry: {}", e)))?
                .is_none();

            if !is_empty {
                return Err(KernelError::InvalidArgument(format!(
                    "Directory {} is not empty (use recursive=true to force)",
                    path_str
                )));
            }
        }

        if was_dir && recursive {
            tokio::fs::remove_dir_all(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to remove dir: {}", e)))?;
        } else if was_dir {
            tokio::fs::remove_dir(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to remove dir: {}", e)))?;
        } else {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to delete file: {}", e)))?;
        }

        Ok(json!({
            "path": path_str,
            "deleted": true,
            "was_dir": was_dir
        }))
    }
}
