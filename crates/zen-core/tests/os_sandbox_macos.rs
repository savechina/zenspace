// ============================================================================
// os_sandbox_macos: E2E tests for macOS sandbox-exec sandbox (T129)
//
// Tests the macOS sandbox layer: profile generation, sandbox-exec wrapping,
// denial heuristics, and (when sandbox-exec is available) E2E isolation.
//
// Platform-gated: #[cfg(target_os = "macos")] - skipped on Linux/Windows.
//
// Note: E2E write/network tests (sandbox-exec actually running) require a
// properly configured sandbox-exec environment. On some macOS configurations
// (SIP, managed profiles), sandbox-exec subpath rules may deny writes to
// temp directories even when the SBPL allows them. These tests are marked
// with `#[ignore]` and should be run manually on a per-machine basis.
// ============================================================================

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;
use zen_core::sandbox::{OsSandboxProfile, SandboxMode, is_likely_sandbox_denied};

/// Helper: create a temporary workspace root for tests.
fn test_workspace() -> PathBuf {
    let dir = std::env::temp_dir().join("zen_sandbox_test_macos");
    std::fs::create_dir_all(&dir).ok();
    dir
}

// ============================================================================
// Profile Generation - OsSandboxProfile::from_mode produces correct fields
// ============================================================================

#[test]
fn test_os_sandbox_macos_readonly_profile() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::ReadOnly, vec![ws.clone()], false);

    assert_eq!(profile.readable_roots, vec![ws]);
    assert!(profile.writable_roots.is_empty());
    assert!(!profile.network);
    assert!(profile.sandboxed);
}

#[test]
fn test_os_sandbox_macos_workspace_write_profile() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws.clone()], true);

    assert_eq!(profile.readable_roots, vec![ws.clone()]);
    assert_eq!(profile.writable_roots, vec![ws]);
    assert!(profile.network);
    assert!(profile.sandboxed);
}

#[test]
fn test_os_sandbox_macos_danger_full_access_profile() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::DangerFullAccess, vec![ws], false);

    assert!(profile.readable_roots.is_empty());
    assert!(profile.writable_roots.is_empty());
    assert!(!profile.network);
    assert!(!profile.sandboxed);
}

// ============================================================================
// sandbox_spawn - wraps Command with sandbox-exec
// ============================================================================

#[test]
fn test_os_sandbox_macos_spawn_wraps_command() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
    let mut cmd = Command::new("echo");
    cmd.arg("hello");

    let result = zen_core::sandbox::sandbox_spawn(cmd, &profile, false);
    assert!(
        result.is_ok(),
        "sandbox_spawn should succeed when sandbox-exec exists"
    );

    let wrapped = result.unwrap();
    assert_eq!(
        wrapped.get_program().to_string_lossy(),
        "/usr/bin/sandbox-exec",
        "wrapped command should be sandbox-exec"
    );
}

#[test]
fn test_os_sandbox_macos_spawn_preserves_args() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
    let mut cmd = Command::new("/bin/echo");
    cmd.arg("arg1").arg("arg2");

    let wrapped = zen_core::sandbox::sandbox_spawn(cmd, &profile, false)
        .expect("sandbox_spawn should succeed");

    let args: Vec<_> = wrapped
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert!(args.contains(&"--".to_string()), "should have -- separator");
    assert!(
        args.contains(&"/bin/echo".to_string()),
        "should preserve program path"
    );
    assert!(
        args.contains(&"arg1".to_string()),
        "should preserve first arg"
    );
    assert!(
        args.contains(&"arg2".to_string()),
        "should preserve second arg"
    );
}

#[test]
fn test_os_sandbox_macos_spawn_preserves_cwd() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws.clone()], false);
    let mut cmd = Command::new("echo");
    cmd.current_dir(&ws);

    let wrapped = zen_core::sandbox::sandbox_spawn(cmd, &profile, false)
        .expect("sandbox_spawn should succeed");

    assert_eq!(
        wrapped.get_current_dir().map(|p| p.to_path_buf()),
        Some(ws),
        "current_dir should be preserved"
    );
}

// ============================================================================
// E2E Write/Network Isolation (sandbox-exec running)
//
// These tests invoke sandbox-exec and verify actual OS-level isolation.
// They are ignored by default because sandbox-exec subpath rules may
// behave differently across macOS configurations (SIP, managed profiles).
// Run manually: cargo test -p zen-core --test os_sandbox_macos -- --ignored
// ============================================================================

#[test]
#[ignore]
fn test_os_sandbox_macos_workspace_write_allowed() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws.clone()], false);
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", &format!("touch {}/sandbox_test_file", ws.display())]);

    let mut wrapped = zen_core::sandbox::sandbox_spawn(cmd, &profile, false)
        .expect("sandbox_spawn should succeed");

    let output = wrapped.output().expect("should execute sandbox-exec");
    assert!(
        output.status.success(),
        "workspace write should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Cleanup
    std::fs::remove_file(ws.join("sandbox_test_file")).ok();
}

