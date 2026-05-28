use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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

        // Ensure parent directory exists
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
        }

        let tmp_path = full_path.with_extension(format!(
            "{}.tmp",
            full_path
                .extension()
                .map_or("tmp".to_string(), |e| e.to_string_lossy().to_string())
        ));

        std::fs::write(&tmp_path, content)
            .with_context(|| format!("failed to write temp file: {}", tmp_path.display()))?;

        std::fs::rename(&tmp_path, &full_path)
            .with_context(|| format!("failed to rename temp to target: {}", full_path.display()))?;

        Ok(())
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
}
