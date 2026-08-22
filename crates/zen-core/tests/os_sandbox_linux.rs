// ============================================================================
// os_sandbox_linux: E2E tests for Linux bubblewrap sandbox (T130)
//
// Tests the Linux sandbox layer: bubblewrap wrapping, write/network isolation,
// and bwrap-absent fail-closed behavior.
//
// Platform-gated: #[cfg(target_os = "linux")] - skipped on macOS/Windows.
// ============================================================================

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;
use zen_core::sandbox::{OsSandboxProfile, SandboxMode, is_likely_sandbox_denied};

/// Helper: create a temporary workspace root for tests.
fn test_workspace() -> PathBuf {
    let dir = std::env::temp_dir().join("zen_sandbox_test_linux");
    std::fs::create_dir_all(&dir).ok();
    dir
}

// ============================================================================
// Profile Generation - OsSandboxProfile::from_mode produces correct fields
// ============================================================================

#[test]
fn test_os_sandbox_linux_readonly_profile() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::ReadOnly, vec![ws.clone()], false);

    assert_eq!(profile.readable_roots, vec![ws]);
    assert!(profile.writable_roots.is_empty());
    assert!(!profile.network);
    assert!(profile.sandboxed);
}

#[test]
fn test_os_sandbox_linux_workspace_write_profile() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws.clone()], true);

    assert_eq!(profile.readable_roots, vec![ws.clone()]);
    assert_eq!(profile.writable_roots, vec![ws]);
    assert!(profile.network);
    assert!(profile.sandboxed);
}

#[test]
fn test_os_sandbox_linux_danger_full_access_profile() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::DangerFullAccess, vec![ws], false);

    assert!(profile.readable_roots.is_empty());
    assert!(profile.writable_roots.is_empty());
    assert!(!profile.network);
    assert!(!profile.sandboxed);
}

// ============================================================================
// sandbox_spawn - wraps Command with bubblewrap
// ============================================================================

#[test]
fn test_os_sandbox_linux_spawn_wraps_command() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
    let mut cmd = Command::new("echo");
    cmd.arg("hello");

    let result = zen_core::sandbox::sandbox_spawn(cmd, &profile, false);
    // On Linux, this should succeed if bubblewrap is installed,
    // or return SandboxUnavailable if not.
    match result {
        Ok(wrapped) => {
            assert_eq!(
                wrapped.get_program().to_string_lossy(),
                "bwrap",
                "wrapped command should be bubblewrap"
            );
        }
        Err(e) => {
            // bubblewrap not installed - verify error type
            let msg = format!("{e}");
            assert!(
                msg.contains("SandboxUnavailable") || msg.contains("bubblewrap"),
                "error should indicate bubblewrap not found: {msg}"
            );
        }
    }
}

#[test]
fn test_os_sandbox_linux_spawn_preserves_args() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
    let mut cmd = Command::new("/bin/echo");
    cmd.arg("arg1").arg("arg2");

    let wrapped = match zen_core::sandbox::sandbox_spawn(cmd, &profile, false) {
        Ok(w) => w,
        Err(_) => return, // bwrap not installed, skip
    };

    let args: Vec<_> = wrapped
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
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
fn test_os_sandbox_linux_spawn_preserves_cwd() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws.clone()], false);
    let mut cmd = Command::new("echo");
    cmd.current_dir(&ws);

    let wrapped = match zen_core::sandbox::sandbox_spawn(cmd, &profile, false) {
        Ok(w) => w,
        Err(_) => return, // bwrap not installed, skip
    };

    // bubblewrap uses --chdir to set working directory
    let args: Vec<_> = wrapped
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let has_chdir = args
        .windows(2)
        .any(|w| w[0] == "--chdir" && w[1] == ws.to_string_lossy().as_ref());
    assert!(has_chdir, "bubblewrap should preserve cwd via --chdir");
}

// ============================================================================
// E2E Write/Network Isolation (bubblewrap running)
//
// These tests invoke bubblewrap and verify actual OS-level isolation.
// They are ignored by default because bubblewrap may not be installed.
// Run manually: cargo test -p zen-core --test os_sandbox_linux -- --ignored
// ============================================================================

