use std::collections::HashMap;
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

    /// Promote FR-033 host staging into the notes inbox (T043).
    ///
    /// Walks `inbox/_incoming/{host_hash}/`, moving every pending markdown/
    /// text file to `inbox/{host_hash}_{filename}` (hash prefix disambiguates
    /// same-named files from different hosts and from raw/ ingest copies).
    /// Promoted files are recorded under `_incoming/{host_hash}/promoted/` so
    /// a file is promoted exactly once even though the sweep re-stages nothing
    /// twice — the staging tree is both the sweep's seen-set and the
    /// promotion ledger.
    ///
    /// # Parameters
    /// - `inbox` — the workspace notes inbox (`vault/inbox/`).
    ///
    /// # Returns
    /// Map of `host_hash` → number of files promoted (for per-source audit).
    ///
    /// # Errors
    /// Bubbles up directory reads and file moves; a failed file is skipped
    /// warn-and-continue so one bad file never blocks the sweep.
    pub fn promote_incoming(&self, inbox: &Path) -> Result<HashMap<String, usize>> {
        let mut promoted: HashMap<String, usize> = HashMap::new();
        let incoming_root = inbox.join("_incoming");
        if !incoming_root.is_dir() {
            return Ok(promoted);
        }

        for hash_dir in fs::read_dir(&incoming_root)? {
            let hash_dir = hash_dir?;
            let hash_path = hash_dir.path();
            if !hash_path.is_dir() {
                continue;
            }
            let Some(host_hash) = hash_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let promoted_dir = hash_path.join("promoted");
            for entry in fs::read_dir(&hash_path)? {
                let entry = entry?;
                let staged = entry.path();
                if !staged.is_file() {
                    continue;
                }
                let ext = staged.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !(ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("txt")) {
                    continue;
                }
                let Some(file_name) = staged.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let dest_name = format!("{host_hash}_{file_name}");
                let dest = inbox.join(&dest_name);
                if dest.exists() {
                    // Previous promotion still awaiting distill — retry later.
                    warn!(file = %dest_name, "incoming promotion target exists, deferring");
                    continue;
                }
                fs::create_dir_all(&promoted_dir).ok();
                // Ledger record keeps the ORIGINAL staged name — the sweep's
                // seen-set checks `_incoming/{hash}/promoted/{filename}`.
                fs::copy(&staged, promoted_dir.join(file_name))
                    .with_context(|| format!("ledger record: {}", promoted_dir.display()))?;
                fs::rename(&staged, &dest).with_context(|| {
                    format!("promote {} -> {}", staged.display(), dest.display())
                })?;
                *promoted.entry(host_hash.to_string()).or_insert(0) += 1;
            }
        }

        let total: usize = promoted.values().sum();
        if total > 0 {
            info!(total, "host staging promoted into inbox/");
        }
        Ok(promoted)
    }
}

impl Default for SourceIngester {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promote_incoming_moves_staging_into_inbox_once() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("ab12cd34");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("report.md"), "# Report").unwrap();
        fs::write(staging.join("data.txt"), "raw text").unwrap();
        fs::write(staging.join("skip.bin"), "\x00\x01").unwrap();

        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert_eq!(promoted.get("ab12cd34"), Some(&2));

        assert!(inbox.join("ab12cd34_report.md").is_file());
        assert!(inbox.join("ab12cd34_data.txt").is_file());
        assert!(!inbox.join("ab12cd34_skip.bin").exists());
        assert!(!staging.join("report.md").exists());
        assert_eq!(
            fs::read_to_string(staging.join("promoted").join("report.md")).unwrap(),
            "# Report"
        );

        // Second run: nothing left pending — no double promotion.
        let again = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn promote_incoming_defers_when_inbox_target_exists() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("ef987654");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("note.md"), "v2").unwrap();
        fs::write(inbox.join("ef987654_note.md"), "v1 still pending distill").unwrap();

        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(promoted.is_empty());
        assert!(staging.join("note.md").is_file());
        assert_eq!(
            fs::read_to_string(inbox.join("ef987654_note.md")).unwrap(),
            "v1 still pending distill"
        );
    }

    #[test]
    fn promote_incoming_noop_without_staging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(promoted.is_empty());
    }
}
