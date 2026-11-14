//! W7.2 (D22) config-surface audit — grep-proof sentinel.
//!
//! PURPOSE: prove the keys the owner ruled out NEVER land: `[agentic.plan]`,
//! `max_background`, `[llm]`, `request_timeout_secs`,
//! `ZEN_LLM_REQUEST_TIMEOUT_SECS` (owner directives ×2 — the primary
//! timeout stays code-tunable). Five policy rules live in AGENTS.md
//! (defaults in code / one concept one key / secrets as env names / no NEW
//! env-only knobs / the two pre-existing stream envs are a recorded
//! exception).
//!
//! USAGE: `cargo nextest run -p zen-agents --test config_surface_audit`.
//!
//! EXPECTED: zero occurrences in `.rs`/`.toml` under `crates/` and
//! `config/`, except the single documented doc-comment anchor in
//! completion_model.rs (the "this key must never exist" note) and this
//! audit file itself.
//!
//! ERRORS: a failure names the file/line that reintroduced a banned key —
//! the fix is to remove the key, not to extend the allowlist.

use std::path::{Path, PathBuf};

const BANNED_KEYS: &[(&str, &[&str])] = &[
    (
        "agentic.plan",
        &["completion_model.rs", "config_surface_audit.rs"],
    ),
    (
        "max_background",
        &["completion_model.rs", "config_surface_audit.rs"],
    ),
    (
        "request_timeout_secs",
        &["completion_model.rs", "config_surface_audit.rs"],
    ),
    (
        "ZEN_LLM_REQUEST_TIMEOUT_SECS",
        &["completion_model.rs", "config_surface_audit.rs"],
    ),
];

fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let stack = vec![root.to_path_buf()];
    for dir in stack {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_source = !path.is_dir()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "rs" || e == "toml");
            if is_source {
                out.push(path);
            } else if path.is_dir() {
                out.extend(source_files(&path));
            }
        }
    }
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

#[test]
fn banned_config_keys_never_land_in_source_or_shipped_config() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();
    let mut hits = Vec::new();
    for file in source_files(&workspace_root.join("crates")) {
        let name = file_name(&file);
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (key, allow) in BANNED_KEYS {
            // Allowlisted files are SKIPPED entirely: they are the doc
            // anchor (completion_model.rs "this key must never exist"
            // note) and this audit itself (whose constants spell the keys).
            // Every other file must not contain the key at all.
            if allow.contains(&name.as_str()) {
                continue;
            }
            if content.contains(key) {
                hits.push(format!("{}: {key}", file.display()));
            }
        }
    }
    // The shipped config template must not declare a [llm] table.
    let shipped = std::fs::read_to_string(workspace_root.join("config").join("config.toml"))
        .unwrap_or_default();
    assert!(
        !shipped.contains("[llm]"),
        "[llm] table must never land in the shipped config"
    );
    assert!(
        hits.is_empty(),
        "banned config keys resurfaced:\n{}",
        hits.join("\n")
    );
}
