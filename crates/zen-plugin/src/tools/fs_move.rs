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
        let source_str = match args["source"].as_str() {
            Some(s) => s,
            None => return Ok(json!({ "error": "Missing or invalid 'source' field" })),
        };

        let dest_str = match args["destination"].as_str() {
            Some(d) => d,
            None => return Ok(json!({ "error": "Missing or invalid 'destination' field" })),
        };

        let source = zen_core::sandbox::resolve_agent_path(source_str);
        let dest = zen_core::sandbox::resolve_agent_path(dest_str);

        self.validator
            .validate_path_for_read(&source)
            .map_err(KernelError::ToolFailed)?;
        self.validator
            .validate_path_for_write(&source)
            .map_err(KernelError::ToolFailed)?;
        self.validator
            .validate_path_for_write(&dest)
            .map_err(KernelError::ToolFailed)?;

        if let Some(parent) = dest.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(json!({ "error": format!("Failed to create parent dir: {}", e) }));
        }

        if let Err(e) = tokio::fs::rename(&source, &dest).await {
            return Ok(json!({ "error": format!("Failed to move: {}", e) }));
        }

        Ok(json!({
            "source": source_str,
            "destination": dest_str,
            "moved": true
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
    fn missing_source_returns_structured_error() {
        let dir = tempdir().unwrap();
        let tool = FsMoveTool::new(make_validator(dir.path()));
        let res = block_on(tool.invoke(json!({
            "source": dir.path().join("does-not-exist.txt").to_string_lossy(),
            "destination": dir.path().join("out.txt").to_string_lossy()
        })))
        .unwrap();
        assert!(
            res["error"]
                .as_str()
                .is_some_and(|m| m.contains("Failed to move")),
            "expected structured error, got: {res}"
        );
    }
}
