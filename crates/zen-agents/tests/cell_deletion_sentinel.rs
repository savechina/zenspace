//! W12.23 (TD6): shared-cell deletion sentinel — grep gate.
//!
//! PURPOSE: the D30 demoted shared cells (wiring sensitivity / orchestrator
//! `delegate_sensitivity` / delegate knowledge & history cells) must not
//! gain NEW readers that bypass the TurnContext-first accessors. All
//! consumers are migrated; the cells are demoted, not deleted (documented
//! non-turn fallback). This sentinel fails when a file OUTSIDE the
//! allowlisted fallback/writer sites reads the demoted cells directly —
//! new code must use `zen_core::turn_context::current_*` instead.
//!
//! USAGE: runs as part of `bin/test`; no environment needed (pure source
//! scan). EXPECTED: PASS while the allowlist below matches reality.
//! ERRORS: a new violator file ⇒ extend the migration, not the allowlist.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The only sites permitted to TOUCH the demoted shared cells: the
/// orchestrator's cell maintenance (`begin_turn_context` fallback write +
/// install seam), the wiring setter implementation itself, and the
/// delegate-side TurnContext-first fallbacks (`turn_sensitivity` /
/// `turn_knowledge` / `turn_history`).
const ALLOWED: &[&str] = &[
    "src/orchestrator.rs",
    "src/wiring.rs",
    "src/delegate_task.rs",
];

/// Direct-read markers of the demoted cells. `current_sensitivity()` and
/// friends are the SANCTIONED TurnContext-first accessors and never match.
const MARKERS: &[&str] = &[
    "delegate_sensitivity",
    "shared_sensitivity",
    "set_sensitivity(",
    "get_sensitivity(",
];

#[test]
fn no_new_readers_of_the_demoted_shared_cells() {
    let allowed: BTreeSet<&str> = ALLOWED.iter().copied().collect();
    let mut violators: Vec<String> = Vec::new();
    let mut src = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    src.push("src");
    let mut stack = vec![src];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    for path in files {
        let rel = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if allowed.contains(rel.as_str()) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        for marker in MARKERS {
            if body.contains(marker) {
                violators.push(format!("{rel}: touches `{marker}`"));
            }
        }
    }
    assert!(
        violators.is_empty(),
        "new demoted-cell consumers appeared — use \
         zen_core::turn_context::current_* (TurnContext-first) instead:\n{}",
        violators.join("\n")
    );
}
