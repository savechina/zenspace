use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Snapshot of consolidation state for crash recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub status: String,
    pub started_at: String,
    pub notes_count: usize,
}

/// Manages consolidation checkpoint persistence.
///
/// Writes JSON checkpoints to `logs_dir/consolidation-checkpoint.json`.
pub struct CheckpointManager {
    logs_dir: PathBuf,
}

impl CheckpointManager {
    /// Create a new checkpoint manager targeting `logs_dir`.
    pub fn new(logs_dir: &Path) -> Self {
        Self {
            logs_dir: logs_dir.to_path_buf(),
        }
    }

    fn checkpoint_path(&self) -> PathBuf {
        self.logs_dir.join("consolidation-checkpoint.json")
    }

    /// Write a checkpoint record as JSON.
    pub fn write_checkpoint(&self, checkpoint: &Checkpoint) -> Result<()> {
        std::fs::create_dir_all(&self.logs_dir)
            .with_context(|| format!("failed to create logs dir: {}", self.logs_dir.display()))?;

        let data = serde_json::to_string_pretty(checkpoint)
            .with_context(|| "failed to serialize checkpoint")?;

        std::fs::write(self.checkpoint_path(), data)
            .with_context(|| "failed to write checkpoint file")?;

        Ok(())
    }

    /// Read the last checkpoint, or `None` if no checkpoint exists.
    pub fn read_checkpoint(&self) -> Result<Option<Checkpoint>> {
        let path = self.checkpoint_path();

        if !path.exists() {
            return Ok(None);
        }

        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read checkpoint: {}", path.display()))?;

        let checkpoint =
            serde_json::from_str(&data).with_context(|| "failed to parse checkpoint JSON")?;

        Ok(Some(checkpoint))
    }

    /// Delete the checkpoint file (mark consolidation as complete).
    pub fn clear_checkpoint(&self) -> Result<()> {
        let path = self.checkpoint_path();

        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove checkpoint: {}", path.display()))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_write_and_read_checkpoint() {
        let dir = tempdir().unwrap();
        let mgr = CheckpointManager::new(dir.path());

        let cp = Checkpoint {
            status: "running".into(),
            started_at: "2025-01-01T00:00:00Z".into(),
            notes_count: 42,
        };
        mgr.write_checkpoint(&cp).unwrap();

        let read = mgr.read_checkpoint().unwrap().unwrap();
        assert_eq!(read.status, "running");
        assert_eq!(read.notes_count, 42);
    }

    #[test]
    fn test_read_none_when_no_checkpoint() {
        let dir = tempdir().unwrap();
        let mgr = CheckpointManager::new(dir.path());
        assert!(mgr.read_checkpoint().unwrap().is_none());
    }

    #[test]
    fn test_clear_checkpoint() {
        let dir = tempdir().unwrap();
        let mgr = CheckpointManager::new(dir.path());

        let cp = Checkpoint {
            status: "done".into(),
            started_at: "2025-01-01T00:00:00Z".into(),
            notes_count: 1,
        };
        mgr.write_checkpoint(&cp).unwrap();
        mgr.clear_checkpoint().unwrap();

        assert!(mgr.read_checkpoint().unwrap().is_none());
    }
}
