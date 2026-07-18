use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::{info, warn};

/// Ingests files from a raw directory into the notes workspace.
pub struct SourceIngester;

impl SourceIngester {
    pub fn new() -> Self {
        Self
    }

    /// Ingest files from `raw_dir`, copying markdown files into the notes
    /// inbox under `workspace/inbox/`. Non-markdown files are skipped with a
    /// warning.
    pub fn ingest(&self, raw_dir: &Path) -> Result<usize> {
        let mut count = 0;

        if !raw_dir.is_dir() {
            return Ok(0);
        }

        let inbox = raw_dir
            .parent()
            .map(|vault| vault.join("inbox"))
            .unwrap_or_else(|| raw_dir.to_path_buf());
        fs::create_dir_all(&inbox)
            .with_context(|| format!("create inbox dir: {}", inbox.display()))?;

        for entry in fs::read_dir(raw_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("txt") {
                let dest = inbox.join(
                    path.file_name()
                        .unwrap_or_else(|| std::ffi::OsStr::new("unknown")),
                );
                fs::copy(&path, &dest)
                    .with_context(|| format!("copy {} -> {}", path.display(), dest.display()))?;
                count += 1;
            } else {
                warn!(path = %path.display(), ext, "skipping non-ingestible file");
            }
        }

        info!(count, "files ingested from raw/ into inbox/");
        Ok(count)
    }
}

impl Default for SourceIngester {
    fn default() -> Self {
        Self::new()
    }
}
