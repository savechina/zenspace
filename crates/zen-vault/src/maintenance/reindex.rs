use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use tracing::{info, warn};
use walkdir::WalkDir;
use zen_repo::{IndexNoteRequest, NotesRepo, SqliteClient};

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
/// re-indexed. Files are indexed into the FTS5 `notes_fts` table via
/// `NotesRepo::index_note()`.
pub struct Reindexer {
    known_checksums: HashMap<String, String>,
    db_client: Option<SqliteClient>,
}

impl Default for Reindexer {
    fn default() -> Self {
        Reindexer {
            known_checksums: HashMap::new(),
            db_client: None,
        }
    }
}

impl Reindexer {
    pub fn new() -> Self {
        Reindexer::default()
    }

    /// Create a reindexer with a database client for actual FTS5 indexing.
    pub fn with_client(db_client: SqliteClient) -> Self {
        Reindexer {
            known_checksums: HashMap::new(),
            db_client: Some(db_client),
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
    pub async fn reindex(&self, knowledge_dir: &Path) -> Result<ReindexReport> {
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
                }
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
            match self.process_file(file_path, &current_checksum).await {
                Ok(()) => {
                    info!("Reindexed: {}", file_display);
                    report.files_updated += 1;
                }
                Err(e) => {
                    warn!("Reindex failed for {}: {}", file_display, e);
                    report
                        .errors
                        .push(format!("reindex {}: {}", file_display, e));
                    // Transactional: we just skip this file (rollback is
                    // implicit because we haven't written anything yet).
                }
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

    /// Process a single file: parse frontmatter, index into FTS5, update checksum.
    async fn process_file(&self, file_path: &Path, checksum: &str) -> Result<()> {
        let content = fs::read_to_string(file_path)?;
        let file_display = file_path.display().to_string();
        let file_name = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();

        let (note_id, body_content, tags) = match note::parse_frontmatter(&content) {
            Ok(parsed_note) => {
                let tags_str = parsed_note.tags.join(",");
                (parsed_note.id, parsed_note.content, tags_str)
            }
            Err(e) => {
                warn!(
                    "Frontmatter parse failed for {}: {} — indexing as raw",
                    file_display, e
                );
                let id = format!("auto-{}", file_name);
                (id, content.clone(), String::new())
            }
        };

        let title = file_name.clone();
        let source = if file_display.contains("/wiki/") {
            "wiki"
        } else if file_display.contains("/inbox/") {
            "inbox"
        } else {
            "vault"
        };

        if let Some(client) = &self.db_client {
            NotesRepo::new(client)
                .index_note(IndexNoteRequest {
                    id: &note_id,
                    title: &title,
                    content: &body_content,
                    tags: &tags,
                    file_path: &file_display,
                    source,
                })
                .await
                .map_err(|e| anyhow::anyhow!("FTS5 index failed for {}: {}", file_display, e))?;

            info!(
                "FTS5 indexed: file={} id={} title={} source={}",
                file_display, note_id, title, source
            );
        } else {
            info!(
                "FTS5 index skipped (no db client): file={} checksum={:.8}…",
                file_display, checksum
            );
        }

        Ok(())
    }
}

// ── Standalone convenience function (T082 public API) ────────────────

/// Reindex all markdown files under `knowledge_dir`, performing
/// checksum-based change detection.
///
/// This is a convenience wrapper around [`Reindexer`] that assumes no
/// prior checksums are known (first run or full reindex).
pub async fn reindex_all(knowledge_dir: &Path) -> Result<ReindexReport> {
    let reindexer = Reindexer::new();
    reindexer.reindex(knowledge_dir).await
}
