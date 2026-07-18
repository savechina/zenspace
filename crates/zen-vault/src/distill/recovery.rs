use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info};

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
