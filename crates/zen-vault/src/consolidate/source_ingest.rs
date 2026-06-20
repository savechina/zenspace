use std::fs;
use std::path::Path;

use anyhow::Result;
use tracing::info;

/// Stub source ingester — counts files in a raw directory.
pub struct SourceIngester;

impl SourceIngester {
    pub fn new() -> Self {
        Self
    }

    /// STUB: count files in `raw_dir`, log count, return it.
    pub fn ingest(&self, raw_dir: &Path) -> Result<usize> {
        let mut count = 0;
        if raw_dir.is_dir() {
            count = fs::read_dir(raw_dir)?.filter_map(|e| e.ok()).count();
        }
        info!("Source ingestion stub: {count} files in raw/");
        Ok(count)
    }
}

impl Default for SourceIngester {
    fn default() -> Self {
        Self::new()
    }
}
