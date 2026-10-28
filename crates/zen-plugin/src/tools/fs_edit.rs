use std::io::Write;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;
use zen_core::tempfile_lifecycle::TempfileDropGuard;

#[derive(Clone)]
pub struct FsEditTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.edit";
const DESCRIPTION: &str =
    "Edit a file atomically (with backup): apply a unified diff, or replace old_text with new_text";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "File to edit" },
            "diff": { "type": "string", "description": "Unified diff to apply (mutually exclusive with old_text/new_text)" },
            "old_text": { "type": "string", "description": "Exact text to find (must be unique in the file)" },
            "new_text": { "type": "string", "description": "Replacement text" },
            "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" }
        },
        "oneOf": [
            { "required": ["path", "diff"] },
            { "required": ["path", "old_text", "new_text"] }
        ]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "replacements": { "type": "integer" },
            "backup_path": { "type": "string" }
        }
    })
});

impl FsEditTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

#[async_trait]
impl Tool for FsEditTool {
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

        let path = zen_core::sandbox::resolve_agent_path(path_str);
        self.validator
            .validate_path_for_write(&path)
            .map_err(KernelError::ToolFailed)?;

        let original = match tokio::fs::read_to_string(&path).await {
            Ok(o) => o,
            Err(e) => return Ok(json!({ "error": format!("Failed to read {}: {}", path_str, e) })),
        };

        let (modified, replacements) = match args.get("diff") {
            Some(diff) => {
                let diff_str = match diff.as_str() {
                    Some(s) => s,
                    None => return Ok(json!({ "error": "'diff' must be a string" })),
                };
                match apply_diff(&original, diff_str) {
                    Ok(applied) => applied,
                    Err(e) => {
                        return Ok(json!({
                            "error": format!("Failed to apply diff to {}: {}", path_str, e)
                        }));
                    }
                }
            }
            None => {
                let old_text = match args["old_text"].as_str() {
                    Some(t) => t,
                    None => {
                        return Ok(json!({ "error": "Missing or invalid 'old_text' field" }));
                    }
                };
                let new_text = match args["new_text"].as_str() {
                    Some(t) => t,
                    None => {
                        return Ok(json!({ "error": "Missing or invalid 'new_text' field" }));
                    }
                };
                let replace_all = args
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let count = original.matches(old_text).count();
                if count == 0 {
                    return Ok(json!({ "error": format!("old_text not found in {}", path_str) }));
                }
                if count > 1 && !replace_all {
                    return Ok(json!({
                        "error": format!(
                            "old_text appears {} times in {} (use replace_all=true)",
                            count, path_str
                        )
                    }));
                }

                let modified = if replace_all {
                    original.replace(old_text, new_text)
                } else {
                    original.replacen(old_text, new_text, 1)
                };
                let replacements = if replace_all { count } else { 1 };
                (modified, replacements)
            }
        };

        if std::str::from_utf8(modified.as_bytes()).is_err() {
            return Ok(json!({ "error": "Result is not valid UTF-8" }));
        }

        let backup_path = path.with_extension(format!(
            "{}.bak",
            path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
        ));
        // FR-040: guard the backup so a failure later in the sequence (temp
        // write, rename) removes the stale .bak instead of leaving it behind.
        let mut backup_guard = TempfileDropGuard::new(&backup_path);
        if let Err(e) = tokio::fs::copy(&path, &backup_path).await {
            return Ok(json!({ "error": format!("Failed to create backup: {}", e) }));
        }

        let temp_path = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|e| e.to_str()).unwrap_or("edit")
        ));

        // FR-040: guard the temp file so a failed write or rename removes the
        // stale .tmp instead of leaving it behind.
        let mut temp_guard = TempfileDropGuard::new(&temp_path);

        // Temp-write + fsync + atomic rename off the async worker (the
        // fsync can block for milliseconds on real disks; mirror of the
        // fs_write tunnel — same FR-021/040 atomic-replace contract, error
        // strings byte-identical).
        let write_result = tokio::task::spawn_blocking({
            let temp_path = temp_path.clone();
            let path = path.clone();
            move || -> Result<(), String> {
                let mut temp_file = match std::fs::File::create(&temp_path) {
                    Ok(f) => f,
                    Err(e) => {
                        return Err(format!("Failed to create temp file: {}", e));
                    }
                };
                if let Err(e) = temp_file.write_all(modified.as_bytes()) {
                    return Err(format!("Failed to write temp file: {}", e));
                }
                if let Err(e) = temp_file.sync_all() {
                    return Err(format!("Failed to sync temp file: {}", e));
                }
                drop(temp_file);
                if let Err(e) = std::fs::rename(&temp_path, &path) {
                    return Err(format!("Failed to atomically replace file: {}", e));
                }
                Ok(())
            }
        })
        .await
        .unwrap_or_else(|e| Err(format!("edit worker panicked: {e}")));

        if let Err(msg) = write_result {
            return Ok(json!({ "error": msg }));
        }

        // Edit fully succeeded: the temp file has been renamed into place and
        // the .bak backup is intentionally retained (returned to the caller).
        // Disarm both guards so Drop leaves them alone.
        temp_guard.disarm();
        backup_guard.disarm();

        Ok(json!({
            "path": path_str,
            "replacements": replacements,
            "backup_path": backup_path.to_string_lossy()
        }))
    }
}

