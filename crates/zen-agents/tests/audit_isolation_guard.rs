//! T111/T118: audit test isolation guard.
//!
//! PURPOSE: Prove that running the zen-agents test suite never writes
//! audit lines into the developer's REAL `~/.zen/logs/audit.jsonl`.
//!
//! USAGE: `cargo test -p zen-agents --test audit_isolation_guard`.
//!
//! EXPECTED: a representative orchestrator turn (mock provider) emits
//! `loop.turn.review` into the frozen temp ZEN_HOME, while the real
//! `~/.zen/logs/audit.jsonl` (if it exists) is byte-identical before and
//! after — same size+mtime, and no line carries the turn's session marker.
//!
//! ERRORS: robust when `~/.zen` does not exist (absence must be preserved).
//! A live `zen serve` daemon appending to the real audit file concurrently
//! could flake the size/mtime assertion — run the suite with the daemon
//! stopped for a deterministic guard.

mod common;

use std::path::PathBuf;
use std::time::SystemTime;

use zen_agents::orchestrator::AgentOrchestrator;
use zen_core::types::SessionContext;

/// The REAL global audit file, resolved via `home::home_dir()` — NOT
/// `ZenPaths::detect()`, which resolves to the frozen temp ZEN_HOME inside
/// this binary.
fn real_audit_path() -> Option<PathBuf> {
    home::home_dir().map(|h| h.join(".zen").join("logs").join("audit.jsonl"))
}

fn real_audit_snapshot() -> Option<(u64, SystemTime)> {
    let path = real_audit_path()?;
    let meta = std::fs::metadata(&path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

#[test]
fn real_audit_jsonl_untouched_by_orchestrator_turn() {
    let _guard = common::begin();

    // Snapshot the real file (or its absence) BEFORE the turn.
    let before = real_audit_snapshot();
    let before_exists = real_audit_path().map(|p| p.exists()).unwrap_or(false);

    // Run a representative orchestrator turn that emits loop.turn.review.
    // The audit line carries the auto-generated session_id, which is the
    // unique marker we search for in both the temp and the real file.
    let router = zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
        default_provider: Some("mock".to_string()),
        ..Default::default()
    })
    .with_mock_response("guard turn complete");
    let orchestrator =
        AgentOrchestrator::with_token_budget(router, 10_000_000).with_tool_loop_config(1);
    let mut session = SessionContext::new("Sisyphus".to_string(), String::new());
    let marker = session.session_id.to_string();
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(orchestrator.execute(&mut session, "guard turn"))
        .expect("turn must complete");

    // Sanity: the turn MUST have audited into the temp ZEN_HOME.
    let temp_audit = zen_core::paths::ZenPaths::detect()
        .expect("paths")
        .logs()
        .join("audit.jsonl");
    let temp_content = std::fs::read_to_string(&temp_audit).unwrap_or_default();
    assert!(
        temp_content.contains("loop.turn.review"),
        "the turn must emit loop.turn.review into the temp ZEN_HOME"
    );
    assert!(
        temp_content.contains(&marker),
        "the temp audit line must carry the turn's session marker"
    );

    // The real file must be untouched: same size+mtime if it existed,
    // still absent if it did not.
    let after = real_audit_snapshot();
    let after_exists = real_audit_path().map(|p| p.exists()).unwrap_or(false);
    assert_eq!(
        before_exists, after_exists,
        "real ~/.zen/logs/audit.jsonl existence must not change"
    );
    assert_eq!(
        before, after,
        "real ~/.zen/logs/audit.jsonl must be byte-identical (size+mtime) after a test turn"
    );

    // GOAL #2: no line with the test-session marker may appear in the real
    // file during the run.
    if let Some(path) = real_audit_path()
        && path.exists()
    {
        let real_content = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !real_content.contains(&marker),
            "real audit.jsonl must not contain the guard session marker"
        );
    }
}
