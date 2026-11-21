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
use zen_core::sandbox::{
    SandboxMode, SandboxValidator, resolve_agent_path, resolve_agent_read_path,
};
use zen_plugin::tools::fs_glob::FsGlobTool;
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

// FIX-5 pins (2026-11-21 search wave): the fs.read not-found error must
// carry the resolved path + a recovery hint (a bare hallucinated filename
// previously ENOENTed with no guidance, so the model retried blindly in
// production), and fs.glob must anchor a pattern-leading subtree at the
// zen root even without an explicit cwd (the daemon's cwd=/ would
// otherwise glob the filesystem root, and "vault/**" matched nothing).
#[tokio::test]
async fn fs_read_not_found_error_carries_resolved_path_and_hint() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let tool = FsReadTool::new(SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]));
    let out = tool
        .invoke(serde_json::json!({ "path": "no-such-file-anywhere.md" }))
        .await
        .unwrap();
    let text = out.to_string();
    assert!(
        text.contains("resolved"),
        "error must name the resolved path: {text}"
    );
    assert!(
        text.contains("fs.glob"),
        "error must hint the recovery tool: {text}"
    );
    assert!(text.contains("no-such-file-anywhere.md"));
}

#[tokio::test]
async fn fs_glob_anchors_pattern_leading_subtree_at_user_root() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let page = home.path().join(".zen/vault/wiki/buffett.md");
    std::fs::create_dir_all(page.parent().unwrap()).unwrap();
    std::fs::write(&page, "letters").unwrap();

    let tool = FsGlobTool::new(SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]));
    let out = tool
        .invoke(serde_json::json!({ "pattern": "vault/wiki/*.md" }))
        .await
        .unwrap();
    let matches = out["matches"].as_array().cloned().unwrap_or_default();
    let expected = lexical(&page).to_string_lossy().to_string();
    assert!(
        matches
            .iter()
            .any(|m| m.as_str() == Some(expected.as_str())),
        "expected {expected} in {out}"
    );
}

#[tokio::test]
async fn fs_glob_bare_pattern_anchors_when_cwd_is_filesystem_root() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let page = home.path().join(".zen/notes/buffett.md");
    std::fs::create_dir_all(page.parent().unwrap()).unwrap();
    std::fs::write(&page, "letters").unwrap();

    let prior = std::env::current_dir().unwrap();
    std::env::set_current_dir("/").unwrap();
    let tool = FsGlobTool::new(SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]));
    let result = tool
        .invoke(serde_json::json!({ "pattern": "notes/*.md" }))
        .await;
    std::env::set_current_dir(&prior).unwrap();
    let out = result.unwrap();

    let matches = out["matches"].as_array().cloned().unwrap_or_default();
    let expected = lexical(&page).to_string_lossy().to_string();
    assert!(
        matches
            .iter()
            .any(|m| m.as_str() == Some(expected.as_str())),
        "expected {expected} in {out}"
    );
}

// Vault-first pins (2026-11-21 fast-search wave): read-class tools probe the
// vault for relative names that resolve nowhere else — agents name knowledge
// files by their vault-relative spelling ("wiki/...", bare filenames), which
// the zen root does not carry, and the daemon (cwd=/) previously turned every
// such name into an ENOENT at the filesystem root.
#[tokio::test]
async fn read_path_bare_name_falls_back_to_vault() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let note = home.path().join(".zen/vault/chairman.md");
    std::fs::create_dir_all(note.parent().unwrap()).unwrap();
    std::fs::write(&note, "letters").unwrap();

    expect_resolved(
        resolve_agent_read_path("chairman.md"),
        &home.path().join(".zen/vault/chairman.md"),
    );
}

#[tokio::test]
async fn read_path_wiki_spelling_falls_back_to_vault() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let note = home.path().join(".zen/vault/wiki/notes.md");
    std::fs::create_dir_all(note.parent().unwrap()).unwrap();
    std::fs::write(&note, "body").unwrap();

    expect_resolved(
        resolve_agent_read_path("wiki/notes.md"),
        &home.path().join(".zen/vault/wiki/notes.md"),
    );
}

#[tokio::test]
async fn read_path_zen_root_hit_beats_vault_fallback() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let zen_root_hit = home.path().join(".zen/skills/x.md");
    std::fs::create_dir_all(zen_root_hit.parent().unwrap()).unwrap();
    std::fs::write(&zen_root_hit, "zen").unwrap();
    let vault_copy = home.path().join(".zen/vault/skills/x.md");
    std::fs::create_dir_all(vault_copy.parent().unwrap()).unwrap();
    std::fs::write(&vault_copy, "vault").unwrap();

    expect_resolved(resolve_agent_read_path("skills/x.md"), &zen_root_hit);
}

#[tokio::test]
async fn read_path_unknown_bare_name_stays_cwd_relative() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    assert_eq!(
        resolve_agent_read_path("no-such-name-anywhere.md"),
        PathBuf::from("no-such-name-anywhere.md")
    );
}

#[tokio::test]
async fn fs_read_bare_vault_name_resolves() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let note = home.path().join(".zen/vault/chairman.md");
    std::fs::create_dir_all(note.parent().unwrap()).unwrap();
    std::fs::write(&note, "letters").unwrap();

    let tool = FsReadTool::new(SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]));
    let out = tool
        .invoke(serde_json::json!({ "path": "chairman.md" }))
        .await
        .unwrap();
    assert_eq!(out["content"], "letters", "got {out}");
}

#[tokio::test]
async fn fs_glob_leading_name_anchors_in_vault() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let page = home.path().join(".zen/vault/wiki/buffett.md");
    std::fs::create_dir_all(page.parent().unwrap()).unwrap();
    std::fs::write(&page, "letters").unwrap();

    let tool = FsGlobTool::new(SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]));
    let out = tool
        .invoke(serde_json::json!({ "pattern": "wiki/*.md" }))
        .await
        .unwrap();
    let matches = out["matches"].as_array().cloned().unwrap_or_default();
    let expected = lexical(&page).to_string_lossy().to_string();
    assert!(
        matches
            .iter()
            .any(|m| m.as_str() == Some(expected.as_str())),
        "expected {expected} in {out}"
    );
}

#[tokio::test]
async fn fs_glob_excludes_iteration_snapshots_from_matches() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zen")).unwrap();
    let _guard = ZenHomeGuard::install(&home.path().join(".zen"));
    let live = home.path().join(".zen/vault/wiki/live.md");
    let stale = home.path().join(".zen/vault/iterations/wiki/live.md");
    for p in [&live, &stale] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "needle letters").unwrap();
    }

    let tool = FsGlobTool::new(SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]));
    let out = tool
        .invoke(serde_json::json!({ "pattern": "vault/**/*.md" }))
        .await
        .unwrap();
    let matches = out["matches"].as_array().cloned().unwrap_or_default();
    let live_str = lexical(&live).to_string_lossy().to_string();
    assert_eq!(matches.len(), 1, "stale snapshot must not match: {out}");
    assert!(
        matches[0].as_str() == Some(live_str.as_str()),
        "only the live page matches: {out}"
    );
}
