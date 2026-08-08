use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsReadTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.read";
const DESCRIPTION: &str = "Read file contents with optional line offset and limit";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Absolute or workspace-relative file path" },
            "offset": { "type": "integer", "description": "Starting line number (1-based, default 1)" },
            "limit": { "type": "integer", "description": "Maximum lines to read (default 2000)" }
        },
        "required": ["path"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "content": { "type": "string" },
            "lines_read": { "type": "integer" },
            "total_lines": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "is_binary": { "type": "boolean" }
        }
    })
});

impl FsReadTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

const BINARY_CHECK_BYTES: usize = 1024;
const DEFAULT_LIMIT: usize = 2000;

fn is_binary(data: &[u8]) -> bool {
    let check = &data[..data.len().min(BINARY_CHECK_BYTES)];
    check.contains(&0x00)
}

#[async_trait]
impl Tool for FsReadTool {
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

        let offset = args.get("offset").and_then(|v| v.as_i64()).unwrap_or(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_LIMIT as i64) as usize;

        let path = PathBuf::from(path_str);

        self.validator
            .validate_path_for_read(&path)
            .map_err(KernelError::ToolFailed)?;

        let content = tokio::fs::read(&path)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("Failed to read {}: {}", path_str, e)))?;

        if is_binary(&content) {
            return Ok(json!({
                "path": path_str,
                "content": "",
                "lines_read": 0,
                "total_lines": 0,
                "truncated": false,
                "is_binary": true
            }));
        }

        let text = String::from_utf8_lossy(&content);
        let all_lines: Vec<&str> = text.lines().collect();
        let total_lines = all_lines.len();

        let start = offset.saturating_sub(1).min(total_lines);
        let end = (start + limit).min(total_lines);
        let selected: Vec<&str> = all_lines[start..end].to_vec();
        let lines_read = selected.len();

        Ok(json!({
            "path": path_str,
            "content": selected.join("\n"),
            "lines_read": lines_read,
            "total_lines": total_lines,
            "truncated": end < total_lines,
            "is_binary": false
        }))
    }
}
