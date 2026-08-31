//! OCC/CAS transactional scope for consolidation file-level operations (FR-032).
//!
//! # Optimistic Concurrency (OCC/CAS) design
//!
//! Reasoning happens *outside* any lock: the caller captures a cheap
//! content-version snapshot of the files it will touch
//! ([`VersionSnapshot::capture`]), performs all expensive work (LLM calls,
//! diffing, merge planning), then conditionally commits only if the captured
//! versions are still current ([`TransactionScope::commit_conditional`]).
//! The conditional filesystem write ([`write_if_unchanged`]) and the
//! transaction commit succeed or fail together — if CAS detects drift, the
//! tracked files are rolled back instead of committing half a cycle.
//!
//! # Tiered-storage framing
//!
//! The working set (files being reasoned about this cycle) is the
//! *snapshot*; the durable tier is the vault files themselves.  The
//! snapshot is cheap and ephemeral; the vault files are the source of
//! truth.  CAS bridges the two: a commit only lands when the durable tier
//! still matches what the reasoning observed.
//!
//! # Flow
//!
//! 1. `VersionSnapshot::capture(paths)` — hash the current content of every
//!    path (missing files → `None`).
//! 2. Reason outside the lock (LLM extraction, merge planning, ...).
//! 3. `write_if_unchanged(path, expected, content)` for each mutation.
//! 4. `commit_conditional(&snapshot)` — no drift → `commit()`; drift →
//!    `rollback()` and report the drifted paths.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

/// Compute the SHA-256 content version of a file.
///
/// Uses the same hash algorithm as `zen-repo`'s `notes_repo.rs`
/// (`content_hash`), keeping versioning consistent across the workspace.
///
/// # Parameters
/// - `path`: file to hash.
///
/// # Returns
/// `Ok(Some(hex))` with the 64-char lowercase hex SHA-256 of the file
/// content; `Ok(None)` if the file does not exist.
///
/// # Errors
/// `anyhow::Error` if the file exists but cannot be read (permission
/// denied, is a directory, I/O failure).
///
/// # Example
/// ```text
/// let v = content_version(Path::new("/tmp/page.md")).unwrap();
/// // Some("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08")
/// ```
pub fn content_version(path: &Path) -> Result<Option<String>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("read for content version: {}", path.display()));
        }
    };
    let hash = Sha256::digest(&bytes);
    Ok(Some(hash.iter().map(|b| format!("{:02x}", b)).collect()))
}

/// Outcome of a conditional write ([`write_if_unchanged`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The expected version matched and the file was written.
    Committed,
    /// The file drifted since capture; nothing was written.
    Conflict,
}

/// Outcome of a conditional transaction commit
/// ([`TransactionScope::commit_conditional`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasCommitOutcome {
    /// No drift detected; the transaction committed normally.
    Committed,
    /// Drift detected; tracked files were rolled back.
    RolledBack {
        /// Paths whose content version changed (or was created/deleted)
        /// since the snapshot was captured.
        drifted: Vec<PathBuf>,
    },
}

/// Content-version snapshot of a working set (OCC/CAS capture).
///
/// Captures the SHA-256 content version of every path up front so the
/// caller can reason outside any lock and later verify nothing drifted.
pub struct VersionSnapshot {
    versions: HashMap<PathBuf, Option<String>>,
}

impl VersionSnapshot {
    /// Capture the current content version of every path.
    ///
    /// Missing files are recorded as `None` so later creation is detected
    /// as drift.
    ///
    /// # Parameters
    /// - `paths`: files to snapshot.
    ///
    /// # Returns
    /// `Ok(Self)` with one entry per path.
    ///
    /// # Errors
    /// `anyhow::Error` if any existing file cannot be read.
    ///
    /// # Example
    /// ```text
    /// let snap = VersionSnapshot::capture(&[page_a, page_b]).unwrap();
    /// ```
    pub fn capture(paths: &[PathBuf]) -> Result<Self> {
        let mut versions = HashMap::with_capacity(paths.len());
        for path in paths {
            versions.insert(path.clone(), content_version(path)?);
        }
        Ok(Self { versions })
    }

