use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::info;

use super::checkpoint::Checkpoint;

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
    /// Currently a stub — real rollback logic deferred.
    pub fn recover(&self) -> Result<()> {
        info!("Recovery stub: would rollback incomplete consolidation");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_recover_is_stub() {
        let dir = tempdir().unwrap();
        let mgr = RecoveryManager::new(dir.path());
        assert!(mgr.recover().is_ok());
    }
}
