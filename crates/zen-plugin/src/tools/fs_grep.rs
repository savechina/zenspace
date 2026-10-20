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

/// Per-file read bound (mirror of tier-1's `TIER1_OUTPUT_CAP_BYTES`): one
/// giant file must not blow the memory of a single grep call — bytes beyond
/// the cap are invisible to the match scan.
const MAX_GREP_FILE_READ_BYTES: u64 = 1024 * 1024;

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
        let pattern_str = match args["pattern"].as_str() {
            Some(p) => p,
            None => return Ok(json!({ "error": "Missing or invalid 'pattern' field" })),
        };

        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return Ok(json!({ "error": "Missing or invalid 'path' field" })),
        };

        let include_glob = args.get("include").and_then(|v| v.as_str());
        let max_matches = args
            .get("max_matches")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_MAX_MATCHES as i64) as usize;
        let case_insensitive = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut re_builder = match Regex::new(pattern_str) {
            Ok(re) => re,
            Err(e) => return Ok(json!({ "error": format!("Invalid regex: {}", e) })),
        };
        if case_insensitive {
            re_builder = match Regex::new(&format!("(?i){}", pattern_str)) {
                Ok(re) => re,
                Err(e) => return Ok(json!({ "error": format!("Invalid regex: {}", e) })),
            };
        }
        let re = re_builder;

        let root = zen_core::sandbox::resolve_agent_path(path_str);
        self.validator
            .validate_path_for_read(&root)
            .map_err(KernelError::ToolFailed)?;

        // Hoisted from the per-file loop. A glob that fails to compile
        // excluded every file before (per-file `unwrap_or(false)`), so the
        // early empty result preserves that behavior byte-exactly.
        let include_pattern = match include_glob {
            Some(glob_pat) => match glob::Pattern::new(glob_pat) {
                Ok(p) => Some(p),
                Err(_) => {
                    return Ok(json!({
                        "pattern": pattern_str,
                        "matches": [],
                        "count": 0,
                        "truncated": false
                    }));
                }
            },
            None => None,
        };

        // The walk + per-file read scan is fully synchronous (walkdir and
        // std::fs reads) — under the shared daemon runtime it has to run on
        // the blocking pool, mirroring the fs_write vault-write seam.
        let validator = self.validator.clone();
        let (results, truncated) = tokio::task::spawn_blocking(move || {
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

                if let Some(pattern) = &include_pattern {
                    let file_name = file_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if !pattern.matches(&file_name) {
                        continue;
                    }
                }

                if validator.validate_path_for_read(&file_path).is_err() {
                    continue;
                }

                {
                    use std::io::Read;
                    let bytes = match std::fs::File::open(&file_path) {
                        Ok(f) => {
                            let mut buf = Vec::new();
                            let _ = f.take(MAX_GREP_FILE_READ_BYTES).read_to_end(&mut buf);
                            buf
                        }
                        Err(_) => continue,
                    };
                    // Strict UTF-8 as before: read_to_string failed the file
                    // silently and so does this decode.
                    let content = match String::from_utf8(bytes) {
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
            }
            (results, truncated)
        })
        .await
        .map_err(|e| KernelError::ToolFailed(format!("fs.grep blocking scan failed: {e}")))?;

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
    fn invalid_regex_returns_structured_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let tool = FsGrepTool::new(make_validator(dir.path()));
        let res = block_on(tool.invoke(json!({
            "pattern": "(",
            "path": dir.path().to_string_lossy()
        })))
        .unwrap();
        assert!(
            res["error"]
                .as_str()
                .is_some_and(|m| m.contains("Invalid regex")),
            "expected structured error, got: {res}"
        );
    }

    #[test]
    fn grep_caps_per_file_read_at_the_memory_bound() {
        // The per-file read cap: a needle inside the first MAX bytes is
        // found; the same needle beyond the cap is invisible. This pins
        // BOTH the bounded-memory contract and its match-scan consequence.
        let dir = tempdir().unwrap();
        let inside = "needle-inside";
        let beyond = "needle-beyond";
        let mut body = String::new();
        body.push_str(inside);
        body.push('\n');
        body.push_str(&"x".repeat(MAX_GREP_FILE_READ_BYTES as usize + 4096));
        body.push_str(beyond);
        std::fs::write(dir.path().join("big.md"), &body).unwrap();

        let tool = FsGrepTool::new(make_validator(dir.path()));
        let res = block_on(tool.invoke(json!({
            "pattern": "needle-\\w+",
            "path": dir.path().to_string_lossy()
        })))
        .unwrap();
        assert_eq!(res["count"], 1, "only the in-cap needle matches: {res}");
        assert_eq!(
            res["matches"][0]["content"], inside,
            "found the in-cap hit, not the beyond-cap one"
        );
    }
}
