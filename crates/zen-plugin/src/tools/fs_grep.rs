use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsGrepTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.grep";
const DESCRIPTION: &str = "Search file contents using regex patterns";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "description": "Regex pattern to search for" },
            "path": { "type": "string", "description": "File or directory to search in" },
            "include": { "type": "string", "description": "File name glob filter (e.g. \"*.rs\")" },
            "max_matches": { "type": "integer", "description": "Maximum matches to return (default 50)" },
            "case_insensitive": { "type": "boolean", "description": "Case-insensitive matching (default false)" }
        },
        "required": ["pattern", "path"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "pattern": { "type": "string" },
            "matches": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string" },
                        "line": { "type": "integer" },
                        "content": { "type": "string" }
                    }
                }
            },
            "count": { "type": "integer" },
            "truncated": { "type": "boolean" }
        }
    })
});

impl FsGrepTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

const DEFAULT_MAX_MATCHES: usize = 50;

#[derive(serde::Serialize)]
struct GrepMatch {
    file: String,
    line: usize,
    content: String,
}

#[async_trait]
impl Tool for FsGrepTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let pattern_str = args["pattern"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'pattern' field".into())
        })?;

        let path_str = args["path"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'path' field".into())
        })?;

        let include_glob = args.get("include").and_then(|v| v.as_str());
        let max_matches = args
            .get("max_matches")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_MAX_MATCHES as i64) as usize;
        let case_insensitive = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut re_builder = Regex::new(pattern_str)
            .map_err(|e| KernelError::InvalidArgument(format!("Invalid regex: {}", e)))?;
        if case_insensitive {
            re_builder = Regex::new(&format!("(?i){}", pattern_str))
                .map_err(|e| KernelError::InvalidArgument(format!("Invalid regex: {}", e)))?;
        }
        let re = re_builder;

        let root = PathBuf::from(path_str);
        self.validator
            .validate_path_for_read(&root)
            .map_err(KernelError::ToolFailed)?;

        let mut results: Vec<GrepMatch> = Vec::new();
        let mut truncated = false;

        let files: Vec<PathBuf> = if root.is_file() {
            vec![root.clone()]
        } else {
            walkdir::WalkDir::new(&root)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .map(|e| e.into_path())
                .collect()
        };

        for file_path in files {
            if truncated {
                break;
            }

            if let Some(glob_pat) = include_glob {
                let file_name = file_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if !glob::Pattern::new(glob_pat)
                    .map(|p| p.matches(&file_name))
                    .unwrap_or(false)
                {
                    continue;
                }
            }

            if self.validator.validate_path_for_read(&file_path).is_err() {
                continue;
            }

            let content = match std::fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            for (line_num, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    if results.len() >= max_matches {
                        truncated = true;
                        break;
                    }
                    results.push(GrepMatch {
                        file: file_path.to_string_lossy().to_string(),
                        line: line_num + 1,
                        content: line.to_string(),
                    });
                }
            }
        }

        let count = results.len();
        let matches_json: Vec<Value> = results
            .iter()
            .map(|m| json!({ "file": m.file, "line": m.line, "content": m.content }))
            .collect();

        Ok(json!({
            "pattern": pattern_str,
            "matches": matches_json,
            "count": count,
            "truncated": truncated
        }))
    }
}
