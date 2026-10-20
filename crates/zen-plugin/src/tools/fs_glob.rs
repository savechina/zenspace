use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsGlobTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.glob";
const DESCRIPTION: &str = "Find files matching a glob pattern within the workspace";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "description": "Glob pattern (e.g. \"**/*.rs\", \"src/*.ts\")" },
            "cwd": { "type": "string", "description": "Base directory for relative patterns (default workspace root)" },
            "max_results": { "type": "integer", "description": "Maximum matches to return (default 100)" }
        },
        "required": ["pattern"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "pattern": { "type": "string" },
            "matches": { "type": "array", "items": { "type": "string" } },
            "count": { "type": "integer" },
            "truncated": { "type": "boolean" }
        }
    })
});

impl FsGlobTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

const DEFAULT_MAX_RESULTS: usize = 100;

#[async_trait]
impl Tool for FsGlobTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let pattern = match args["pattern"].as_str() {
            Some(p) => p,
            None => return Ok(json!({ "error": "Missing or invalid 'pattern' field" })),
        };

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_MAX_RESULTS as i64) as usize;

        let base = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(zen_core::sandbox::resolve_agent_path);

        let full_pattern = match &base {
            Some(b) => {
                self.validator
                    .validate_path_for_read(b)
                    .map_err(KernelError::ToolFailed)?;
                b.join(pattern).to_string_lossy().to_string()
            }
            None => pattern.to_string(),
        };

        // The glob iterator walks the filesystem synchronously — under the
        // shared daemon runtime it has to run on the blocking pool
        // (fs_write vault-write seam precedent).
        let validator = self.validator.clone();
        let (matches, truncated, invalid) = tokio::task::spawn_blocking(move || {
            let mut matches: Vec<String> = Vec::new();
            let mut truncated = false;

            let glob_iter = match glob::glob(&full_pattern) {
                Ok(it) => it,
                Err(e) => {
                    // Surfaced as the legacy structured error, not a
                    // KernelError.
                    return (
                        matches,
                        truncated,
                        Some(format!("Invalid glob pattern: {}", e)),
                    );
                }
            };
            for entry in glob_iter {
                match entry {
                    Ok(path) => {
                        if matches.len() >= max_results {
                            truncated = true;
                            break;
                        }
                        if validator.validate_path_for_read(&path).is_err() {
                            continue;
                        }
                        matches.push(path.to_string_lossy().to_string());
                    }
                    Err(_) => continue,
                }
            }
            (matches, truncated, None)
        })
        .await
        .map_err(|e| KernelError::ToolFailed(format!("fs.glob blocking scan failed: {e}")))?;
        if let Some(message) = invalid {
            return Ok(json!({ "error": message }));
        }

        let count = matches.len();
        Ok(json!({
            "pattern": pattern,
            "matches": matches,
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

    #[test]
    fn invalid_glob_returns_structured_error() {
        let dir = tempdir().unwrap();
        let tool = FsGlobTool::new(make_validator(dir.path()));
        let res = block_on(tool.invoke(json!({ "pattern": "[" }))).unwrap();
        assert!(
            res["error"]
                .as_str()
                .is_some_and(|m| m.contains("Invalid glob pattern")),
            "expected structured error, got: {res}"
        );
    }
}
