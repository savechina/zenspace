use std::path::Path;

use anyhow::Result;
use tracing::info;

/// Stub chat importer — returns 0 imported messages.
pub struct ChatImporter;

impl ChatImporter {
    pub fn new() -> Self {
        Self
    }

    /// STUB: log the path, return 0.
    pub fn import(&self, chat_export_path: &Path) -> Result<usize> {
        info!("Chat import stub: {}", chat_export_path.display());
        Ok(0)
    }
}

impl Default for ChatImporter {
    fn default() -> Self {
        Self::new()
    }
}
