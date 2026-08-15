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
            "max_entries": { "type": "integer", "description": "Maximum entries to return (default 200)" },
            "depth": { "type": "integer", "description": "Max recursion depth (default unlimited). depth=1 = immediate children only" },
            "glob": { "type": "string", "description": "Filter entries by filename glob (e.g. \"*.rs\")" },
            "include_hidden": { "type": "boolean", "description": "Include hidden (dot-prefixed) entries (default false)" }
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
        let depth = args.get("depth").and_then(|v| v.as_i64());
        let glob_pattern = args
            .get("glob")
            .and_then(|v| v.as_str())
            .map(|s| {
                glob::Pattern::new(s)
                    .map_err(|e| KernelError::InvalidArgument(format!("Invalid glob: {}", e)))
            })
            .transpose()?;
        let include_hidden = args
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = PathBuf::from(path_str);

        self.validator
            .validate_path_for_read(&path)
            .map_err(KernelError::ToolFailed)?;

        let should_include = |name: &std::ffi::OsStr| -> bool {
            let name_str = name.to_string_lossy();
            if !include_hidden && name_str.starts_with('.') {
                return false;
            }
            if let Some(pattern) = &glob_pattern
                && !pattern.matches(name_str.as_ref())
            {
                return false;
            }
            true
        };

        let mut entries: Vec<Value> = Vec::new();
        let mut truncated = false;

        if recursive {
            let mut walker = walkdir::WalkDir::new(&path);
            if let Some(d) = depth {
                walker = walker.max_depth(d as usize);
            }
            for entry in walker.into_iter().filter_map(|e| e.ok()) {
                if entries.len() >= max_entries {
                    truncated = true;
                    break;
                }
                let entry_path = entry.path();
                if entry_path == path {
                    continue;
                }
                if !should_include(entry.file_name()) {
                    continue;
                }
                // entry.file_type() is symlink_metadata-based (does not follow
                // symlinks), matching the non-recursive path below.
                let ft = entry.file_type();
                let meta = entry.metadata().ok();
                entries.push(json!({
                    "name": entry.file_name().to_string_lossy(),
                    "path": entry_path.to_string_lossy(),
                    "is_dir": ft.is_dir(),
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
                if !should_include(entry.file_name().as_os_str()) {
                    continue;
                }
                // symlink_metadata (not metadata) so symlink-to-dir reports the
                // same is_dir as the walkdir path.
                let meta = tokio::fs::symlink_metadata(entry.path()).await.ok();
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

    fn invoke(tool: &FsListTool, args: Value) -> Value {
        block_on(tool.invoke(args)).unwrap()
    }

    #[test]
    fn depth_limits_recursion() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        std::fs::write(dir.path().join("a/root.txt"), "x").unwrap();
        std::fs::write(dir.path().join("a/b/mid.txt"), "x").unwrap();
        std::fs::write(dir.path().join("a/b/c/deep.txt"), "x").unwrap();

        let tool = FsListTool::new(make_validator(dir.path()));
        let res = invoke(
            &tool,
            json!({
                "path": dir.path().to_string_lossy(),
                "recursive": true,
                "depth": 1
            }),
        );
        let entries = res["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "a");
    }

    #[test]
    fn glob_filters_entries() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x").unwrap();
        std::fs::write(dir.path().join("b.txt"), "x").unwrap();
        std::fs::write(dir.path().join("c.rs"), "x").unwrap();

        let tool = FsListTool::new(make_validator(dir.path()));
        let res = invoke(
            &tool,
            json!({
                "path": dir.path().to_string_lossy(),
                "glob": "*.rs"
            }),
        );
        let entries = res["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        for e in entries {
            assert!(e["name"].as_str().unwrap().ends_with(".rs"));
        }
    }

    #[test]
    fn hidden_excluded_by_default() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden"), "x").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "x").unwrap();

        let tool = FsListTool::new(make_validator(dir.path()));
        let res = invoke(&tool, json!({ "path": dir.path().to_string_lossy() }));
        let entries = res["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "visible.txt");

        let res2 = invoke(
            &tool,
            json!({
                "path": dir.path().to_string_lossy(),
                "include_hidden": true
            }),
        );
        assert_eq!(res2["entries"].as_array().unwrap().len(), 2);
    }
}
