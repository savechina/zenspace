use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use tracing::{info, warn};
use walkdir::WalkDir;

use super::checksum;
use crate::note;

// ── Report ───────────────────────────────────────────────────────────

/// Summary of a reindex run.
#[derive(Debug, Default)]
pub struct ReindexReport {
    /// Total `.md` files discovered under `knowledge_dir`.
    pub files_scanned: usize,
    /// Files whose checksum changed or that were not yet indexed.
    pub files_updated: usize,
    /// Files whose checksum matched the stored value.
    pub files_unchanged: usize,
    /// Non-fatal error messages encountered while processing files.
    pub errors: Vec<String>,
}

// ── Reindexer ────────────────────────────────────────────────────────

/// Orchestrates a full or incremental reindex of the knowledge directory.
///
/// The reindexer walks `knowledge_dir` for all `.md` files, computes
/// SHA-256 checksums, and decides whether each note needs to be
/// re-indexed.  FTS5 and SQLite index updates are stubbed with
/// `tracing::info` as per the spec.
#[derive(Default)]
pub struct Reindexer {
    /// Map of file path → stored checksum (populated by callers that
    /// maintain a `notes_metadata` table).
    known_checksums: HashMap<String, String>,
}

impl Reindexer {
    pub fn new() -> Self {
        Reindexer {
            known_checksums: HashMap::new(),
        }
    }

    /// Register a known checksum for a file.
    ///
    /// Call this once per file before invoking [`Reindexer::reindex`] if
    /// you maintain a `notes_metadata` table from disk / SQLite.
    pub fn set_known_checksum(&mut self, file_path: String, checksum: String) {
        self.known_checksums.insert(file_path, checksum);
    }

    /// Walk `knowledge_dir` for `.md` files, decide which need reindexing,
    /// and return a [`ReindexReport`].
    pub fn reindex(&self, knowledge_dir: &Path) -> Result<ReindexReport> {
        let mut report = ReindexReport::default();

        if !knowledge_dir.is_dir() {
            info!(
                "Knowledge directory does not exist: {}",
                knowledge_dir.display()
            );
            return Ok(report);
        }

        // Collect all .md files (recursive).
        let md_files: Vec<_> = WalkDir::new(knowledge_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .map(|e| e.into_path())
            .collect();

        report.files_scanned = md_files.len();
        info!(
            "Reindex: found {} markdown files in {}",
            report.files_scanned,
            knowledge_dir.display()
        );

        for file_path in &md_files {
            // ── Per-file processing ─────────────────────────────────
            let file_display = file_path.display().to_string();

            // Compute current checksum.
            let current_checksum = match checksum::compute_file_checksum(file_path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to checksum {}: {}", file_display, e);
                    report
                        .errors
                        .push(format!("checksum {}: {}", file_display, e));
                    continue;
                },
            };

            // Decide: unchanged, new, or changed.
            let stored = self.known_checksums.get(&file_display);
            let needs_update = match stored {
                None => true, // new file
                Some(sc) => sc != &current_checksum,
            };

            if !needs_update {
                report.files_unchanged += 1;
                continue;
            }

            // Process: parse frontmatter, update index (stub), update checksum.
            match self.process_file(file_path, &current_checksum) {
                Ok(()) => {
                    info!("Reindexed: {}", file_display);
                    report.files_updated += 1;
                },
                Err(e) => {
                    warn!("Reindex failed for {}: {}", file_display, e);
                    report
                        .errors
                        .push(format!("reindex {}: {}", file_display, e));
                    // Transactional: we just skip this file (rollback is
                    // implicit because we haven't written anything yet).
                },
            }
        }

        info!(
            "Reindex complete: scanned={} updated={} unchanged={} errors={}",
            report.files_scanned,
            report.files_updated,
            report.files_unchanged,
            report.errors.len()
        );

        Ok(report)
    }

    /// Process a single file: parse frontmatter, update index stub, log.
    fn process_file(&self, file_path: &Path, checksum: &str) -> Result<()> {
        // Read content (the "transaction" boundary is this function).
        let content = fs::read_to_string(file_path)?;

        // Parse frontmatter stub (may fail for files without YAML headers).
        match note::parse_frontmatter(&content) {
            Ok(parsed_note) => {
                info!(
                    "Parsed note: id={} tags={} domain={:?}",
                    parsed_note.id,
                    parsed_note.tags.len(),
                    parsed_note.domain
                );
            },
            Err(e) => {
                warn!(
                    "Frontmatter parse failed for {}: {} — indexing as raw",
                    file_path.display(),
                    e
                );
            },
        }

        // FTS5 index update stub.
        info!(
            "FTS5 update stub: indexing file={} checksum={:.8}…",
            file_path.display(),
            checksum
        );

        // notes_metadata checksum update stub.
        info!(
            "Metadata update stub: storing checksum={:.8}… for file={}",
            checksum,
            file_path.display()
        );

        Ok(())
    }
}

// ── Standalone convenience function (T082 public API) ────────────────

/// Reindex all markdown files under `knowledge_dir`, performing
/// checksum-based change detection.
///
/// This is a convenience wrapper around [`Reindexer`] that assumes no
/// prior checksums are known (first run or full reindex).
pub fn reindex_all(knowledge_dir: &Path) -> Result<ReindexReport> {
    let reindexer = Reindexer::new();
    reindexer.reindex(knowledge_dir)
}
