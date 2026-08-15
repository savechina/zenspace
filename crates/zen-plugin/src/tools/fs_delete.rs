use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsDeleteTool {
    validator: SandboxValidator,
    workspace_root: Option<PathBuf>,
}

const NAME: &str = "fs.delete";
const DESCRIPTION: &str = "Delete a file or empty directory";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Path to delete" },
            "recursive": { "type": "boolean", "description": "Allow deleting non-empty directories (default false)" },
            "clear_contents": { "type": "boolean", "description": "Delete a directory's contents but keep the directory itself (default false)" }
        },
        "required": ["path"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "deleted": { "type": "boolean" },
            "was_dir": { "type": "boolean" }
        }
    })
});

impl FsDeleteTool {
    pub fn new(validator: SandboxValidator, workspace_root: Option<PathBuf>) -> Self {
        Self {
            validator,
            workspace_root,
        }
    }
}

fn is_protected_name(name: &str) -> bool {
    const PROTECTED: &[&str] = &[".git", ".zen", ".ssh", ".aws", ".gnupg"];
    PROTECTED.contains(&name) || name.ends_with(".env")
}

#[async_trait]
impl Tool for FsDeleteTool {
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
        let clear_contents = args
            .get("clear_contents")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = PathBuf::from(path_str);
        self.validator
            .validate_path_for_write(&path)
            .map_err(KernelError::ToolFailed)?;

        if let Some(root) = &self.workspace_root
            && path == *root
        {
            return Ok(json!({ "error": "cannot delete workspace root" }));
        }

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("Failed to stat {}: {}", path_str, e)))?;

        let was_dir = meta.is_dir();

        if clear_contents {
            if !was_dir {
                return Err(KernelError::InvalidArgument(
                    "clear_contents applies only to directories".into(),
                ));
            }
            let mut reader = tokio::fs::read_dir(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to read dir: {}", e)))?;
            while let Some(entry) = reader
                .next_entry()
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to read entry: {}", e)))?
            {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_protected_name(&name) {
                    continue;
                }
                let entry_path = entry.path();
                let entry_meta = tokio::fs::symlink_metadata(&entry_path)
                    .await
                    .map_err(|e| KernelError::ToolFailed(format!("Failed to stat entry: {}", e)))?;
                if entry_meta.is_dir() {
                    tokio::fs::remove_dir_all(&entry_path).await.map_err(|e| {
                        KernelError::ToolFailed(format!("Failed to remove dir: {}", e))
                    })?;
                } else {
                    tokio::fs::remove_file(&entry_path).await.map_err(|e| {
                        KernelError::ToolFailed(format!("Failed to delete file: {}", e))
                    })?;
                }
            }
            return Ok(json!({
                "path": path_str,
                "deleted": true,
                "was_dir": true
            }));
        }

        if was_dir && !recursive {
            let is_empty = tokio::fs::read_dir(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to read dir: {}", e)))?
                .next_entry()
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to read entry: {}", e)))?
                .is_none();

            if !is_empty {
                return Err(KernelError::InvalidArgument(format!(
                    "Directory {} is not empty (use recursive=true to force)",
                    path_str
                )));
            }
        }

        if was_dir && recursive {
            tokio::fs::remove_dir_all(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to remove dir: {}", e)))?;
        } else if was_dir {
            tokio::fs::remove_dir(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to remove dir: {}", e)))?;
        } else {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| KernelError::ToolFailed(format!("Failed to delete file: {}", e)))?;
        }

        Ok(json!({
            "path": path_str,
            "deleted": true,
            "was_dir": was_dir
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
    fn rejects_workspace_root() {
        let dir = tempdir().unwrap();
        let tool = FsDeleteTool::new(make_validator(dir.path()), Some(dir.path().to_path_buf()));
        let res = block_on(tool.invoke(json!({
            "path": dir.path().to_string_lossy(),
            "recursive": true
        })))
        .unwrap();
        assert_eq!(res["error"], "cannot delete workspace root");
        assert!(dir.path().exists());
    }

    #[test]
    fn clear_contents_empties_dir_but_keeps_it() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("a.txt"), "x").unwrap();
        std::fs::create_dir_all(target.join("sub")).unwrap();
        std::fs::write(target.join("sub/b.txt"), "x").unwrap();
        std::fs::create_dir_all(target.join(".git")).unwrap();
        std::fs::write(target.join(".git/config"), "x").unwrap();

        let tool = FsDeleteTool::new(make_validator(dir.path()), Some(dir.path().to_path_buf()));
        let res = block_on(tool.invoke(json!({
            "path": target.to_string_lossy(),
            "clear_contents": true
        })))
        .unwrap();
        assert_eq!(res["deleted"], true);
        assert!(target.exists());
        assert!(!target.join("a.txt").exists());
        assert!(!target.join("sub").exists());
        assert!(target.join(".git/config").exists());
    }

    #[test]
    fn clear_contents_rejects_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        let tool = FsDeleteTool::new(make_validator(dir.path()), Some(dir.path().to_path_buf()));
        let err = block_on(tool.invoke(json!({
            "path": file.to_string_lossy(),
            "clear_contents": true
        })))
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("clear_contents applies only to directories")
        );
    }
}