    /// Verify the snapshot against the live filesystem.
    ///
    /// # Returns
    /// `Ok(Vec<PathBuf>)` — the drifted paths: files whose content changed,
    /// files created since capture (were `None`), or files deleted since
    /// capture (now `None`).  Empty when nothing drifted.
    ///
    /// # Errors
    /// `anyhow::Error` if any path cannot be read.
    ///
    /// # Example
    /// ```text
    /// let drifted = snap.verify().unwrap();
    /// assert!(drifted.is_empty());
    /// ```
    pub fn verify(&self) -> Result<Vec<PathBuf>> {
        let mut drifted = Vec::new();
        for (path, expected) in &self.versions {
            let current = content_version(path)?;
            if current.as_deref() != expected.as_deref() {
                drifted.push(path.clone());
            }
        }
        Ok(drifted)
    }
}

/// Conditionally write `content` to `path` only if its content version
/// still matches `expected` (OCC/CAS compare-and-swap, FR-032).
///
/// Compares the live SHA-256 content version against `expected`; on match
/// the write is performed via the shared tmp+rename primitive
/// (`crate::wiki::writer::atomic_write`), on drift nothing is written.
///
/// # Parameters
/// - `path`: absolute target path.
/// - `expected`: content version observed at capture time (`None` = file
///   did not exist then).
/// - `content`: full file content to write on match.
///
/// # Returns
/// `Ok(WriteOutcome::Committed)` if written, `Ok(WriteOutcome::Conflict)`
/// if drift was detected.
///
/// # Errors
/// `std::io::Error` if the version read or the atomic write fails.
///
/// # Example
/// ```text
/// let v = content_version(&path).unwrap();
/// match write_if_unchanged(&path, v.as_deref(), "new content")? {
///     WriteOutcome::Committed => { /* index update + commit_conditional */ }
///     WriteOutcome::Conflict => { /* retry or skip */ }
/// }
/// ```
pub fn write_if_unchanged(
    path: &Path,
    expected: Option<&str>,
    content: &str,
) -> Result<WriteOutcome, std::io::Error> {
    let current = content_version(path).map_err(std::io::Error::other)?;
    if current.as_deref() != expected {
        return Ok(WriteOutcome::Conflict);
    }
    crate::wiki::writer::atomic_write(path, content)?;
    Ok(WriteOutcome::Committed)
}

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

    /// Commit the transaction only if the snapshot is still current
    /// (OCC/CAS commit path, FR-032).
    ///
    /// Verifies the captured [`VersionSnapshot`] against the live
    /// filesystem: no drift → normal [`Self::commit`]; drift → [`Self::rollback`]
    /// of all tracked files and report the drifted paths.  This is the
    /// "atomic index+fs write" half of the contract — pair it with
    /// [`write_if_unchanged`] so the index update and the fs write succeed
    /// or fail together.
    ///
    /// # Parameters
    /// - `snapshot`: versions captured before the expensive reasoning.
    ///
    /// # Returns
    /// `Ok(CasCommitOutcome::Committed)` on clean commit,
    /// `Ok(CasCommitOutcome::RolledBack { drifted })` when drift forced a
    /// rollback.
    ///
    /// # Errors
    /// `anyhow::Error` if the snapshot cannot be verified or the
    /// commit/rollback filesystem operations fail.
    ///
    /// # Example
    /// ```text
    /// let snap = VersionSnapshot::capture(&paths).unwrap();
    /// // ... reason outside the lock ...
    /// match txn.commit_conditional(&snap).unwrap() {
    ///     CasCommitOutcome::Committed => { /* cycle done */ }
    ///     CasCommitOutcome::RolledBack { drifted } => { /* retry */ }
    /// }
    /// ```
    pub fn commit_conditional(&self, snapshot: &VersionSnapshot) -> Result<CasCommitOutcome> {
        let drifted = snapshot.verify()?;
        if drifted.is_empty() {
            self.commit()?;
            Ok(CasCommitOutcome::Committed)
        } else {
            self.rollback()?;
            Ok(CasCommitOutcome::RolledBack { drifted })
        }
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

    #[test]
    fn content_version_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.md");
        assert_eq!(content_version(&missing).unwrap(), None);
    }

    #[test]
    fn content_version_hashes_content_stably() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.md");
        fs::write(&path, "hello world").unwrap();

        let v1 = content_version(&path).unwrap().unwrap();
        let v2 = content_version(&path).unwrap().unwrap();
        assert_eq!(v1.len(), 64, "SHA-256 hex must be 64 chars");
        assert_eq!(v1, v2, "same content must hash identically");

        fs::write(&path, "hello world!").unwrap();
        let v3 = content_version(&path).unwrap().unwrap();
        assert_ne!(v1, v3, "changed content must hash differently");
    }

    #[test]
    fn snapshot_verify_no_drift() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.md");
        let b = dir.path().join("b.md");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();

        let snap = VersionSnapshot::capture(&[a.clone(), b.clone()]).unwrap();
        assert!(snap.verify().unwrap().is_empty());
    }

    #[test]
    fn snapshot_verify_detects_modification() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.md");
        fs::write(&a, "v1").unwrap();

        let snap = VersionSnapshot::capture(std::slice::from_ref(&a)).unwrap();
        fs::write(&a, "v2").unwrap();

        let drifted = snap.verify().unwrap();
        assert_eq!(drifted, vec![a]);
    }

    #[test]
    fn snapshot_verify_detects_creation_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let created = dir.path().join("created.md");
        let deleted = dir.path().join("deleted.md");
        fs::write(&deleted, "x").unwrap();

        let snap = VersionSnapshot::capture(&[created.clone(), deleted.clone()]).unwrap();

        // created since capture → drift; deleted since capture → drift.
        fs::write(&created, "new").unwrap();
        fs::remove_file(&deleted).unwrap();

        let mut drifted = snap.verify().unwrap();
        drifted.sort();
        let mut expected = vec![created, deleted];
        expected.sort();
        assert_eq!(drifted, expected);
    }

    #[test]
    fn write_if_unchanged_commits_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.md");
        fs::write(&path, "v1").unwrap();
        let version = content_version(&path).unwrap();

        let outcome = write_if_unchanged(&path, version.as_deref(), "v2").unwrap();
        assert_eq!(outcome, WriteOutcome::Committed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
        assert!(!path.with_extension("md.tmp").exists(), "no tmp leftover");
    }

    #[test]
    fn write_if_unchanged_conflicts_on_drift() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.md");
        fs::write(&path, "v1").unwrap();
        let version = content_version(&path).unwrap();

        // External modification between capture and write → drift.
        fs::write(&path, "v1-external").unwrap();

        let outcome = write_if_unchanged(&path, version.as_deref(), "v2").unwrap();
        assert_eq!(outcome, WriteOutcome::Conflict);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "v1-external",
            "conflict must not overwrite the drifted file"
        );
    }

    #[test]
    fn write_if_unchanged_conflicts_when_file_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.md");

        // Captured as missing (None); created before the conditional write.
        fs::write(&path, "v1-external").unwrap();

        let outcome = write_if_unchanged(&path, None, "v2").unwrap();
        assert_eq!(outcome, WriteOutcome::Conflict);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "v1-external",
            "conflict must not overwrite the created file"
        );
    }

    #[test]
    fn write_if_unchanged_commits_when_file_still_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.md");

        // Captured as missing (None) and still missing → safe to create.
        let outcome = write_if_unchanged(&path, None, "v2").unwrap();
        assert_eq!(outcome, WriteOutcome::Committed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
    }

    #[test]
    fn commit_conditional_commits_when_clean() {
        let logs = temp_logs_dir();
        let work = logs.join("work");
        fs::create_dir_all(&work).unwrap();

        let txn = TransactionScope::new("cas-commit-clean-test");
        txn.begin().unwrap();
        let path = work.join("kept.md");
        fs::write(&path, "kept").unwrap();
        txn.track_path(&path).unwrap();

        let snap = VersionSnapshot::capture(std::slice::from_ref(&path)).unwrap();
        let outcome = txn.commit_conditional(&snap).unwrap();

        assert_eq!(outcome, CasCommitOutcome::Committed);
        assert!(path.exists(), "clean commit must keep tracked files");
        fs::remove_dir_all(&logs).ok();
    }

    #[test]
    fn commit_conditional_rolls_back_on_drift() {
        let logs = temp_logs_dir();
        let work = logs.join("work");
        fs::create_dir_all(&work).unwrap();

        let txn = TransactionScope::new("cas-rollback-drift-test");
        txn.begin().unwrap();
        let path = work.join("drifted.md");
        fs::write(&path, "v1").unwrap();
        txn.track_path(&path).unwrap();

        let snap = VersionSnapshot::capture(std::slice::from_ref(&path)).unwrap();
        // External modification between capture and commit → drift.
        fs::write(&path, "v1-external").unwrap();

        let outcome = txn.commit_conditional(&snap).unwrap();

        match outcome {
            CasCommitOutcome::RolledBack { drifted } => {
                assert_eq!(drifted, vec![path.clone()]);
            }
            CasCommitOutcome::Committed => panic!("drift must force rollback"),
        }
        assert!(
            !path.exists(),
            "rollback must remove the drifted tracked file"
        );
        fs::remove_dir_all(&logs).ok();
    }
}
