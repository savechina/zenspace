// ============================================================================
// 4D Test Suite: zen-core paths.rs
//
// IMPORTANT: These tests require `cargo nextest` for proper execution.
// Regular `cargo test` does not provide process isolation, causing environment
// variable leakage between tests. The CI uses nextest.
//
// Dimensions:
//   NORMAL     — detect() works, all path methods return expected paths
//   REVERSE    — No HOME / empty HOME produces correct errors
//   ADVERSARIAL — Empty domains, path traversal, non-existent ZEN_WORKSPACE
//   LOGIC TREE  — Workspace Some vs None branches, detect() success vs error
// ============================================================================

use std::path::PathBuf;
use tempfile::TempDir;
use zen_core::paths::*;

// ============================================================================
// Helpers
// ============================================================================

/// Create a temp dir with a `.zen` subdirectory, set ZEN_HOME to its parent,
/// and return the TempDir (keeps dir alive) plus the "home" path.
fn setup_zen_home() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("Failed to create temp dir");
    let home_path = tmp.path().join("home");
    std::fs::create_dir_all(&home_path).expect("Failed to create home dir");
    // SAFETY: nextest runs each test in its own process, so env var isolation is guaranteed
    unsafe {
        std::env::set_var("ZEN_HOME", &home_path);
    }
    (tmp, home_path)
}

/// Create a workspace structure with a `.zen` subdirectory inside a temp dir.
fn setup_workspace(base: &std::path::Path, name: &str) -> PathBuf {
    let ws = base.join(name);
    std::fs::create_dir_all(&ws).expect("Failed to create workspace dir");
    let dot_zen = ws.join(".zen");
    std::fs::create_dir_all(&dot_zen).expect("Failed to create .zen dir");
    ws
}

/// Unset both HOME and ZEN_HOME so that user_root() returns default (empty).
fn unset_home_env() {
    // SAFETY: nextest runs each test in its own process, so env var isolation is guaranteed
    unsafe {
        std::env::remove_var("ZEN_HOME");
    }
    unsafe {
        std::env::remove_var("HOME");
    }
}

// ============================================================================
// NORMAL PATH — Basic detect and path resolution
// ============================================================================

#[test]
fn detect_with_zen_home_succeeds() {
    let (_tmp, home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed with ZEN_HOME set");
    assert_eq!(
        paths.global_root(),
        &home_path,
        "global_root should match ZEN_HOME"
    );
    assert!(
        paths.workspace_root().is_none(),
        "No workspace should be found in temp dir"
    );
}

#[test]
fn detect_with_workspace_sets_workspace_root() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "my-project");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }

    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert_eq!(paths.global_root(), &home_path);
    assert_eq!(
        paths.workspace_root(),
        Some(&ws),
        "workspace_root should be set"
    );

    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn config_file_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "project-a");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }

    let paths = ZenPaths::detect().expect("detect() should succeed");
    let cfg = paths.config_file();
    assert!(
        cfg.starts_with(&home_path),
        "config_file should be under global_root (Path Spec v2): {cfg:?}"
    );
    assert!(
        cfg.ends_with("config.toml"),
        "config_file should end with config.toml: {cfg:?}"
    );

    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn config_file_uses_global_when_no_workspace() {
    let (_tmp, home_path) = setup_zen_home();

    let paths = ZenPaths::detect().expect("detect() should succeed");
    let cfg = paths.config_file();
    assert!(
        cfg.starts_with(&home_path),
        "config_file should be under global: {cfg:?}"
    );
    assert!(
        cfg.ends_with("config.toml"),
        "config_file should end with config.toml: {cfg:?}"
    );
}

