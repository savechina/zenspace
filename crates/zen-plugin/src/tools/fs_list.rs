use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsListTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.list";
const DESCRIPTION: &str = "List directory entries with optional recursion";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Directory path to list" },
            "recursive": { "type": "boolean", "description": "List recursively (default false)" },
            "max_entries": { "type": "integer", "description": "Maximum entries to return (default 200)" }
        },
        "required": ["path"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "entries": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "path": { "type": "string" },
                        "is_dir": { "type": "boolean" },
                        "size_bytes": { "type": "integer" }
                    }
                }
            },
            "count": { "type": "integer" },
            "truncated": { "type": "boolean" }
        }
    })
});

impl FsListTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

const DEFAULT_MAX_ENTRIES: usize = 200;

#[async_trait]
impl Tool for FsListTool {
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
        let max_entries = args
            .get("max_entries")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_MAX_ENTRIES as i64) as usize;

        let path = PathBuf::from(path_str);

        self.validator
            .validate_path_for_read(&path)
            .map_err(KernelError::ToolFailed)?;

        let mut entries: Vec<Value> = Vec::new();
        let mut truncated = false;

        if recursive {
            let walker = walkdir::WalkDir::new(&path);
            for entry in walker.into_iter().filter_map(|e| e.ok()) {
                if entries.len() >= max_entries {
                    truncated = true;
                    break;
                }
                let entry_path = entry.path();
                if entry_path == path {
                    continue;
                }
                let meta = entry.metadata().ok();
                entries.push(json!({
                    "name": entry.file_name().to_string_lossy(),
                    "path": entry_path.to_string_lossy(),
                    "is_dir": meta.as_ref().is_some_and(|m| m.is_dir()),
                    "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0)
                }));
            }
        } else {
            let mut reader = tokio::fs::read_dir(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to read dir: {}", e)))?;

            while let Some(entry) = reader
                .next_entry()
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to read entry: {}", e)))?
            {
                if entries.len() >= max_entries {
                    truncated = true;
                    break;
                }
                let meta = entry.metadata().await.ok();
                entries.push(json!({
                    "name": entry.file_name().to_string_lossy(),
                    "path": entry.path().to_string_lossy(),
                    "is_dir": meta.as_ref().is_some_and(|m| m.is_dir()),
                    "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0)
                }));
            }
        }

        let count = entries.len();
        Ok(json!({
            "path": path_str,
            "entries": entries,
            "count": count,
            "truncated": truncated
        }))
    }
}