fn apply_diff(original: &str, diff: &str) -> Result<(String, usize), String> {
    let patch = diffy::Patch::from_str(diff).map_err(|e| format!("invalid unified diff: {}", e))?;
    let applied =
        diffy::apply(original, &patch).map_err(|e| format!("patch application failed: {}", e))?;
    Ok((applied, patch.hunks().len()))
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
    fn apply_diff_replaces_line() {
        let (result, hunks) = apply_diff(
            "one\ntwo\nthree\n",
            "@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n",
        )
        .unwrap();
        assert_eq!(result, "one\nTWO\nthree\n");
        assert_eq!(hunks, 1);
    }

    #[test]
    fn apply_diff_parses_without_headers() {
        let diff = "@@ -1 +1 @@\n-old\n+new\n";
        let (result, _) = apply_diff("old\n", diff).unwrap();
        assert_eq!(result, "new\n");
    }

    #[test]
    fn apply_diff_rejects_invalid() {
        assert!(apply_diff("x\n", "@@ -1 +1 @@\n-b\n+c\n").is_err());
    }

    #[test]
    fn apply_diff_garbage_is_noop() {
        let (result, hunks) = apply_diff("x\n", "not a diff at all").unwrap();
        assert_eq!(result, "x\n");
        assert_eq!(hunks, 0);
    }

    #[test]
    fn edit_with_diff_mode_via_tool() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("note.md");
        std::fs::write(&file, "# Title\n\nbody\n").unwrap();
        let tool = FsEditTool::new(make_validator(dir.path()));
        let diff = "@@ -1,3 +1,3 @@\n # Title\n \n-body\n+BODY\n";
        let args = json!({
            "path": file.to_string_lossy(),
            "diff": diff,
        });
        let result = block_on(tool.invoke(args)).unwrap();
        assert_eq!(result["replacements"], 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "# Title\n\nBODY\n");
        let backup = std::fs::read_to_string(result["backup_path"].as_str().unwrap()).unwrap();
        assert_eq!(backup, "# Title\n\nbody\n");
    }

    #[test]
    fn edit_with_old_text_mode_via_tool() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello world\n").unwrap();
        let tool = FsEditTool::new(make_validator(dir.path()));
        let args = json!({
            "path": file.to_string_lossy(),
            "old_text": "world",
            "new_text": "rust",
        });
        let result = block_on(tool.invoke(args)).unwrap();
        assert_eq!(result["replacements"], 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello rust\n");
    }

    #[test]
    fn edit_rejects_outside_sandbox() {
        let dir = tempdir().unwrap();
        let outside = std::env::temp_dir().join("zen-fs-edit-outside-test.txt");
        std::fs::write(&outside, "x\n").unwrap();
        let tool = FsEditTool::new(make_validator(dir.path()));
        let args = json!({
            "path": outside.to_string_lossy(),
            "old_text": "x",
            "new_text": "y",
        });
        assert!(block_on(tool.invoke(args)).is_err());
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn failed_edit_leaves_no_temp_artifacts() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("note.md");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let tool = FsEditTool::new(make_validator(dir.path()));
        // Patch parses but does not apply → structured error after reading the file.
        let args = json!({
            "path": file.to_string_lossy(),
            "diff": "@@ -1 +1 @@\n-b\n+c\n",
        });
        let res = block_on(tool.invoke(args)).unwrap();
        assert!(
            res["error"]
                .as_str()
                .is_some_and(|m| m.contains("Failed to apply diff")),
            "expected structured diff error, got: {res}"
        );
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp") || n.ends_with(".bak"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "failed edit must leave no .tmp/.bak: {leftovers:?}"
        );
    }

    #[test]
    fn failed_edit_cleans_up_backup_when_temp_write_fails() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("note.md");
        std::fs::write(&file, "hello world\n").unwrap();
        // Pre-create a directory at the temp path so File::create fails after
        // the backup has already been written — exercising the backup guard's
        // Drop cleanup.
        let temp_dir = dir.path().join("note.md.tmp");
        std::fs::create_dir(&temp_dir).unwrap();
        let tool = FsEditTool::new(make_validator(dir.path()));
        let args = json!({
            "path": file.to_string_lossy(),
            "old_text": "world",
            "new_text": "rust",
        });
        let res = block_on(tool.invoke(args)).unwrap();
        assert!(
            res["error"]
                .as_str()
                .is_some_and(|m| m.contains("Failed to create temp file")),
            "expected structured temp-file error, got: {res}"
        );
        assert!(
            !dir.path().join("note.md.bak").exists(),
            "stale .bak must be removed by the backup guard"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world\n");
    }
}
