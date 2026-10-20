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

/// Shared entry filter for both the recursive walk and read_dir branches.
fn entry_visible(
    include_hidden: bool,
    pattern: Option<&glob::Pattern>,
    name: &std::ffi::OsStr,
) -> bool {
    let name_str = name.to_string_lossy();
    if !include_hidden && name_str.starts_with('.') {
        return false;
    }
    if let Some(pattern) = pattern
        && !pattern.matches(name_str.as_ref())
    {
        return false;
    }
    true
}

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
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return Ok(json!({ "error": "Missing or invalid 'path' field" })),
        };

        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_entries = args
            .get("max_entries")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_MAX_ENTRIES as i64) as usize;
        let depth = args.get("depth").and_then(|v| v.as_i64());
        let glob_pattern = match args.get("glob").and_then(|v| v.as_str()) {
            Some(s) => match glob::Pattern::new(s) {
                Ok(p) => Some(p),
                Err(e) => return Ok(json!({ "error": format!("Invalid glob: {}", e) })),
            },
            None => None,
        };
        let include_hidden = args
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = zen_core::sandbox::resolve_agent_path(path_str);

        self.validator
            .validate_path_for_read(&path)
            .map_err(KernelError::ToolFailed)?;

        let mut entries: Vec<Value> = Vec::new();
        let mut truncated = false;

        if recursive {
            // walkdir is a synchronous iterator: under the shared daemon
            // runtime a deep recursive walk has to run on the blocking pool
            // (fs_write vault-write seam precedent).
            let walk_root = path.clone();
            let walk_glob = glob_pattern.clone();
            let (walk_entries, walk_truncated) = tokio::task::spawn_blocking(move || {
                let mut entries: Vec<Value> = Vec::new();
                let mut truncated = false;
                let mut walker = walkdir::WalkDir::new(&walk_root);
                if let Some(d) = depth {
                    walker = walker.max_depth(d as usize);
                }
                for entry in walker.into_iter().filter_map(|e| e.ok()) {
                    if entries.len() >= max_entries {
                        truncated = true;
                        break;
                    }
                    let entry_path = entry.path();
                    if entry_path == walk_root {
                        continue;
                    }
                    if !entry_visible(include_hidden, walk_glob.as_ref(), entry.file_name()) {
                        continue;
                    }
                    // entry.file_type() is symlink_metadata-based (does not
                    // follow symlinks), matching the non-recursive path below.
                    let ft = entry.file_type();
                    let meta = entry.metadata().ok();
                    entries.push(json!({
                        "name": entry.file_name().to_string_lossy(),
                        "path": entry_path.to_string_lossy(),
                        "is_dir": ft.is_dir(),
                        "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0)
                    }));
                }
                (entries, truncated)
            })
            .await
            .map_err(|e| KernelError::ToolFailed(format!("fs.list blocking walk failed: {e}")))?;
            entries = walk_entries;
            truncated = walk_truncated;
        } else {
            let mut reader = match tokio::fs::read_dir(&path).await {
                Ok(r) => r,
                Err(e) => return Ok(json!({ "error": format!("Failed to read dir: {}", e) })),
            };

            while let Some(entry) = match reader.next_entry().await {
                Ok(entry) => entry,
                Err(e) => {
                    return Ok(json!({ "error": format!("Failed to read entry: {}", e) }));
                }
            } {
                if entries.len() >= max_entries {
                    truncated = true;
                    break;
                }
                if !entry_visible(
                    include_hidden,
                    glob_pattern.as_ref(),
                    entry.file_name().as_os_str(),
                ) {
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

    #[test]
    fn missing_dir_returns_structured_error() {
        let dir = tempdir().unwrap();
        let tool = FsListTool::new(make_validator(dir.path()));
        let res = invoke(
            &tool,
            json!({ "path": dir.path().join("does-not-exist").to_string_lossy() }),
        );
        assert!(
            res["error"]
                .as_str()
                .is_some_and(|m| m.contains("Failed to read dir")),
            "expected structured error, got: {res}"
        );
    }
}