#[test]
fn vault_is_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "project");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }

    let paths = ZenPaths::detect().expect("detect() should succeed");
    let kb = paths.vault();
    assert!(
        kb.starts_with(&home_path),
        "vault should be under global_root: {kb:?}"
    );
    assert!(
        kb.ends_with("vault"),
        "vault should end with 'vault': {kb:?}"
    );

    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn inbox_is_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "project");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }

    let paths = ZenPaths::detect().expect("detect() should succeed");
    let inbox = paths.inbox();
    assert!(
        inbox.starts_with(&home_path),
        "inbox should be under global_root: {inbox:?}"
    );
    assert!(inbox.ends_with("vault/inbox"), "inbox path: {inbox:?}");

    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn raw_is_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "project");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }

    let paths = ZenPaths::detect().expect("detect() should succeed");
    let raw = paths.raw();
    assert!(
        raw.starts_with(&home_path),
        "raw should be under global_root: {raw:?}"
    );
    assert!(raw.ends_with("vault/raw"), "raw path: {raw:?}");

    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn wiki_is_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "project");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }

    let paths = ZenPaths::detect().expect("detect() should succeed");
    let wiki = paths.wiki();
    assert!(
        wiki.starts_with(&home_path),
        "wiki should be under global_root: {wiki:?}"
    );
    assert!(wiki.ends_with("vault/wiki"), "wiki path: {wiki:?}");

    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn skills_is_under_user_data() {
    let (_tmp, _home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    let skills = paths.skills();
    assert!(skills.ends_with("skills"), "skills path: {skills:?}");
}

#[test]
fn cache_is_under_global_root() {
    let (_tmp, home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");

    let cache_path = paths.cache("embeddings");
    assert!(
        cache_path.starts_with(&home_path),
        "cache should be under global: {cache_path:?}"
    );
    assert!(
        cache_path.ends_with("cache/embeddings"),
        "cache path: {cache_path:?}"
    );
}

#[test]
fn db_is_under_global_root() {
    let (_tmp, home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    let db = paths.db();
    assert!(db.starts_with(&home_path));
    assert!(db.ends_with("data"), "db path: {db:?}");
}

#[test]
fn sessions_is_under_global_root() {
    let (_tmp, _home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.sessions().ends_with("sessions"));
}

#[test]
fn memory_is_under_global_root() {
    let (_tmp, _home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.memory().ends_with("memories"));
}

#[test]
fn identity_is_under_global_root() {
    let (_tmp, _home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.identity().ends_with("memories"));
}

#[test]
fn logs_is_under_global_root() {
    let (_tmp, _home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.logs().ends_with("logs"));
}

#[test]
fn plugins_is_under_global_root() {
    let (_tmp, _home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.plugins().ends_with("plugins"));
}

#[test]
fn finance_is_under_user_data() {
    let (_tmp, _home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.finance().ends_with("finance"));
}

#[test]
fn output_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "output-project");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }

    let paths = ZenPaths::detect().expect("detect() should succeed");
    let out = paths.output();
    assert!(
        out.starts_with(&home_path),
        "output should be under global_root (Path Spec v2): {out:?}"
    );
    assert!(out.ends_with("output"), "output path: {out:?}");

    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn output_under_global_when_no_workspace() {
    let (_tmp, home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    let out = paths.output();
    assert!(
        out.starts_with(&home_path),
        "output should be under global: {out:?}"
    );
    assert!(out.ends_with("output"), "output path: {out:?}");
}

// ============================================================================
// REVERSE PATH — Absent HOME, absent ZEN_WORKSPACE
// ============================================================================

#[test]
fn detect_without_home_env_does_not_panic() {
    unset_home_env();
    // macOS home::home_dir() may still return a path via getpwuid even without HOME env,
    // so we cannot assert HomeDirNotFound portably. Instead verify no crash.
    let _ = ZenPaths::detect();
}

#[test]
fn detect_with_bogus_zen_workspace_ignores_it() {
    let (_tmp, _home_path) = setup_zen_home();
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", "/nonexistent/deadbeef");
    }

    let paths = ZenPaths::detect().expect("detect() should succeed even with bogus ZEN_WORKSPACE");
    assert!(
        paths.workspace_root().is_none(),
        "Bogus workspace should be ignored"
    );

    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

// ============================================================================
// ADVERSARIAL PATH — Edge cases
// ============================================================================

#[test]
fn cache_with_empty_domain() {
    let (_tmp, home_path) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    let cache_path = paths.cache("");
    assert!(
        cache_path.starts_with(&home_path),
        "cache should be under global: {cache_path:?}"
    );
    assert!(
        cache_path.ends_with("cache/"),
        "empty domain cache path: {cache_path:?}"
    );
}

#[test]
fn cache_with_path_traversal() {
    let (_tmp, _) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");

    // This is a logic-level test: cache just joins paths, it doesn't sanitize
    let cache_path = paths.cache("../../../etc");
    assert!(
        cache_path.to_string_lossy().contains("cache/../../../etc"),
        "cache should append domain as-is: {cache_path:?}"
    );
}

#[test]
fn cache_with_unicode_domain() {
    let (_tmp, _) = setup_zen_home();
    let paths = ZenPaths::detect().expect("detect() should succeed");
    let cache_path = paths.cache("数据");
    assert!(
        cache_path.to_string_lossy().contains("数据"),
        "Unicode domain should be preserved: {cache_path:?}"
    );
}

// ============================================================================
// LOGIC TREE — Workspace-dependent methods exercise both branches
// ============================================================================

#[test]
fn config_file_always_global_regardless_of_workspace() {
    // Path Spec v2 (T18): config is global-only
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws-check");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    let cfg = paths.config_file();
    assert!(
        cfg.starts_with(&home_path),
        "config_file should always be under global_root: {cfg:?}"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn output_always_global_regardless_of_workspace() {
    // Path Spec v2 (T18): output is global-only
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws-check");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    let out = paths.output();
    assert!(
        out.starts_with(&home_path),
        "output should always be under global_root: {out:?}"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn vault_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(
        paths.vault().starts_with(&home_path),
        "vault should always be under global"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn inbox_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(
        paths.inbox().starts_with(&home_path),
        "inbox should always be under global"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn raw_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(
        paths.raw().starts_with(&home_path),
        "raw should always be under global"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn wiki_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(
        paths.wiki().starts_with(&home_path),
        "wiki should always be under global"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn skills_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(
        paths.skills().starts_with(&home_path),
        "skills should always be under global"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn finance_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(
        paths.finance().starts_with(&home_path),
        "finance should always be under global"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

/// These methods should ALWAYS use global_root regardless of workspace:
#[test]
fn cache_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    let ws = setup_workspace(&home_path, "ws");
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", &ws);
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(
        paths.cache("x").starts_with(&home_path),
        "cache should always be under global"
    );
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn db_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", setup_workspace(&home_path, "ws"));
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.db().starts_with(&home_path));
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn sessions_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", setup_workspace(&home_path, "ws"));
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.sessions().starts_with(&home_path));
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn memory_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", setup_workspace(&home_path, "ws"));
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.memory().starts_with(&home_path));
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn identity_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", setup_workspace(&home_path, "ws"));
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.identity().starts_with(&home_path));
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn logs_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", setup_workspace(&home_path, "ws"));
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.logs().starts_with(&home_path));
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[test]
fn plugins_always_global() {
    let (_tmp, home_path) = setup_zen_home();
    unsafe {
        std::env::set_var("ZEN_WORKSPACE", setup_workspace(&home_path, "ws"));
    }
    let paths = ZenPaths::detect().expect("detect() should succeed");
    assert!(paths.plugins().starts_with(&home_path));
    unsafe {
        std::env::remove_var("ZEN_WORKSPACE");
    }
}
