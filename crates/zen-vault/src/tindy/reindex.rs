use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use tracing::{debug, info, warn};
use walkdir::WalkDir;
use zen_repo::{IndexNoteRequest, NotesRepo, SqliteClient};

use super::checksum;
use super::embeddings::compute_embeddings_for_text;
use crate::note;
use crate::search::Tier3Search;

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
    /// Files successfully embedded into `note_embeddings` (vec0).
    pub files_embedded: usize,
    /// Files where embedding generation was skipped (fallback exhausted or no client).
    pub embeddings_skipped: usize,
    /// Non-fatal error messages encountered while processing files.
    pub errors: Vec<String>,
}

// ── Reindexer ────────────────────────────────────────────────────────

/// Orchestrates a full or incremental reindex of the knowledge directory.
///
/// Walks `knowledge_dir` for `.md` files, computes SHA-256 checksums, and
/// indexes each changed file into both:
///   - FTS5 `notes_fts` table (always, when `db_client` is set)
///   - vec0 `note_embeddings` table (when `embed == true`, the default)
///
/// Embedding generation uses the 3-fallback chain in `tindy::embeddings`
/// (provider → local ONNX → hash) and is guaranteed not to fail.
#[derive(Default)]
pub struct Reindexer {
    known_checksums: HashMap<String, String>,
    db_client: Option<SqliteClient>,
    embed: bool,
}

impl Reindexer {
    pub fn new() -> Self {
        Reindexer {
            embed: true,
            ..Default::default()
        }
    }

    /// Create a reindexer with a database client for FTS5 + vec0 indexing.
    pub fn with_client(db_client: SqliteClient) -> Self {
        Reindexer {
            known_checksums: HashMap::new(),
            db_client: Some(db_client),
            embed: true,
        }
    }

    /// Disable vector embedding generation (FTS5-only mode).
    pub fn without_embeddings(mut self) -> Self {
        self.embed = false;
        self
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

            // Process: parse frontmatter, FTS5 index, embedding index, update checksum.
            match self.process_file(file_path, &current_checksum, &mut report).await {
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
            "Reindex complete: scanned={} updated={} unchanged={} embedded={} embed_skipped={} errors={}",
            report.files_scanned,
            report.files_updated,
            report.files_unchanged,
            report.files_embedded,
            report.embeddings_skipped,
            report.errors.len()
        );

        Ok(report)
    }

    /// Process a single file: parse frontmatter, index into FTS5, generate +
    /// store vector embedding (when enabled), update checksum.
    async fn process_file(
        &self,
        file_path: &Path,
        checksum: &str,
        report: &mut ReindexReport,
    ) -> Result<()> {
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

            if self.embed {
                match compute_embeddings_for_text(&body_content) {
                    Ok(embedding) if !embedding.is_empty() => {
                        match Tier3Search.insert_embedding(client, &note_id, &embedding).await {
                            Ok(()) => {
                                report.files_embedded += 1;
                                debug!(
                                    "Embedded: file={} id={} dim={}",
                                    file_display,
                                    note_id,
                                    embedding.len()
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "Embedding insert failed for {} (id={}): {} — FTS5 still indexed",
                                    file_display, note_id, e
                                );
                                report.embeddings_skipped += 1;
                            }
                        }
                    }
                    Ok(_) => {
                        report.embeddings_skipped += 1;
                    }
                    Err(e) => {
                        warn!(
                            "Embedding generation failed for {}: {} — this should not happen (3-fallback chain)",
                            file_display, e
                        );
                        report.embeddings_skipped += 1;
                    }
                }
            }
        } else {
            info!(
                "FTS5 index skipped (no db client): file={} checksum={:.8}…",
                file_display, checksum
            );
            if self.embed {
                report.embeddings_skipped += 1;
            }
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
