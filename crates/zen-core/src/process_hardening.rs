//! Process hardening for the zen binary (FR-044).
//!
//! Runs at the very start of `main()` before any other code to reduce the risk
//! of secret leakage via core dumps, debugger attachment, or library injection.
//! All operations are best-effort: failures are logged but never panic.

use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{info, warn};

static HARDENED: AtomicBool = AtomicBool::new(false);

/// Case-sensitive exact matches only — prefix-matching would wrongly strip
/// user vars like `LD_DEBUG` or `DYLD_FALLBACK_LIBRARY_PATH`.
const DANGEROUS_ENV_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
];

/// Initialize process hardening.
///
/// Call this at the very start of `main()`, before any other code (including
/// `.env` loading and the CLI dispatcher). Performs, in order:
///
/// 1. Strip `LD_*` / `DYLD_*` library-injection env vars.
/// 2. Deny debugger / ptrace attachment (platform-specific).
/// 3. Set `RLIMIT_CORE = 0` to disable core dumps.
///
/// All steps are best-effort; failures are logged and swallowed. The
/// [`is_hardened`] flag is set only after all steps have been attempted.
pub fn init() {
    strip_dangerous_env();
    deny_debugger();
    set_rlimit_core_zero();

    HARDENED.store(true, Ordering::SeqCst);
}

/// Returns `true` once [`init`] has completed.
///
/// This reports that the hardening attempts were made, not that the kernel
/// honored every request.
pub fn is_hardened() -> bool {
    HARDENED.load(Ordering::SeqCst)
}

// Two-pass: `std::env::vars()` borrows the env table, so collect names first
// and remove afterward to avoid mutation-during-iteration panics.
fn strip_dangerous_env() {
    let to_remove: Vec<String> = std::env::vars()
        .filter(|(name, _)| is_dangerous_env_var(name))
        .map(|(name, value)| {
            info!(env = %name, value_len = value.len(), "stripping dangerous env var");
            name
        })
        .collect();

    for name in &to_remove {
        // SAFETY: `remove_var` is unsafe only because concurrent env reads on
        // other threads could observe a torn state. This runs at process startup
        // before any other thread exists, and we have already finished the
        // `vars()` iteration above, so no concurrent env access is in flight.
        unsafe {
            std::env::remove_var(name);
        }
    }
}

fn is_dangerous_env_var(name: &str) -> bool {
    DANGEROUS_ENV_VARS.contains(&name)
}

fn deny_debugger() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `PR_SET_DUMPABLE` with argument `0` is a well-formed kernel
        // request. It takes no pointers we must keep valid — the only effect is
        // clearing the process dumpable flag. Return value is checked; on failure
        // we log and continue (best-effort).
        unsafe {
            if libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0 {
                warn!(
                    err = %std::io::Error::last_os_error(),
                    "prctl(PR_SET_DUMPABLE, 0) failed; process remains dumpable"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // SAFETY: `PT_DENY_ATTACH` takes a pid and two ignored args (null pointer,
        // 0). It instructs the kernel to reject any future ptrace attach to this
        // process. No pointers are dereferenced; the null is accepted by the
        // syscall. Return value is checked; on failure we log and continue.
        unsafe {
            if libc::ptrace(
                libc::PT_DENY_ATTACH,
                0,
                std::ptr::null_mut::<libc::c_char>(),
                0,
            ) != 0
            {
                warn!(
                    err = %std::io::Error::last_os_error(),
                    "ptrace(PT_DENY_ATTACH) failed; process remains attachable"
                );
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        info!("no debugger-denial hardening implemented for this platform");
    }
}

/// Best-effort `RLIMIT_CORE = 0` to suppress core dumps.
///
/// Mirrors the `rlimit` pattern in [`crate::sandbox::apply_resource_limits`]
/// but scoped to `RLIMIT_CORE` only — we do not constrain `NOFILE`/`NPROC` here.
fn set_rlimit_core_zero() {
    #[cfg(unix)]
    {
        // SAFETY: `setrlimit` reads two `rlim_t` fields from the `rlimit` struct
        // we pass by reference. The struct is fully initialized on the stack and
        // outlives the call. Setting both limits to 0 is a documented, legitimate
        // operation. The return value is checked; on failure we log and continue.
        unsafe {
            let rlimit_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &rlimit_core) != 0 {
                warn!(
                    err = %std::io::Error::last_os_error(),
                    "setrlimit(RLIMIT_CORE, 0) failed; core dumps may still be produced"
                );
            }
        }
    }

    #[cfg(not(unix))]
    {
        info!("RLIMIT_CORE hardening not applicable on this platform");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_env_predicate_matches_known_vectors() {
        for known in DANGEROUS_ENV_VARS {
            assert!(is_dangerous_env_var(known), "should match {known}");
        }
    }

    #[test]
    fn dangerous_env_predicate_rejects_unrelated_names() {
        // Must NOT match unrelated vars that merely share a prefix — stripping
        // `LD_DEBUG` or `DYLD_FALLBACK_LIBRARY_PATH` would surprise users.
        assert!(!is_dangerous_env_var("LD_DEBUG"));
        assert!(!is_dangerous_env_var("DYLD_FALLBACK_LIBRARY_PATH"));
        assert!(!is_dangerous_env_var("PATH"));
        assert!(!is_dangerous_env_var("HOME"));
        assert!(!is_dangerous_env_var(""));
    }

    #[test]
    fn dangerous_env_predicate_is_case_sensitive() {
        // env var names are case-sensitive on Unix; lowercase must not match.
        assert!(!is_dangerous_env_var("ld_preload"));
        assert!(!is_dangerous_env_var("Ld_Preload"));
    }

    #[test]
    fn is_hardened_is_true_after_init() {
        // `init()` has global side effects on the test process (sets RLIMIT_CORE=0,
        // strips dangerous env vars). This mirrors real startup and is acceptable
        // for the test binary. The hardened flag is set last, so once `init()`
        // returns it must read true.
        init();
        assert!(is_hardened(), "is_hardened() must be true after init()");
    }
}