#[test]
#[ignore]
fn test_os_sandbox_linux_workspace_write_allowed() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws.clone()], false);
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", &format!("touch {}/sandbox_test_file", ws.display())]);

    let wrapped = match zen_core::sandbox::sandbox_spawn(cmd, &profile, false) {
        Ok(w) => w,
        Err(_) => return, // bwrap not installed, skip
    };

    let output = wrapped.output().expect("should execute bubblewrap");
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
fn test_os_sandbox_linux_etc_write_denied() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "touch /etc/zen_sandbox_test_write 2>&1 || exit 0"]);

    let wrapped = match zen_core::sandbox::sandbox_spawn(cmd, &profile, false) {
        Ok(w) => w,
        Err(_) => return, // bwrap not installed, skip
    };

    let output = wrapped.output().expect("should execute bubblewrap");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let combined = format!("{stdout}{stderr}");
    let denied = !output.status.success()
        || combined.contains("operation not permitted")
        || combined.contains("permission denied")
        || combined.contains("read-only file system")
        || combined.contains("Read-only file system");
    assert!(
        denied,
        "/etc write should be denied by sandbox: stdout={stdout}, stderr={stderr}"
    );

    assert!(
        !std::path::Path::new("/etc/zen_sandbox_test_write").exists(),
        "file should not exist after denied write"
    );
}

// ============================================================================

#[test]
#[ignore]
fn test_os_sandbox_linux_network_blocked() {
    let ws = test_workspace();
    let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
    let mut cmd = Command::new("/bin/sh");
    cmd.args([
        "-c",
        "curl -s -o /dev/null https://example.com 2>&1 || echo CURL_FAILED",
    ]);

    let wrapped = match zen_core::sandbox::sandbox_spawn(cmd, &profile, false) {
        Ok(w) => w,
        Err(_) => return, // bwrap not installed, skip
    };

    let output = wrapped.output().expect("should execute bubblewrap");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let blocked = !output.status.success()
        || combined.contains("CURL_FAILED")
        || combined.contains("operation not permitted")
        || combined.contains("Network is unreachable")
        || combined.contains("permission denied")
        || combined.contains("unshare-net");
    assert!(
        blocked,
        "network should be blocked when network=false: {combined}"
    );
}

// ============================================================================
// is_likely_sandbox_denied - heuristic classifier (shared with macOS)
// ============================================================================

#[test]
fn test_os_sandbox_linux_denied_exit_code_2() {
    assert!(is_likely_sandbox_denied(Some(2), ""));
}

#[test]
fn test_os_sandbox_linux_denied_exit_code_126() {
    assert!(is_likely_sandbox_denied(Some(126), ""));
}

#[test]
fn test_os_sandbox_linux_denied_exit_code_127() {
    assert!(is_likely_sandbox_denied(Some(127), ""));
}

#[test]
fn test_os_sandbox_linux_denied_exit_code_sigsys() {
    // 128 + 31 (SIGSYS) = 159
    assert!(is_likely_sandbox_denied(Some(159), ""));
}

#[test]
fn test_os_sandbox_linux_denied_stderr_operation_not_permitted() {
    assert!(is_likely_sandbox_denied(
        None,
        "sh: operation not permitted"
    ));
}

#[test]
fn test_os_sandbox_linux_denied_stderr_permission_denied() {
    assert!(is_likely_sandbox_denied(None, "Permission denied"));
}

#[test]
fn test_os_sandbox_linux_denied_stderr_seccomp() {
    assert!(is_likely_sandbox_denied(None, "seccomp: SIGSYS"));
}

#[test]
fn test_os_sandbox_linux_denied_stderr_landlock() {
    assert!(is_likely_sandbox_denied(None, "landlock: access denied"));
}

#[test]
fn test_os_sandbox_linux_not_denied_normal_exit() {
    assert!(!is_likely_sandbox_denied(Some(0), ""));
}

#[test]
fn test_os_sandbox_linux_not_denied_exit_1_normal() {
    assert!(!is_likely_sandbox_denied(Some(1), "some error"));
}

#[test]
fn test_os_sandbox_linux_not_denied_empty() {
    assert!(!is_likely_sandbox_denied(None, ""));
}

// ============================================================================
// Fail-Closed - error when bubblewrap absent
// ============================================================================

#[test]
fn test_os_sandbox_linux_fail_closed_without_bwrap() {
    let bwrap = which("bwrap");
    if bwrap.is_none() {
        // bubblewrap not present - verify fail-closed behavior
        let ws = test_workspace();
        let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![ws], false);
        let cmd = Command::new("echo");
        let result = zen_core::sandbox::sandbox_spawn(cmd, &profile, false);
        assert!(result.is_err(), "should fail when bubblewrap absent");
        let err = result.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("SandboxUnavailable") || msg.contains("bubblewrap"),
            "error should indicate bubblewrap not found: {msg}"
        );
    }
    // If bubblewrap exists, the test is a no-op (pass vacuously).
}

/// Simple which-like lookup for bwrap.
fn which(name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let p = std::path::Path::new(dir).join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}
