use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsCopyTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.copy";
const DESCRIPTION: &str = "Copy a file to a new location within the workspace";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "source": { "type": "string", "description": "Source file path" },
            "destination": { "type": "string", "description": "Destination path" },
            "overwrite": { "type": "boolean", "description": "Overwrite if destination exists (default false)" }
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
            "copied": { "type": "boolean" },
            "bytes_copied": { "type": "integer" }
        }
    })
});

impl FsCopyTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

#[async_trait]
impl Tool for FsCopyTool {
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

        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let source = PathBuf::from(source_str);
        let dest = PathBuf::from(dest_str);

        self.validator
            .validate_path_for_read(&source)
            .map_err(KernelError::ToolFailed)?;
        self.validator
            .validate_path_for_write(&dest)
            .map_err(KernelError::ToolFailed)?;

        if dest.exists() && !overwrite {
            return Err(KernelError::InvalidArgument(format!(
                "Destination {} already exists (use overwrite=true)",
                dest_str
            )));
        }

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                KernelError::ToolFailed(format!("Failed to create parent dir: {}", e))
            })?;
        }

        let bytes = tokio::fs::copy(&source, &dest)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("Failed to copy: {}", e)))?;

        Ok(json!({
            "source": source_str,
            "destination": dest_str,
            "copied": true,
            "bytes_copied": bytes
        }))
    }
}
