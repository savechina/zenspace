mod common;

use common::{ZenOutput, ZenTest};

#[test]
fn test_zen_version_shows_version() {
    let test = ZenTest::new();
    let output = test.zen(&["version"]);
    assert!(
        output.success(),
        "zen version should succeed: {}",
        output.stderr()
    );
    let stdout = output.stdout();
    assert!(stdout.contains("zen version:"));
}

#[test]
fn test_zen_session_status() {
    let test = ZenTest::new();
    let output = test.zen(&["session", "status"]);
    assert!(output.success(), "session status should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("Session Status") || stdout.contains("No active session"));
}

#[test]
fn test_zen_session_start_with_agent() {
    let test = ZenTest::new();
    let output = test.zen(&["session", "start", "--agent=hermes"]);
    assert!(output.success(), "session start should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("hermes"));
}

#[test]
fn test_zen_agent_list() {
    let test = ZenTest::new();
    let output = test.zen(&["agent", "list"]);
    assert!(output.success(), "agent list should succeed");
    let stdout = output.stdout().to_lowercase();
    assert!(stdout.contains("hermes"));
    assert!(stdout.contains("metis"));
}

#[test]
fn test_zen_agent_select() {
    let test = ZenTest::new();
    let output = test.zen(&["agent", "select", "codex"]);
    assert!(output.success(), "agent select should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("codex"));
}

#[test]
fn test_zen_workspace_init_creates_zen_dir() {
    let test = ZenTest::new();
    let output = test.zen(&["workspace", "init"]);
    assert!(output.success(), "workspace init should succeed");

    let zen_dir = test.cwd.join(".zen");
    assert!(zen_dir.exists(), ".zen/ directory should be created");
    assert!(
        zen_dir.join("knowledge").exists(),
        "knowledge/ should be created"
    );
    assert!(
        zen_dir.join("sessions").exists(),
        "sessions/ should be created"
    );
    assert!(zen_dir.join("memory").exists(), "memory/ should be created");
    assert!(
        zen_dir.join("knowledge/inbox").exists(),
        "knowledge/inbox/ should be created"
    );
    assert!(
        zen_dir.join("knowledge/raw").exists(),
        "knowledge/raw/ should be created"
    );
    assert!(
        zen_dir.join("knowledge/wiki").exists(),
        "knowledge/wiki/ should be created"
    );
    assert!(
        zen_dir.join("config.toml").exists(),
        "config.toml should be created"
    );
}

#[test]
fn test_zen_workspace_status() {
    let test = ZenTest::new();
    let output = test.zen(&["workspace", "status"]);
    assert!(output.success());
    let stdout = output.stdout();
    assert!(stdout.contains("Workspace Status"));
}

#[test]
fn test_zen_config_show() {
    let test = ZenTest::new();
    let output = test.zen(&["config", "show"]);
    assert!(output.success(), "config show should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("Merged Configuration") || stdout.contains("LLM"));
}

#[test]
fn test_zen_config_validate() {
    let test = ZenTest::new();
    let output = test.zen(&["config", "validate"]);
    assert!(output.success(), "config validate should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("valid") || stdout.contains("Configuration"));
}

#[test]
fn test_zen_provider_route() {
    let test = ZenTest::new();
    let output = test.zen(&["provider", "route", "--task=synthesis"]);
    assert!(output.success(), "provider route should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("Route Selection") || stdout.contains("synthesis"));
}

#[test]
fn test_zen_provider_list() {
    let test = ZenTest::new();
    let output = test.zen(&["provider", "list"]);
    assert!(output.success(), "provider providers should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("Providers") || stdout.contains("ollama"));
}

#[test]
fn test_zen_audit_log() {
    let test = ZenTest::new();
    let output = test.zen(&["audit", "log", "--session=abc123"]);
    assert!(output.success(), "audit log should succeed");
    let stdout = output.stdout();
    assert!(stdout.contains("abc123"));
}

#[test]
fn test_zen_help_includes_new_commands() {
    let test = ZenTest::new();
    let output = test.zen(&["--help"]);
    assert!(output.success());
    let stdout = output.stdout();
    assert!(
        stdout.contains("session"),
        "help should list session subcommand"
    );
    assert!(
        stdout.contains("agent"),
        "help should list agent subcommand"
    );
    assert!(
        stdout.contains("workspace"),
        "help should list workspace subcommand"
    );
    assert!(
        stdout.contains("config"),
        "help should list config subcommand"
    );
    assert!(
        stdout.contains("provider"),
        "help should list provider subcommand"
    );
    assert!(
        stdout.contains("audit"),
        "help should list audit subcommand"
    );
}

#[test]
fn test_zen_serve_status_not_running() {
    let test = ZenTest::new();
    let output = test.zen(&["serve", "status"]);
    assert!(output.success(), "serve status should succeed");
    let stdout = output.stdout();
    assert!(
        stdout.contains("not running") || stdout.contains("Gateway"),
        "should show gateway status"
    );
}

#[test]
fn test_zen_serve_start_foreground_help() {
    let test = ZenTest::new();
    let output = test.zen(&["serve", "start", "--help"]);
    assert!(output.success(), "serve start --help should succeed");
    let stdout = output.stdout();
    assert!(
        stdout.contains("--foreground"),
        "should show foreground option"
    );
    assert!(stdout.contains("--bind"), "should show bind option");
    assert!(stdout.contains("--port"), "should show port option");
}

#[test]
fn test_zen_serve_stop_no_pid_file() {
    let test = ZenTest::new();
    let output = test.zen(&["serve", "stop"]);
    assert!(
        output.success(),
        "serve stop should succeed even without PID file"
    );
    let stdout = output.stdout();
    assert!(
        stdout.contains("not running") || stdout.contains("no PID"),
        "should indicate no running gateway"
    );
}

#[test]
fn test_zen_help_lists_serve() {
    let test = ZenTest::new();
    let output = test.zen(&["--help"]);
    assert!(output.success());
    let stdout = output.stdout();
    assert!(
        stdout.contains("serve"),
        "help should list serve subcommand for gateway"
    );
}