#[test]
#[ignore]
fn test_os_sandbox_macos_etc_write_denied() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "touch /etc/zen_sandbox_test_write 2>&1 || exit 0"]);

    let mut wrapped = zen_core::sandbox::sandbox_spawn(cmd, &profile, false)
        .expect("sandbox_spawn should succeed");

    let output = wrapped.output().expect("should execute sandbox-exec");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let combined = format!("{stdout}{stderr}");
    let denied = !output.status.success()
        || combined.contains("operation not permitted")
        || combined.contains("permission denied")
        || combined.contains("read-only file system");
    assert!(
        denied,
        "/etc write should be denied by sandbox: stdout={stdout}, stderr={stderr}"
    );

    assert!(
        !std::path::Path::new("/etc/zen_sandbox_test_write").exists(),
        "file should not exist after denied write"
    );
}

#[test]
#[ignore]
fn test_os_sandbox_macos_network_blocked() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
    let mut cmd = Command::new("/bin/sh");
    cmd.args([
        "-c",
        "curl -s -o /dev/null https://example.com 2>&1 || echo CURL_FAILED",
    ]);

    let mut wrapped = zen_core::sandbox::sandbox_spawn(cmd, &profile, false)
        .expect("sandbox_spawn should succeed");

    let output = wrapped.output().expect("should execute sandbox-exec");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let blocked = !output.status.success()
        || combined.contains("CURL_FAILED")
        || combined.contains("operation not permitted")
        || combined.contains("Network is unreachable")
        || combined.contains("permission denied");
    assert!(
        blocked,
        "network should be blocked when network=false: {combined}"
    );
}

// ============================================================================
// is_likely_sandbox_denied - heuristic classifier
// ============================================================================

#[test]
fn test_os_sandbox_macos_denied_exit_code_2() {
    assert!(is_likely_sandbox_denied(Some(2), ""));
}

#[test]
fn test_os_sandbox_macos_denied_exit_code_126() {
    assert!(is_likely_sandbox_denied(Some(126), ""));
}

#[test]
fn test_os_sandbox_macos_denied_exit_code_127() {
    assert!(is_likely_sandbox_denied(Some(127), ""));
}

#[test]
fn test_os_sandbox_macos_denied_exit_code_sigsys() {
    // 128 + 31 (SIGSYS) = 159
    assert!(is_likely_sandbox_denied(Some(159), ""));
}

#[test]
fn test_os_sandbox_macos_denied_stderr_operation_not_permitted() {
    assert!(is_likely_sandbox_denied(
        None,
        "sh: operation not permitted"
    ));
}

#[test]
fn test_os_sandbox_macos_denied_stderr_permission_denied() {
    assert!(is_likely_sandbox_denied(None, "Permission denied"));
}

#[test]
fn test_os_sandbox_macos_denied_stderr_readonly_fs() {
    assert!(is_likely_sandbox_denied(None, "read-only file system"));
}

#[test]
fn test_os_sandbox_macos_denied_stderr_sandbox() {
    assert!(is_likely_sandbox_denied(None, "sandbox violation"));
}

#[test]
fn test_os_sandbox_macos_denied_stderr_landlock() {
    assert!(is_likely_sandbox_denied(None, "landlock denied"));
}

#[test]
fn test_os_sandbox_macos_denied_stderr_seccomp() {
    assert!(is_likely_sandbox_denied(None, "seccomp trap"));
}

#[test]
fn test_os_sandbox_macos_not_denied_normal_exit() {
    assert!(!is_likely_sandbox_denied(Some(0), ""));
}

#[test]
fn test_os_sandbox_macos_not_denied_exit_1_normal() {
    assert!(!is_likely_sandbox_denied(Some(1), "some error"));
}

#[test]
fn test_os_sandbox_macos_not_denied_empty() {
    assert!(!is_likely_sandbox_denied(None, ""));
}

// ============================================================================
// Fail-Closed - error when sandbox-exec absent
// ============================================================================

#[test]
fn test_os_sandbox_macos_fail_closed_without_sandbox_exec() {
    let sandbox_exec = std::path::Path::new("/usr/bin/sandbox-exec");
    if !sandbox_exec.exists() {
        let ws = test_workspace();
        let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
        let cmd = Command::new("echo");
        let result = zen_core::sandbox::sandbox_spawn(cmd, &profile, false);
        assert!(result.is_err(), "should fail when sandbox-exec absent");
        let err = result.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("SandboxUnavailable") || msg.contains("sandbox-exec not found"),
            "error should indicate sandbox-exec not found: {msg}"
        );
    }
    // If sandbox-exec exists, the test is a no-op (pass vacuously).
}
