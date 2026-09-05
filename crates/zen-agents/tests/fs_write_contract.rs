//! T104 (T064 contract): `fs.write` containment — vault-relative writes
//! land in the workspace, escapes are denied with a hint, and metadata
//! paths stay blocked. Ask-Deny surfacing (`tool_completed{error}` +
//! audit) is wiring-level, covered by gateway approval tests; this file
//! pins the tool-level gate.

use rig_compose::tool::Tool;
use tempfile::tempdir;
use zen_core::sandbox::{SandboxMode, SandboxValidator};
use zen_plugin::tools::fs_write::FsWriteTool;

fn tool_at(root: &std::path::Path) -> FsWriteTool {
    FsWriteTool::new(SandboxValidator::new(
        SandboxMode::WorkspaceWrite,
        vec![root.to_path_buf()],
    ))
}

fn write_args(path: &str) -> serde_json::Value {
    serde_json::json!({"path": path, "content": "hello contract"})
}

#[tokio::test]
async fn workspace_relative_write_lands_in_workspace() {
    let dir = tempdir().unwrap();
    let tool = tool_at(dir.path());
    let target = dir.path().join("vault/resources/x.md");
    let out = tool
        .invoke(write_args(target.to_str().unwrap()))
        .await
        .expect("workspace write must succeed");
    assert_eq!(out["bytes_written"], 14);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello contract");
}

#[tokio::test]
async fn absolute_escape_denied_with_hint() {
    let dir = tempdir().unwrap();
    let tool = tool_at(dir.path());
    for escaped in ["/tmp/outside.md", "./test.md"] {
        let err = tool
            .invoke(write_args(escaped))
            .await
            .expect_err("escape must be denied");
        let msg = err.to_string();
        assert!(
            msg.contains("workspace") || msg.contains("denied") || msg.contains("blocked"),
            "denial must hint at containment, got: {msg}"
        );
    }
    assert!(!dir.path().join("test.md").exists());
}

#[tokio::test]
async fn env_and_git_paths_blocked() {
    let dir = tempdir().unwrap();
    let tool = tool_at(dir.path());
    let env_target = dir.path().join(".env");
    assert!(
        tool.invoke(write_args(env_target.to_str().unwrap()))
            .await
            .is_err(),
        ".env writes must be blocked"
    );
    let git_target = dir.path().join(".git/config");
    assert!(
        tool.invoke(write_args(git_target.to_str().unwrap()))
            .await
            .is_err(),
        ".git writes must be blocked"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_escape_blocked() {
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    symlink(outside.path(), dir.path().join("link")).unwrap();
    let tool = tool_at(dir.path());
    let target = dir.path().join("link/evil.md");
    assert!(
        tool.invoke(write_args(target.to_str().unwrap()))
            .await
            .is_err(),
        "symlink escape must be blocked"
    );
    assert!(!outside.path().join("evil.md").exists());
}
