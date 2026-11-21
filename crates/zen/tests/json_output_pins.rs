mod common;

use common::ZenTest;

// ============================================================================
// JSON Output Pins — 2026-11-21 plan-eng-review T2 (F5)
// Every backfilled list/status/report-shaped subcommand honors `--json`
// (human output stays the default). Each pin spawns the real zen binary
// against a fresh isolated ZEN_HOME and asserts exit 0 + parseable JSON.
// The parse-level contract (flag wiring, human default) is pinned inline in
// crates/zen-cli/src/cli.rs `json_flags_parse`.
// ============================================================================

fn assert_json_parses(args: &[&str]) {
    let test = ZenTest::new();
    let output = test.zen(args);
    assert!(
        output.success(),
        "`zen {}` exited {:?}: {}",
        args.join(" "),
        output.output.status.code(),
        output.stderr()
    );
    let stdout = output.stdout();
    serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|e| {
        panic!(
            "`zen {}` stdout is not parseable JSON: {e}\n---\n{stdout}",
            args.join(" ")
        )
    });
}

#[test]
fn session_list_json_parses() {
    assert_json_parses(&["session", "list", "--json"]);
}

#[test]
fn session_status_json_parses() {
    assert_json_parses(&["session", "status", "--json"]);
}

#[test]
fn model_list_json_parses() {
    assert_json_parses(&["model", "list", "--json"]);
}

#[test]
fn provider_list_json_parses() {
    assert_json_parses(&["provider", "list", "--json"]);
}

#[test]
fn provider_route_json_parses() {
    assert_json_parses(&["provider", "route", "--task", "synthesis", "--json"]);
}

#[test]
fn config_show_json_parses() {
    assert_json_parses(&["config", "show", "--json"]);
}

#[test]
fn habit_list_json_parses() {
    assert_json_parses(&["habit", "list", "--json"]);
}

#[test]
fn goal_list_json_parses() {
    assert_json_parses(&["goal", "list", "--json"]);
}

#[test]
fn auth_list_json_parses() {
    assert_json_parses(&["auth", "list", "--json"]);
}

#[test]
fn auth_status_json_parses() {
    assert_json_parses(&["auth", "status", "--json"]);
}

#[test]
fn audit_log_json_parses() {
    assert_json_parses(&["audit", "log", "--session", "s1", "--json"]);
}

#[test]
fn plugin_list_json_parses() {
    assert_json_parses(&["plugin", "list", "--json"]);
}

#[test]
fn plugin_tools_json_parses() {
    assert_json_parses(&["plugin", "tools", "--json"]);
}

#[test]
fn discover_queue_json_parses() {
    assert_json_parses(&["discover", "queue", "--json"]);
}

#[test]
fn wiki_list_json_parses() {
    assert_json_parses(&["wiki", "list", "--json"]);
}
