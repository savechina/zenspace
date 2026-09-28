use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::checkpoint::Checkpoint;

/// Tracking files younger than this are assumed to belong to a live cycle
/// and are never replayed. A distill cycle is bounded far below this
/// (turn watchdog 900s), so a `.txn-*.jsonl` older than an hour belongs to
/// a dead process with certainty.
pub const STALE_TXN_MIN_AGE: Duration = Duration::from_secs(3600);

/// Outcome of one crash-replay pass (compile-hygiene ④).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayReport {
    /// Stale `.txn-*.jsonl` tracking files found and replayed.
    pub tracking_files: usize,
    /// Tracked files deleted by the replay (the interrupted cycle's
    /// partial writes).
    pub files_removed: usize,
}

/// Handles recovery from incomplete consolidation runs.
pub struct RecoveryManager {
    logs_dir: PathBuf,
}

impl RecoveryManager {
    /// Create a new recovery manager targeting `logs_dir`.
    pub fn new(logs_dir: &Path) -> Self {
        Self {
            logs_dir: logs_dir.to_path_buf(),
        }
    }

    /// Check for an incomplete consolidation checkpoint.
    ///
    /// Returns `Some(Checkpoint)` if a checkpoint exists with status != "completed",
    /// otherwise `None`.
    pub fn check_incomplete(&self) -> Result<Option<Checkpoint>> {
        let mgr = super::checkpoint::CheckpointManager::new(&self.logs_dir);
        let maybe = mgr.read_checkpoint()?;

        match maybe {
            Some(cp) if cp.status != "completed" => Ok(Some(cp)),
            _ => Ok(None),
        }
    }

    /// Recover from an incomplete consolidation.
    ///
    /// Reads the checkpoint to determine what was in progress, then
    /// clears it so the next pipeline run can restart from scratch.
    /// Returns the checkpoint that was recovered (if any).
    pub fn recover(&self) -> Result<Option<Checkpoint>> {
        let incomplete = self.check_incomplete()?;
        match incomplete {
            Some(cp) => {
                info!(
                    status = %cp.status,
                    notes_count = cp.notes_count,
                    "recovering from incomplete consolidation"
                );
                let mgr = super::checkpoint::CheckpointManager::new(&self.logs_dir);
                mgr.clear_checkpoint().with_context(|| {
                    format!("clear checkpoint in logs dir: {}", self.logs_dir.display())
                })?;
                Ok(Some(cp))
            }
            None => {
                debug!("no incomplete consolidation to recover");
                Ok(None)
            }
        }
    }

    /// Crash replay (compile-hygiene ④): roll back every stale
    /// `.txn-*.jsonl` tracking file left behind by a dead cycle. Uses the
    /// default staleness age ([`STALE_TXN_MIN_AGE`]).
    ///
    /// Runs at cycle start, before the new cycle's `txn.begin()` — a
    /// tracking file present at that moment with age above the threshold
    /// belongs to an interrupted cycle, and its tracked paths are exactly
    /// the partial writes that must be removed (full rollback is the
    /// correct transaction semantics: nothing is durable without commit).
    pub fn replay_stale_transactions(&self) -> Result<ReplayReport> {
        self.replay_stale_transactions_older_than(STALE_TXN_MIN_AGE)
    }

    /// [`replay_stale_transactions`](Self::replay_stale_transactions) with
    /// an injectable age threshold (tests pass `Duration::ZERO`).
    pub fn replay_stale_transactions_older_than(&self, min_age: Duration) -> Result<ReplayReport> {
        let mut report = ReplayReport::default();
        let Ok(entries) = std::fs::read_dir(&self.logs_dir) else {
            return Ok(report);
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !(name.starts_with(".txn-") && name.ends_with(".jsonl") && path.is_file()) {
                continue;
            }
            let age_ok = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age >= min_age);
            if !age_ok {
                debug!(txn = %name, "txn tracking file is fresh — assuming a live cycle");
                continue;
            }

            let removed = super::transaction::rollback_tracking_file(&path)?;
            report.tracking_files += 1;
            report.files_removed += removed;
            info!(
                txn = %name,
                files_removed = removed,
                "replayed stale transaction from an interrupted cycle"
            );
        }

        if report.tracking_files > 0 {
            warn!(
                tracking_files = report.tracking_files,
                files_removed = report.files_removed,
                "crash replay rolled back partial writes from interrupted cycle(s)"
            );
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_check_incomplete_none_when_no_checkpoint() {
        let dir = tempdir().unwrap();
        let mgr = RecoveryManager::new(dir.path());
        assert!(mgr.check_incomplete().unwrap().is_none());
    }

    #[test]
    fn test_check_incomplete_returns_running() {
        let dir = tempdir().unwrap();
        let mgr = RecoveryManager::new(dir.path());

        let cp = Checkpoint {
            status: "running".into(),
            started_at: "2025-01-01T00:00:00Z".into(),
            notes_count: 5,
        };
        super::super::checkpoint::CheckpointManager::new(dir.path())
            .write_checkpoint(&cp)
            .unwrap();

        let result = mgr.check_incomplete().unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().status, "running");
    }

