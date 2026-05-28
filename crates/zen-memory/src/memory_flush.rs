use std::path::Path;
use tracing::info;

/// Handles pre-compaction memory flush — writing pending facts to memory.md.
pub struct MemoryFlush;

impl Default for MemoryFlush {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryFlush {
    pub fn new() -> Self {
        Self
    }

    /// Stub: flush pending facts to the memory markdown file at `memory_md_path`.
    /// Currently logs and returns `Ok(())` without writing.
    pub fn flush_pending(&self, memory_md_path: &Path) -> Result<(), anyhow::Error> {
        info!(
            "Memory flush stub: would write pending facts to {}",
            memory_md_path.display()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_is_stub_and_succeeds() {
        let flush = MemoryFlush::new();
        let tmp = tempfile::tempdir().unwrap();
        let test_file = tmp.path().join("test.md");
        let result = flush.flush_pending(&test_file);
        assert!(result.is_ok());
    }
}
