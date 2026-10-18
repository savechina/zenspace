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
        let source_str = match args["source"].as_str() {
            Some(s) => s,
            None => return Ok(json!({ "error": "Missing or invalid 'source' field" })),
        };

        let dest_str = match args["destination"].as_str() {
            Some(d) => d,
            None => return Ok(json!({ "error": "Missing or invalid 'destination' field" })),
        };

        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let source = zen_core::sandbox::resolve_agent_path(source_str);
        let dest = zen_core::sandbox::resolve_agent_path(dest_str);

        self.validator
            .validate_path_for_read(&source)
            .map_err(KernelError::ToolFailed)?;
        self.validator
            .validate_path_for_write(&dest)
            .map_err(KernelError::ToolFailed)?;

        if dest.exists() && !overwrite {
            return Ok(json!({
                "error": format!(
                    "Destination {} already exists (use overwrite=true)",
                    dest_str
                )
            }));
        }

        if let Some(parent) = dest.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(json!({ "error": format!("Failed to create parent dir: {}", e) }));
        }

        let bytes = match tokio::fs::copy(&source, &dest).await {
            Ok(b) => b,
            Err(e) => return Ok(json!({ "error": format!("Failed to copy: {}", e) })),
        };

        Ok(json!({
            "source": source_str,
            "destination": dest_str,
            "copied": true,
            "bytes_copied": bytes
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_validator(dir: &std::path::Path) -> SandboxValidator {
        SandboxValidator::new(
            zen_core::sandbox::SandboxMode::WorkspaceWrite,
            vec![dir.to_path_buf()],
        )
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(fut)
    }

    #[test]
    fn existing_destination_returns_structured_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("src.txt"), "a").unwrap();
        std::fs::write(dir.path().join("dst.txt"), "b").unwrap();
        let tool = FsCopyTool::new(make_validator(dir.path()));
        let res = block_on(tool.invoke(json!({
            "source": dir.path().join("src.txt").to_string_lossy(),
            "destination": dir.path().join("dst.txt").to_string_lossy()
        })))
        .unwrap();
        assert!(
            res["error"]
                .as_str()
                .is_some_and(|m| m.contains("already exists")),
            "expected structured error, got: {res}"
        );
    }
}