    #[test]
    fn test_check_incomplete_ignores_completed() {
        let dir = tempdir().unwrap();
        let mgr = RecoveryManager::new(dir.path());

        let cp = Checkpoint {
            status: "completed".into(),
            started_at: "2025-01-01T00:00:00Z".into(),
            notes_count: 5,
        };
        super::super::checkpoint::CheckpointManager::new(dir.path())
            .write_checkpoint(&cp)
            .unwrap();

        assert!(mgr.check_incomplete().unwrap().is_none());
    }

    #[test]
    fn recover_clears_a_running_checkpoint() {
        let dir = tempdir().unwrap();
        let mgr = RecoveryManager::new(dir.path());

        super::super::checkpoint::CheckpointManager::new(dir.path())
            .write_checkpoint(&Checkpoint {
                status: "running".into(),
                started_at: "2025-01-01T00:00:00Z".into(),
                notes_count: 3,
            })
            .unwrap();

        let recovered = mgr.recover().unwrap();
        assert!(
            recovered.is_some(),
            "a running checkpoint must be recovered"
        );
        assert!(
            mgr.check_incomplete().unwrap().is_none(),
            "recovery must clear the checkpoint for idempotent restart"
        );
    }

    // ── Crash replay (compile-hygiene ④) ────────────────────────

    /// Simulate a cycle killed mid-write: `begin()` + tracked writes, no
    /// commit, no rollback — the state a dead process leaves on disk.
    fn simulate_crashed_cycle(logs: &Path, work: &Path, names: &[&str]) {
        let txn = super::super::transaction::TransactionScope::new_for_logs(logs, "crash-sim");
        txn.begin().unwrap();
        for name in names {
            let path = work.join(name);
            fs::write(&path, format!("partial write of {name}")).unwrap();
            txn.track_path(&path).unwrap();
        }
        // No commit, no rollback — the "process death" point.
    }

    #[test]
    fn crash_mid_write_replay_removes_tracked_files() {
        let dir = tempdir().unwrap();
        let logs = dir.path().join("logs");
        let work = dir.path().join("wiki");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&work).unwrap();

        simulate_crashed_cycle(&logs, &work, &["page-a.md", "page-b.md"]);
        assert!(work.join("page-a.md").exists());
        assert!(logs.join(".txn-crash-sim.jsonl").exists());

        let mgr = RecoveryManager::new(&logs);
        let report = mgr
            .replay_stale_transactions_older_than(Duration::ZERO)
            .unwrap();

        assert_eq!(report.tracking_files, 1);
        assert_eq!(report.files_removed, 2, "both partial writes must go");
        assert!(!work.join("page-a.md").exists());
        assert!(!work.join("page-b.md").exists());
        assert!(
            !logs.join(".txn-crash-sim.jsonl").exists(),
            "replayed tracking file must be consumed"
        );
    }

    #[test]
    fn crash_after_write_before_commit_rolls_back_everything() {
        let dir = tempdir().unwrap();
        let logs = dir.path().join("logs");
        let work = dir.path().join("wiki");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&work).unwrap();

        // Kill point AFTER all writes landed but BEFORE commit: every
        // written file is rolled back — full rollback is the correct
        // transaction semantics (nothing is durable without commit), and
        // this test pins that choice explicitly.
        simulate_crashed_cycle(&logs, &work, &["done-looking.md"]);
        let replay = RecoveryManager::new(&logs)
            .replay_stale_transactions_older_than(Duration::ZERO)
            .unwrap();
        assert_eq!(replay.files_removed, 1);
        assert!(
            !work.join("done-looking.md").exists(),
            "a written-but-uncommitted file must not survive crash replay"
        );
    }

    #[test]
    fn fresh_tracking_file_belongs_to_live_cycle_and_is_untouched() {
        let dir = tempdir().unwrap();
        let logs = dir.path().join("logs");
        let work = dir.path().join("wiki");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&work).unwrap();

        simulate_crashed_cycle(&logs, &work, &["live.md"]);

        // Default threshold (1h): a just-created tracking file is a live
        // cycle's and must not be replayed.
        let report = RecoveryManager::new(&logs)
            .replay_stale_transactions()
            .unwrap();
        assert_eq!(report.tracking_files, 0);
        assert!(work.join("live.md").exists(), "live cycle writes stay");
        assert!(logs.join(".txn-crash-sim.jsonl").exists());
    }

    #[test]
    fn clean_logs_replay_is_a_no_op() {
        let dir = tempdir().unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir_all(&logs).unwrap();

        let report = RecoveryManager::new(&logs)
            .replay_stale_transactions_older_than(Duration::ZERO)
            .unwrap();
        assert_eq!(report, ReplayReport::default());
    }
}
