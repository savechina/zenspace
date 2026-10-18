// PURPOSE: Pin resolve_agent_path anchoring — daemon/hosted turns (cwd=/)
//          must address the knowledge base by its documented relative
//          spelling (vault/...), instead of ENOENTing on /vault like the
//          previous bare PathBuf::from did (production audit 2026-09-03/04).
// USAGE: cargo nextest run -p zen-plugin fs_relative_anchor
// EXPECTED: all pass; ZEN_HOME points at a tempdir, never the real ~/.zen.
// ERRORS: none — env is saved and restored under a lock.

use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

use rig_compose::tool::Tool;
use zen_core::sandbox::{SandboxMode, SandboxValidator, resolve_agent_path};
use zen_plugin::tools::fs_read::FsReadTool;
use zen_plugin::tools::fs_write::FsWriteTool;

static ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct ZenHomeGuard(Option<String>);

impl ZenHomeGuard {
    fn install(dir: &Path) -> Self {
        let prior = std::env::var("ZEN_HOME").ok();
        // SAFETY: nextest runs each test in its own process; the mutex also
        // serializes any same-process (cargo test) runs of this file.
        unsafe { std::env::set_var("ZEN_HOME", dir) };
        Self(prior)
    }
}

impl Drop for ZenHomeGuard {
    fn drop(&mut self) {
        // SAFETY: same isolation contract as install().
        match self.0.take() {
            Some(v) => unsafe { std::env::set_var("ZEN_HOME", v) },
            None => unsafe { std::env::remove_var("ZEN_HOME") },
        }
    }
}

fn expect_resolved(actual: PathBuf, expected: &Path) {
    assert_eq!(
        lexical(&actual),
        lexical(expected),
        "resolved={actual:?} expected={expected:?}"
    );
}

fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[tokio::test]
async fn absolute_paths_pass_through() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    // Mirror the production layout: the global root path itself carries the
    // `.zen` component that `is_zen_path` gates on (~/.zen in production).
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let abs = home.path().join(".zen").join("vault/wiki/x.md");
    assert_eq!(resolve_agent_path(abs.to_str().unwrap()), abs);
}

#[tokio::test]
async fn cwd_hit_wins_over_global_root() {
    // Cargo.toml exists in the package root (the test process cwd).
    let resolved = resolve_agent_path("Cargo.toml");
    assert_eq!(resolved, PathBuf::from("Cargo.toml"));
}

#[tokio::test]
async fn existing_global_root_path_anchors() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    // Mirror the production layout: the global root path itself carries the
    // `.zen` component that `is_zen_path` gates on (~/.zen in production).
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let wiki = home.path().join(".zen").join("vault/wiki/go.md");
    std::fs::create_dir_all(wiki.parent().unwrap()).unwrap();
    std::fs::write(&wiki, "package main").unwrap();

    expect_resolved(resolve_agent_path("vault/wiki/go.md"), &wiki);
}

#[tokio::test]
async fn new_file_under_vault_anchors_without_existence() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    // Mirror the production layout: the global root path itself carries the
    // `.zen` component that `is_zen_path` gates on (~/.zen in production).
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    expect_resolved(
        resolve_agent_path("vault/resources/buffett-letters.md"),
        &home
            .path()
            .join(".zen")
            .join("vault/resources/buffett-letters.md"),
    );
}

#[tokio::test]
async fn unknown_relative_stays_cwd_relative() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    // Mirror the production layout: the global root path itself carries the
    // `.zen` component that `is_zen_path` gates on (~/.zen in production).
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    assert_eq!(
        resolve_agent_path("some/other.txt"),
        PathBuf::from("some/other.txt")
    );
}

#[tokio::test]
async fn fs_read_resolves_vault_relative_path() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    // Mirror the production layout: the global root path itself carries the
    // `.zen` component that `is_zen_path` gates on (~/.zen in production).
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let note = home.path().join(".zen").join("vault/wiki/go.md");
    std::fs::create_dir_all(note.parent().unwrap()).unwrap();
    std::fs::write(&note, "package main").unwrap();

    let tool = FsReadTool::new(SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]));
    let out = tool
        .invoke(serde_json::json!({ "path": "vault/wiki/go.md" }))
        .await
        .unwrap();
    assert_eq!(out["content"], "package main", "got {out}");
}

#[tokio::test]
async fn fs_write_anchors_new_note_into_global_vault() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    // Mirror the production layout: the global root path itself carries the
    // `.zen` component that `is_zen_path` gates on (~/.zen in production).
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));

    let tool = FsWriteTool::new(SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]));
    let out = tool
        .invoke(serde_json::json!({
            "path": "vault/resources/buffett-letters.md",
            "content": "# Buffett Letters\n"
        }))
        .await
        .unwrap();
    assert!(
        out.get("error").is_none(),
        "write must be allowed under the zen global root, got {out}"
    );
    let written = home
        .path()
        .join(".zen")
        .join("vault/resources/buffett-letters.md");
    assert_eq!(
        std::fs::read_to_string(&written).unwrap(),
        "# Buffett Letters\n"
    );
}
