mod common;

use common::ZenTest;

// ============================================================================
// CLI Smoke Tests — all 24+ zen commands
//
// Each test runs `zen <command>` as a subprocess and verifies exit code 0.
// These supplement the existing integration_test.rs with additional coverage.
// ============================================================================

// ---------------------------------------------------------------------------
// Version & Help (3 tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_version_shows_version() {
    let test = ZenTest::new();
    let output = test.zen(&["version"]);
    assert!(output.success(), "zen version should succeed");
    assert!(output.stdout().contains("zen version:"));
}

#[test]
fn test_zen_help_short_lists_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["-h"]);
    assert!(output.success(), "zen -h should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(stdout.contains("session"));
    assert!(stdout.contains("agent"));
    assert!(stdout.contains("workspace"));
}

#[test]
fn test_zen_help_long_shows_detailed_help() {
    let test = ZenTest::new();
    let output = test.zen(&["--help"]);
    assert!(output.success(), "zen --help should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("Usage:") || stdout.contains("Commands:"));
}

// ---------------------------------------------------------------------------
// Workspace Commands (3 tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_workspace_init_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["workspace", "init"]);
    assert!(output.success(), "workspace init should succeed");
    let zen_dir = test.cwd.join(".zen");
    assert!(zen_dir.exists(), ".zen/ directory should be created");
}

#[test]
fn test_zen_workspace_status_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["workspace", "status"]);
    assert!(output.success(), "workspace status should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("Workspace") || stdout.contains("Status"));
}

#[test]
fn test_zen_workspace_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["workspace", "--help"]);
    assert!(output.success(), "workspace --help should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(stdout.contains("init") || stdout.contains("status"));
}

// ---------------------------------------------------------------------------
// Config Commands (2 tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_config_show_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["config", "show"]);
    assert!(output.success(), "config show should succeed");
    let stdout = output.stdout();
    assert!(
        stdout.contains("Configuration") || stdout.contains("LLM"),
        "config show should output configuration"
    );
}

#[test]
fn test_zen_config_validate_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["config", "validate"]);
    assert!(output.success(), "config validate should succeed");
}

// ---------------------------------------------------------------------------
// Agent Commands (2 tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_agent_list_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["agent", "list"]);
    assert!(output.success(), "agent list should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(
        stdout.contains("hermes"),
        "agent list should include hermes"
    );
}

#[test]
fn test_zen_agent_select_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["agent", "select", "codex"]);
    assert!(output.success(), "agent select should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(stdout.contains("codex"));
}

// ---------------------------------------------------------------------------
// Session Commands (3 tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_session_status_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["session", "status"]);
    assert!(output.success(), "session status should succeed");
}

#[test]
fn test_zen_session_start_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["session", "start", "--agent=hermes"]);
    assert!(output.success(), "session start should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(
        stdout.contains("hermes"),
        "session start should mention agent"
    );
}

#[test]
fn test_zen_session_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["session", "--help"]);
    assert!(output.success(), "session --help should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(
        stdout.contains("start") || stdout.contains("status"),
        "session help should list subcommands"
    );
}

// ---------------------------------------------------------------------------
// Provider Commands (2 tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_provider_list_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["provider", "list"]);
    assert!(output.success(), "provider list should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(stdout.contains("provider") || stdout.contains("ollama"));
}

#[test]
fn test_zen_provider_route_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["provider", "route", "--task=synthesis"]);
    assert!(output.success(), "provider route should succeed");
}

// ---------------------------------------------------------------------------
// Note / Knowledge Commands (3 tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_note_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["note", "--help"]);
    assert!(output.success(), "note --help should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(stdout.contains("create"), "note help should list create");
}

#[test]
fn test_zen_search_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["search", "--help"]);
    assert!(output.success(), "search --help should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(
        stdout.contains("run") || stdout.contains("query"),
        "search help should list subcommands"
    );
}

#[test]
fn test_zen_lint_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["lint", "--help"]);
    assert!(output.success(), "lint --help should succeed");
}

// ---------------------------------------------------------------------------
// Gateway & Utility Commands (6 tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_serve_status_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["serve", "status"]);
    assert!(output.success(), "serve status should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(
        stdout.contains("not running") || stdout.contains("gateway"),
        "serve status should report gateway state"
    );
}

#[test]
fn test_zen_serve_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["serve", "--help"]);
    assert!(output.success(), "serve --help should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(stdout.contains("start") || stdout.contains("status"));
}

#[test]
fn test_zen_audit_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["audit", "--help"]);
    assert!(output.success(), "audit --help should succeed");
}

#[test]
fn test_zen_clean_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["clean", "--help"]);
    assert!(output.success(), "clean --help should succeed");
}

#[test]
fn test_zen_starter_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["starter", "--help"]);
    assert!(output.success(), "starter --help should succeed");
}

#[test]
fn test_zen_plugin_help_shows_subcommands() {
    let test = ZenTest::new();
    let output = test.zen(&["plugin", "--help"]);
    assert!(output.success(), "plugin --help should succeed");
}

// ---------------------------------------------------------------------------
// Additional CLI Commands (remaining — help-only smoke tests)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_similar_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["similar", "--help"]);
    assert!(output.success(), "similar --help should succeed");
}

#[test]
fn test_zen_graph_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["graph", "--help"]);
    assert!(output.success(), "graph --help should succeed");
}

#[test]
fn test_zen_reindex_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["reindex", "--help"]);
    assert!(output.success(), "reindex --help should succeed");
}

#[test]
fn test_zen_research_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["research", "--help"]);
    assert!(output.success(), "research --help should succeed");
}

#[test]
fn test_zen_distill_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["distill", "--help"]);
    assert!(output.success(), "distill --help should succeed");
}

#[test]
fn test_zen_ingest_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["ingest", "--help"]);
    assert!(output.success(), "ingest --help should succeed");
}

#[test]
fn test_zen_routine_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["routine", "--help"]);
    assert!(output.success(), "routine --help should succeed");
}

#[test]
fn test_zen_task_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["task", "--help"]);
    assert!(output.success(), "task --help should succeed");
}

#[test]
fn test_zen_brief_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["brief", "--help"]);
    assert!(output.success(), "brief --help should succeed");
}

#[test]
fn test_zen_auth_help_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["auth", "--help"]);
    assert!(output.success(), "auth --help should succeed");
}

// ---------------------------------------------------------------------------
// Edge case: unknown command returns failure (not panic)
// ---------------------------------------------------------------------------

#[test]
fn test_zen_unknown_command_fails_gracefully() {
    let test = ZenTest::new();
    let output = test.zen(&["nonexistent_subcommand_xyz"]);
    assert!(!output.success(), "unknown command should fail");
}
