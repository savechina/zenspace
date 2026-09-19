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
//!
//! T164: acquire failures are split into two distinguishable cases —
//! [`LeaseError::Contention`] (another host holds the lease; the caller
//! should back off or skip) and [`LeaseError::OpenFailed`] (the lock file
//! itself could not be opened; a real filesystem problem that must be
//! surfaced loudly, never silently treated as contention).

use std::fs::{File, OpenOptions};
use std::path::Path;

use fs2::FileExt;
use zen_core::paths::ZenPaths;

/// Why a lease acquire failed.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// Another host holds the lease (or the flock could not be taken).
    /// Expected under coexistence — callers back off or skip.
    #[error("scheduler lease held by another host")]
    Contention,
    /// The lock file could not be opened — a real filesystem problem,
    /// not coexistence. Callers must surface this loudly.
    #[error("scheduler lease lock file could not be opened: {0}")]
    OpenFailed(#[source] std::io::Error),
}

/// Guard owning the lock file. Dropping it (or process exit) releases the lease.
#[derive(Debug)]
pub struct SchedulerLease {
    _file: File,
}

impl SchedulerLease {
    /// Try to take the scheduler lease at `<logs>/scheduler.lock`.
    ///
    /// # Errors
    /// - [`LeaseError::Contention`] when another host holds the lease.
    /// - [`LeaseError::OpenFailed`] when the lock file cannot be opened.
    pub fn try_acquire(paths: &ZenPaths) -> Result<Self, LeaseError> {
        let dir = paths.logs();
        let _ = std::fs::create_dir_all(&dir);
        Self::try_acquire_at(&dir.join("scheduler.lock"))
    }

    /// Core acquire-or-fail on an explicit lock path (test seam).
    ///
    /// # Errors
    /// - [`LeaseError::Contention`] when another host holds the lease.
    /// - [`LeaseError::OpenFailed`] when the lock file cannot be opened.
    pub fn try_acquire_at(lock_path: &Path) -> Result<Self, LeaseError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)
            .map_err(LeaseError::OpenFailed)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(SchedulerLease { _file: file }),
            Err(_) => Err(LeaseError::Contention),
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
        assert!(first.is_ok(), "first acquire must succeed");

        let second = SchedulerLease::try_acquire_at(&path);
        assert!(
            matches!(second, Err(LeaseError::Contention)),
            "second acquire must report contention while held, got {second:?}"
        );
    }

    #[test]
    fn drop_releases_and_lease_is_reacquirable() {
        let dir = lock_env();
        let path = dir.path().join("scheduler.lock");

        drop(SchedulerLease::try_acquire_at(&path).unwrap());
        assert!(
            SchedulerLease::try_acquire_at(&path).is_ok(),
            "lease must be reacquirable after drop"
        );
    }

    #[test]
    fn independent_paths_do_not_contend() {
        let dir = lock_env();
        let a = SchedulerLease::try_acquire_at(&dir.path().join("a.lock"));
        let b = SchedulerLease::try_acquire_at(&dir.path().join("b.lock"));
        assert!(a.is_ok() && b.is_ok(), "distinct locks must not contend");
    }

    #[test]
    fn open_failure_is_distinguishable_from_contention() {
        let dir = lock_env();

        // A lock path whose parent directory does not exist cannot be
        // opened — this is an OpenFailed, NOT contention.
        let missing = dir.path().join("no-such-dir").join("scheduler.lock");
        match SchedulerLease::try_acquire_at(&missing) {
            Err(LeaseError::OpenFailed(_)) => {}
            other => panic!("expected OpenFailed for unopenable path, got {other:?}"),
        }

        // A held lease is contention, not an open failure.
        let path = dir.path().join("scheduler.lock");
        let _first = SchedulerLease::try_acquire_at(&path).unwrap();
        match SchedulerLease::try_acquire_at(&path) {
            Err(LeaseError::Contention) => {}
            other => panic!("expected Contention for held lease, got {other:?}"),
        }
    }
}
