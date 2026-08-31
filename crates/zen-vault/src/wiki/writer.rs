use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Atomic tmp+rename write to an absolute path (shared primitive).
///
/// This is the single home of the tmp+rename atomic-write sequence.  Both
/// [`AtomicWikiWriter::write`] and the OCC/CAS conditional writer
/// (`crate::distill::transaction::write_if_unchanged`) call it — the
/// sequence is never duplicated.  Writes `content` to `{path}.tmp` first,
/// then renames over `path`: the target is either fully written or
/// untouched, never partially written (crash-safe).
///
/// # Parameters
/// - `path`: absolute target path; parent directories are created as needed.
/// - `content`: full file content to write.
///
/// # Returns
/// `Ok(())` on success.
///
/// # Errors
/// `std::io::Error` if the temp write or rename fails (permission denied,
/// disk full, unwritable parent directory).
///
/// # Example
/// ```text
/// use zen_vault::wiki::writer::atomic_write;
/// atomic_write(Path::new("/tmp/page.md"), "# Hello").unwrap();
/// ```
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .map_or("tmp".to_string(), |e| e.to_string_lossy().to_string())
    ));

    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, path)?;

    Ok(())
}

/// Atomic wiki page writer — writes to a temp file then renames.
///
/// Guarantees that either the full file is present or the old
/// version remains, avoiding partial writes on crash.
pub struct AtomicWikiWriter {
    base_dir: PathBuf,
}

impl AtomicWikiWriter {
    /// Create a writer targeting `base_dir`.
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
        }
    }

    /// Write `content` to `path` atomically.
    ///
    /// Writes to `{path}.tmp` first, then renames over the target.
    pub fn write(&self, path: &Path, content: &str) -> Result<()> {
        let full_path = self.base_dir.join(path);
        atomic_write(&full_path, content)
            .with_context(|| format!("failed to write temp file: {}", full_path.display()))
    }

    /// Conditionally write `content` to `path` only if the file's current
    /// content version still matches `expected_version` (OCC/CAS, FR-032).
    ///
    /// Compares the live SHA-256 content version of `base_dir.join(path)`
    /// against `expected_version`; on match the write is performed via the
    /// shared [`atomic_write`] primitive, on drift the write is skipped.
    /// This is the filesystem half of the "atomic index+fs write" contract —
    /// pair it with `TransactionScope::commit_conditional` so the index
    /// update and the fs write succeed or fail together.
    ///
    /// # Parameters
    /// - `path`: target path relative to `base_dir`.
    /// - `expected_version`: content version observed at capture time
    ///   (`None` = file did not exist then).
    /// - `content`: full file content to write on match.
    ///
    /// # Returns
    /// `Ok(true)` if the write was committed, `Ok(false)` if a conflict was
    /// detected and nothing was written.
    ///
    /// # Errors
    /// `std::io::Error` if the version read or the atomic write fails.
    ///
    /// # Example
    /// ```text
    /// let writer = AtomicWikiWriter::new(dir);
    /// let v = content_version(&dir.join("page.md")).unwrap();
    /// let committed = writer.write_conditional(Path::new("page.md"), v.as_deref(), "v2").unwrap();
    /// ```
    pub fn write_conditional(
        &self,
        path: &Path,
        expected_version: Option<&str>,
        content: &str,
    ) -> Result<bool, std::io::Error> {
        let full_path = self.base_dir.join(path);
        match crate::distill::transaction::write_if_unchanged(
            &full_path,
            expected_version,
            content,
        )? {
            crate::distill::transaction::WriteOutcome::Committed => Ok(true),
            crate::distill::transaction::WriteOutcome::Conflict => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_atomic_write_creates_file() {
        let dir = tempdir().unwrap();
        let writer = AtomicWikiWriter::new(dir.path());

        writer.write(Path::new("notes/test.md"), "# Hello").unwrap();

        let content = std::fs::read_to_string(dir.path().join("notes/test.md")).unwrap();
        assert_eq!(content, "# Hello");

        // No leftover temp file
        assert!(!dir.path().join("notes/test.md.tmp").exists());
    }

    #[test]
    fn test_atomic_write_overwrites() {
        let dir = tempdir().unwrap();
        let writer = AtomicWikiWriter::new(dir.path());

        writer.write(Path::new("page.md"), "v1").unwrap();
        writer.write(Path::new("page.md"), "v2").unwrap();

        let content = std::fs::read_to_string(dir.path().join("page.md")).unwrap();
        assert_eq!(content, "v2");
    }

    #[test]
    fn test_atomic_write_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let writer = AtomicWikiWriter::new(dir.path());

        writer.write(Path::new("a/b/c/deep.md"), "deep").unwrap();

        let content = std::fs::read_to_string(dir.path().join("a/b/c/deep.md")).unwrap();
        assert_eq!(content, "deep");
    }

    #[test]
    fn test_write_conditional_commits_when_unchanged() {
        let dir = tempdir().unwrap();
        let writer = AtomicWikiWriter::new(dir.path());
        let path = Path::new("page.md");
        let full = dir.path().join(path);

        writer.write(path, "v1").unwrap();
        let version = crate::distill::transaction::content_version(&full).unwrap();

        let committed = writer
            .write_conditional(path, version.as_deref(), "v2")
            .unwrap();
        assert!(committed, "unchanged version must commit");
        assert_eq!(std::fs::read_to_string(&full).unwrap(), "v2");
    }

    #[test]
    fn test_write_conditional_conflicts_on_drift() {
        let dir = tempdir().unwrap();
        let writer = AtomicWikiWriter::new(dir.path());
        let path = Path::new("page.md");
        let full = dir.path().join(path);

        writer.write(path, "v1").unwrap();
        let version = crate::distill::transaction::content_version(&full).unwrap();

        // External modification between capture and write → drift.
        std::fs::write(&full, "v1-external").unwrap();

        let committed = writer
            .write_conditional(path, version.as_deref(), "v2")
            .unwrap();
        assert!(!committed, "drifted version must conflict");
        assert_eq!(
            std::fs::read_to_string(&full).unwrap(),
            "v1-external",
            "conflict must not overwrite the drifted file"
        );
    }
}
