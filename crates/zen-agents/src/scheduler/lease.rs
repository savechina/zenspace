//! Cross-process scheduler role lease (advisory flock).
//!
//! Hosts: the gateway daemon's Full-profile scheduler and the TUI's
//! InApp-profile scheduler both guard their `run()` with this lease, so at
//! most one learning-core scheduler fires per machine regardless of start
//! order (probe-then-spawn alone is TOCTOU — a daemon started after a TUI
//! probe, or two TUIs, would double-fire every cron worker).
//!
//! Semantics: flock on `<logs>/scheduler.lock`. The kernel drops the lock
//! when the holder's last fd closes or the process dies, so crashes
//! release the lease without cleanup. `try_acquire` never blocks — callers
//! decide whether to skip (TUI) or retry (daemon, which outlives TUIs and
//! is the canonical long-lived host).

use std::fs::{File, OpenOptions};
use std::path::Path;

use fs2::FileExt;
use zen_core::paths::ZenPaths;

/// Guard owning the lock file. Dropping it (or process exit) releases the lease.
pub struct SchedulerLease {
    _file: File,
}

impl SchedulerLease {
    /// Try to take the scheduler lease at `<logs>/scheduler.lock`.
    /// Returns `None` when another host holds it or the lock cannot be
    /// taken (callers treat both as "someone else owns the role").
    pub fn try_acquire(paths: &ZenPaths) -> Option<Self> {
        let dir = paths.logs();
        let _ = std::fs::create_dir_all(&dir);
        Self::try_acquire_at(&dir.join("scheduler.lock"))
    }

    /// Core acquire-or-fail on an explicit lock path (test seam).
    pub fn try_acquire_at(lock_path: &Path) -> Option<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)
            .ok()?;
        match file.try_lock_exclusive() {
            Ok(()) => Some(SchedulerLease { _file: file }),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn lock_env() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn second_acquire_fails_while_first_held() {
        let dir = lock_env();
        let path = dir.path().join("scheduler.lock");

        let first = SchedulerLease::try_acquire_at(&path);
        assert!(first.is_some(), "first acquire must succeed");

        let second = SchedulerLease::try_acquire_at(&path);
        assert!(second.is_none(), "second acquire must fail while held");
    }

    #[test]
    fn drop_releases_and_lease_is_reacquirable() {
        let dir = lock_env();
        let path = dir.path().join("scheduler.lock");

        drop(SchedulerLease::try_acquire_at(&path).unwrap());
        assert!(
            SchedulerLease::try_acquire_at(&path).is_some(),
            "lease must be reacquirable after drop"
        );
    }

    #[test]
    fn independent_paths_do_not_contend() {
        let dir = lock_env();
        let a = SchedulerLease::try_acquire_at(&dir.path().join("a.lock"));
        let b = SchedulerLease::try_acquire_at(&dir.path().join("b.lock"));
        assert!(
            a.is_some() && b.is_some(),
            "distinct locks must not contend"
        );
    }
}
