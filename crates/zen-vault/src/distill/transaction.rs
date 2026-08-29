use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{info, warn};

/// Transactional scope for consolidation file-level operations.
///
/// Tracks file paths written during a transaction so they can be
/// cleaned up on rollback.  On commit the tracking file is deleted;
/// on rollback all tracked files are removed and the tracking file
/// is deleted.
pub struct TransactionScope {
    name: String,
    tracking_file: PathBuf,
}

impl TransactionScope {
    /// Create a new named transaction scope.
    ///
    /// The tracking file is written to the workspace's logs directory.
    pub fn new(name: &str) -> Self {
        use zen_core::paths::ZenPaths;
        let logs_dir = ZenPaths::detect()
            .map(|p| p.logs().to_path_buf())
            .unwrap_or_else(|_| PathBuf::from("."));
        let tracking_file = logs_dir.join(format!(".txn-{name}.jsonl"));
        Self {
            name: name.to_string(),
            tracking_file,
        }
    }

    /// Begin the transaction by creating or truncating the tracking file.
    pub fn begin(&self) -> Result<()> {
        if let Some(parent) = self.tracking_file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.tracking_file, "")
            .with_context(|| format!("create txn file: {}", self.tracking_file.display()))?;
        info!("Transaction begin: {}", self.name);
        Ok(())
    }

    /// Record a path that will be cleaned up on rollback.
    pub fn track_path(&self, path: &std::path::Path) -> Result<()> {
        use std::io::Write;
        let line = format!("{}\n", path.display());
        // F2 fix: append (fs::write truncated — only the last tracked path survived).
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.tracking_file)
            .with_context(|| format!("open txn file: {}", self.tracking_file.display()))?;
        file.write_all(line.as_bytes())
            .with_context(|| format!("track path: {}", path.display()))?;
        Ok(())
    }

    /// Commit the transaction by deleting the tracking file.
    pub fn commit(&self) -> Result<()> {
        if self.tracking_file.exists() {
            fs::remove_file(&self.tracking_file)
                .with_context(|| format!("remove txn file: {}", self.tracking_file.display()))?;
        }
        info!("Transaction commit: {}", self.name);
        Ok(())
    }

    /// Rollback the transaction by deleting all tracked files.
    pub fn rollback(&self) -> Result<()> {
        if !self.tracking_file.exists() {
            return Ok(());
        }

        let content = fs::read_to_string(&self.tracking_file)
            .with_context(|| format!("read txn file: {}", self.tracking_file.display()))?;

        let mut removed = 0usize;
        for line in content.lines() {
            let path = line.trim();
            if !path.is_empty() && std::path::Path::new(path).exists() {
                if let Err(e) = fs::remove_file(path) {
                    warn!(path, error = %e, "failed to rollback file");
                } else {
                    removed += 1;
                }
            }
        }

        fs::remove_file(&self.tracking_file).ok();
        info!(removed, "Transaction rollback: {}", self.name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_logs_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zen-txn-test-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// T006 F2: ≥3 tracked paths must ALL survive tracking and ALL be
    /// removed on rollback (the old fs::write impl kept only the last one).
    #[test]
    fn multi_file_rollback_restores_every_tracked_path() {
        let logs = temp_logs_dir();
        let work = logs.join("work");
        fs::create_dir_all(&work).unwrap();

        let txn = TransactionScope::new("multi-rollback-test");
        txn.begin().unwrap();

        let mut tracked = Vec::new();
        for i in 0..3 {
            let path = work.join(format!("page-{i}.md"));
            fs::write(&path, format!("content {i}")).unwrap();
            txn.track_path(&path).unwrap();
            tracked.push(path);
        }

        for path in &tracked {
            assert!(path.exists(), "{path:?} must exist pre-rollback");
        }

        txn.rollback().unwrap();

        for path in &tracked {
            assert!(!path.exists(), "{path:?} must be removed by rollback");
        }
        fs::remove_dir_all(&logs).ok();
    }

    #[test]
    fn commit_deletes_tracking_file_and_keeps_files() {
        let logs = temp_logs_dir();
        fs::create_dir_all(&logs).unwrap();
        let work = logs.join("work");
        fs::create_dir_all(&work).unwrap();

        let txn = TransactionScope::new("commit-keeps-test");
        txn.begin().unwrap();
        let path = work.join("kept.md");
        fs::write(&path, "kept").unwrap();
        txn.track_path(&path).unwrap();

        txn.commit().unwrap();
        assert!(path.exists(), "commit must keep tracked files");
        fs::remove_dir_all(&logs).ok();
    }
}
