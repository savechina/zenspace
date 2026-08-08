use std::path::PathBuf;
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
        let pattern = args["pattern"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'pattern' field".into())
        })?;

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_MAX_RESULTS as i64) as usize;

        let base = args.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from);

        let full_pattern = match &base {
            Some(b) => {
                self.validator
                    .validate_path_for_read(b)
                    .map_err(KernelError::ToolFailed)?;
                b.join(pattern).to_string_lossy().to_string()
            }
            None => pattern.to_string(),
        };

        let mut matches: Vec<String> = Vec::new();
        let mut truncated = false;

        for entry in glob::glob(&full_pattern)
            .map_err(|e| KernelError::InvalidArgument(format!("Invalid glob pattern: {}", e)))?
        {
            match entry {
                Ok(path) => {
                    if matches.len() >= max_results {
                        truncated = true;
                        break;
                    }
                    if self.validator.validate_path_for_read(&path).is_err() {
                        continue;
                    }
                    matches.push(path.to_string_lossy().to_string());
                }
                Err(_) => continue,
            }
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
