//! FR-038 resource-limit regression coverage.
//!
//! `apply_resource_limits` used to lower `RLIMIT_NPROC` to 50. That limit
//! counts processes/threads per REAL UID across the whole login session, so on
//! a desktop where the user owns hundreds of processes every `posix_spawn`
//! returned EAGAIN (2026-10-17: 5/5 tier-1 `rg` spawns failed and fell back to
//! the in-process walker; no child process could start). The cap is now gone,
//! and this pins both halves: NPROC is untouched, and a child can be spawned.

#[cfg(unix)]
mod unix {
    use std::process::Command;

    use zen_core::sandbox::apply_resource_limits;

    fn get(resource: libc::c_int) -> libc::rlimit {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes into a fully-initialized stack struct.
        // `as _` adapts the RLIMIT resource type across libc flavors (macOS
        // c_int vs Linux u32) without platform cfg.
        let rc = unsafe { libc::getrlimit(resource as _, &mut rl) };
        assert_eq!(rc, 0, "getrlimit failed for {resource}");
        rl
    }

    /// `apply_resource_limits` lowers the NOFILE soft cap for the rest of the
    /// process; under a shared-process runner that would outlive this test.
    struct NofileGuard {
        saved: libc::rlimit,
    }

    impl Drop for NofileGuard {
        fn drop(&mut self) {
            // SAFETY: setrlimit reads the fully-initialized struct by reference.
            unsafe {
                libc::setrlimit(libc::RLIMIT_NOFILE, &self.saved);
            }
        }
    }

    #[test]
    fn apply_resource_limits_leaves_nproc_alone_and_allows_spawning() {
        let _nofile_guard = NofileGuard {
            saved: get(libc::RLIMIT_NOFILE as _),
        };
        let nproc_before = get(libc::RLIMIT_NPROC as _);

        apply_resource_limits().expect("resource limits must apply");

        let nproc_after = get(libc::RLIMIT_NPROC as _);
        assert_eq!(
            nproc_after.rlim_cur, nproc_before.rlim_cur,
            "RLIMIT_NPROC is per-UID, not per-subtree: a soft cap here makes every \
             child spawn fail with EAGAIN on any busy login session"
        );

        let status = Command::new("/bin/echo")
            .arg("zen-spawn-probe")
            .status()
            .expect("spawning a child must succeed after apply_resource_limits");
        assert!(status.success(), "spawned child exited with {status}");
    }
}
