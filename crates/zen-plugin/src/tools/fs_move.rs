use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsMoveTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.move";
const DESCRIPTION: &str = "Move or rename a file/directory within the workspace";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "source": { "type": "string", "description": "Source path" },
            "destination": { "type": "string", "description": "Destination path" }
        },
        "required": ["source", "destination"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "source": { "type": "string" },
            "destination": { "type": "string" },
            "moved": { "type": "boolean" }
        }
    })
});

impl FsMoveTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

#[async_trait]
impl Tool for FsMoveTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let source_str = args["source"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'source' field".into())
        })?;

        let dest_str = args["destination"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'destination' field".into())
        })?;

        let source = PathBuf::from(source_str);
        let dest = PathBuf::from(dest_str);

        self.validator
            .validate_path_for_read(&source)
            .map_err(KernelError::ToolFailed)?;
        self.validator
            .validate_path_for_write(&source)
            .map_err(KernelError::ToolFailed)?;
        self.validator
            .validate_path_for_write(&dest)
            .map_err(KernelError::ToolFailed)?;

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                KernelError::ToolFailed(format!("Failed to create parent dir: {}", e))
            })?;
        }

        tokio::fs::rename(&source, &dest)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("Failed to move: {}", e)))?;

        Ok(json!({
            "source": source_str,
            "destination": dest_str,
            "moved": true
        }))
    }
}
