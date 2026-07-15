//! Nightly M2-M4 knowledge indexer.
//!
//! Reads journal, wiki, and wisdom files from the filesystem and writes them into the
//! [`ZenMemvidStore`] for unified vector retrieval.  This is a batch indexer — it does NOT
//! extract or generate content, it only sinks existing Markdown into the memvid store.
//!
//! Indexing tiers:
//! - **M2 (Episodic)** — `memories/journal/*.md`, chunked by `## ` headers
//! - **M3 (Semantic)** — `wiki/notions/technology/*.md`, full content per file
//! - **M4 (Wisdom)** — `wiki/wisdom/{reflections,anti-patterns,models}/*.md`, full content per file

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use tracing::{debug, info, warn};

use crate::memvid::ZenMemvidStore;

// ─── Data types ────────────────────────────────────────────────────────

/// Report produced by a full indexing run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemvidIndexReport {
    /// Total number of `.md` files read (across all tiers).
    pub files_scanned: usize,
    /// Total number of text chunks written to the store.
    pub chunks_indexed: usize,
    /// Non-fatal errors (unreadable files, etc.).
    pub errors: Vec<String>,
}

// ─── MemvidIndexer ────────────────────────────────────────────────────

/// Batch indexer that scans source directories and feeds Markdown content
/// into a [`ZenMemvidStore`].
pub struct MemvidIndexer {
    workspace_root: PathBuf,
}

impl MemvidIndexer {
    /// Create a new indexer rooted at `workspace_root`.
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    /// Run all indexing tiers (M2 + M3 + M4) and return a combined report.
    pub fn index_all(&self, store: &mut ZenMemvidStore) -> Result<MemvidIndexReport> {
        let mut report = MemvidIndexReport::default();

        // M2 (Episodic)
        match self.index_m2_episodic(store) {
            Ok((files, chunks)) => {
                report.files_scanned += files;
                report.chunks_indexed += chunks;
            }
            Err(e) => {
                report.errors.push(format!("M2 episodic: {e}"));
            }
        }

        // M3 (Semantic)
        match self.index_m3_semantic(store) {
            Ok((files, chunks)) => {
                report.files_scanned += files;
                report.chunks_indexed += chunks;
            }
            Err(e) => {
                report.errors.push(format!("M3 semantic: {e}"));
            }
        }

        // M4 (Wisdom)
        match self.index_m4_wisdom(store) {
            Ok((files, chunks)) => {
                report.files_scanned += files;
                report.chunks_indexed += chunks;
            }
            Err(e) => {
                report.errors.push(format!("M4 wisdom: {e}"));
            }
        }

        info!(
            files = report.files_scanned,
            chunks = report.chunks_indexed,
            errors = report.errors.len(),
            "memvid indexing complete"
        );

        Ok(report)
    }

    // ─── M2 (Episodic) ──────────────────────────────────────────────

    /// Index journal files under `memories/journal/*.md`.
    ///
    /// Each file is chunked by `## ` headers (Facts, Reflections, Commitments, etc.)
    /// and each chunk is written to the store with a `"journal-{date}"` session id.
    pub fn index_m2_episodic(&self, store: &mut ZenMemvidStore) -> Result<(usize, usize)> {
        let journal_dir = self.workspace_root.join("memories").join("journal");

        let files = list_md_files(&journal_dir)?;
        if files.is_empty() {
            debug!("M2: no journal files in {}", journal_dir.display());
            return Ok((0, 0));
        }

        let mut files_scanned = 0usize;
        let mut chunks_indexed = 0usize;

        for path in &files {
            files_scanned += 1;
            let date = extract_date_from_filename(path);

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "M2: failed to read journal file, skipping"
                    );
                    continue;
                }
            };

            if content.trim().is_empty() {
                debug!(path = %path.display(), "M2: empty journal file, skipping");
                continue;
            }

            let chunks = chunk_by_headers(&content);
            let session_id = match &date {
                Some(d) => format!("journal-{d}"),
                None => "journal-unknown".to_string(),
            };

            for chunk in &chunks {
                if chunk.text.trim().is_empty() {
                    continue;
                }
                let label = format!("[{}] {}", chunk.header, chunk.text.trim());
                match store.persist_structured_turn(&session_id, "system", &label) {
                    Ok(_) => chunks_indexed += 1,
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            header = chunk.header,
                            error = %e,
                            "M2: failed to index chunk"
                        );
                    }
                }
            }
        }

        Ok((files_scanned, chunks_indexed))
    }

    // ─── M3 (Semantic) ──────────────────────────────────────────────

    /// Index wiki notion files under `wiki/notions/technology/*.md`.
    ///
    /// Each file is stored in full with a `"knowledge-base"` session id.
    pub fn index_m3_semantic(&self, store: &mut ZenMemvidStore) -> Result<(usize, usize)> {
        let entities_dir = self.workspace_root.join("wiki").join("notions");

        let files = list_md_files(&entities_dir)?;
        if files.is_empty() {
            debug!("M3: no notion files in {}", entities_dir.display());
            return Ok((0, 0));
        }

        let mut files_scanned = 0usize;
        let mut chunks_indexed = 0usize;

        for path in &files {
            files_scanned += 1;

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "M3: failed to read notion file, skipping"
                    );
                    continue;
                }
            };

            if content.trim().is_empty() {
                debug!(path = %path.display(), "M3: empty notion file, skipping");
                continue;
            }

            match store.persist_structured_turn("knowledge-base", "system", &content) {
                Ok(_) => chunks_indexed += 1,
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "M3: failed to index notion"
                    );
                }
            }
        }

        Ok((files_scanned, chunks_indexed))
    }

    // ─── M4 (Wisdom) ────────────────────────────────────────────────

    /// Index wisdom files across three subdirectories:
    /// - `wiki/wisdom/reflections/*.md`
    /// - `wiki/wisdom/anti-patterns/*.md`
    /// - `wiki/wisdom/models/*.md`
    ///
    /// Each file is stored in full with a `"knowledge-base"` session id.
    pub fn index_m4_wisdom(&self, store: &mut ZenMemvidStore) -> Result<(usize, usize)> {
        let wisdom_root = self.workspace_root.join("wiki").join("wisdom");

        let subdirs = ["reflections", "anti-patterns", "models"];

        let mut files_scanned = 0usize;
        let mut chunks_indexed = 0usize;

        for subdir in &subdirs {
            let dir = wisdom_root.join(subdir);

            let files = match list_md_files(&dir) {
                Ok(f) => f,
                Err(e) => {
                    warn!(
                        dir = %dir.display(),
                        error = %e,
                        "M4: failed to list wisdom subdir, skipping"
                    );
                    continue;
                }
            };

            for path in &files {
                files_scanned += 1;

                let content = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "M4: failed to read wisdom file, skipping"
                        );
                        continue;
                    }
                };

                if content.trim().is_empty() {
                    debug!(path = %path.display(), "M4: empty wisdom file, skipping");
                    continue;
                }

                match store.persist_structured_turn("knowledge-base", "system", &content) {
                    Ok(_) => chunks_indexed += 1,
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "M4: failed to index wisdom"
                        );
                    }
                }
            }
        }

        Ok((files_scanned, chunks_indexed))
    }
}

// ─── Internal helpers ──────────────────────────────────────────────────

/// A single chunk extracted from a Markdown file by header splitting.
struct HeaderChunk {
    /// The header text (without `## ` prefix).
    header: String,
    /// The body text belonging to this header.
    text: String,
}

fn chunk_by_headers(content: &str) -> Vec<HeaderChunk> {
    let body = strip_frontmatter(content);
    let mut chunks = Vec::new();
    let mut current_header = String::from("Header");
    let mut current_body = String::new();

    for line in body.lines() {
        if let Some(stripped) = line.strip_prefix("## ") {
            let trimmed = current_body.trim().to_string();
            if !trimmed.is_empty() {
                chunks.push(HeaderChunk {
                    header: current_header.clone(),
                    text: trimmed,
                });
            }
            current_header = stripped.trim().to_string();
            current_body.clear();
        } else if current_header == "Header" && (line.starts_with("# ") || line.trim().is_empty()) {
            // Skip H1 headings and blank lines in the preamble zone
            continue;
        } else {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        }
    }

    let trimmed = current_body.trim().to_string();
    if !trimmed.is_empty() {
        chunks.push(HeaderChunk {
            header: current_header,
            text: trimmed,
        });
    }

    chunks
}

fn strip_frontmatter(content: &str) -> &str {
    let trimmed_start = content.trim_start();
    if !trimmed_start.starts_with("---") {
        return content;
    }
    let after_first = &trimmed_start[3..];
    if let Some(end_idx) = after_first.find("\n---") {
        let after_fm = &after_first[end_idx + 4..];
        after_fm.strip_prefix('\n').unwrap_or(after_fm)
    } else {
        content
    }
}

/// List `.md` files in a directory, returning an empty vec if the dir doesn't exist.
fn list_md_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();

    files.sort();
    Ok(files)
}

/// Extract a YYYY-MM-DD date string from a filename like `2026-05-23.md`.
fn extract_date_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // Validate format loosely: must contain hyphens in expected positions
    if stem.len() == 10 && stem.as_bytes()[4] == b'-' && stem.as_bytes()[7] == b'-' {
        Some(stem.to_string())
    } else {
        None
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── chunk_by_headers ────────────────────────────────────────────

    #[test]
    fn chunk_by_headers_single_section() {
        let content = "## Facts\n- fact one\n- fact two\n";
        let chunks = chunk_by_headers(content);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].header, "Facts");
        assert_eq!(chunks[0].text, "- fact one\n- fact two");
    }

    #[test]
    fn chunk_by_headers_multiple_sections() {
        let content = "## Facts\n- a\n\n## Reflections\n- b\n\n## Commitments\n- c\n";
        let chunks = chunk_by_headers(content);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].header, "Facts");
        assert_eq!(chunks[0].text, "- a");
        assert_eq!(chunks[1].header, "Reflections");
        assert_eq!(chunks[1].text, "- b");
        assert_eq!(chunks[2].header, "Commitments");
        assert_eq!(chunks[2].text, "- c");
    }

    #[test]
    fn chunk_by_headers_skips_frontmatter() {
        let content = "---\nfrontmatter\n---\n# Session\n\n## Facts\n- x\n";
        let chunks = chunk_by_headers(content);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].header, "Facts");
        assert_eq!(chunks[0].text, "- x");
    }

    #[test]
    fn chunk_by_headers_empty_body_skipped() {
        let content = "## Facts\n\n## Reflections\n- something\n";
        let chunks = chunk_by_headers(content);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].header, "Reflections");
    }

    #[test]
    fn chunk_by_headers_preamble_only() {
        let content = "Some preamble text without any headers\n";
        let chunks = chunk_by_headers(content);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].header, "Header");
        assert_eq!(chunks[0].text, "Some preamble text without any headers");
    }

    #[test]
    fn chunk_by_headers_empty_content() {
        let chunks = chunk_by_headers("");
        assert!(chunks.is_empty());
    }

    // ── list_md_files ───────────────────────────────────────────────

    #[test]
    fn list_md_files_nonexistent_dir() {
        let dir = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let files = list_md_files(&dir).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn list_md_files_with_various_extensions() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.md"), "hello").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "skip").unwrap();
        std::fs::write(tmp.path().join("c.md"), "world").unwrap();

        let files = list_md_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 2);
        // Sorted
        assert!(files[0].to_string_lossy().contains("a.md"));
        assert!(files[1].to_string_lossy().contains("c.md"));
    }

    // ── extract_date_from_filename ──────────────────────────────────

    #[test]
    fn extract_date_valid() {
        let p = PathBuf::from("/some/path/2026-05-23.md");
        assert_eq!(extract_date_from_filename(&p), Some("2026-05-23".into()));
    }

    #[test]
    fn extract_date_invalid_format() {
        let p = PathBuf::from("/some/path/session-notes.md");
        assert_eq!(extract_date_from_filename(&p), None);
    }

    // ── MemvidIndexReport ───────────────────────────────────────────

    #[test]
    fn report_default_is_zero() {
        let r = MemvidIndexReport::default();
        assert_eq!(r.files_scanned, 0);
        assert_eq!(r.chunks_indexed, 0);
        assert!(r.errors.is_empty());
    }

    // ── MemvidIndexer — missing directories ─────────────────────────

    #[test]
    fn indexer_missing_dirs_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let indexer = MemvidIndexer::new(tmp.path().to_path_buf());

        // Create a minimal MemvidStore
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let report = indexer.index_all(&mut store).unwrap();
        assert_eq!(report.files_scanned, 0);
        assert_eq!(report.chunks_indexed, 0);
        assert!(report.errors.is_empty());
    }

    // ── MemvidIndexer — M2 episodic ─────────────────────────────────

    #[test]
    fn m2_indexes_journal_chunks() {
        let tmp = TempDir::new().unwrap();

        // Create journal directory with one file
        let journal_dir = tmp.path().join("memories").join("journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(
            journal_dir.join("2026-06-01.md"),
            "---\nfrontmatter\n---\n# Session\n\n## Facts\n- learned rust\n\n## Reflections\n- coding is fun\n",
        )
        .unwrap();

        let indexer = MemvidIndexer::new(tmp.path().to_path_buf());
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m2_episodic(&mut store).unwrap();
        assert_eq!(files, 1);
        assert_eq!(chunks, 2); // Facts + Reflections
    }

    // ── MemvidIndexer — M3 semantic ─────────────────────────────────

    #[test]
    fn m3_indexes_entity_files() {
        let tmp = TempDir::new().unwrap();

        let entities_dir = tmp.path().join("wiki").join("notions");
        std::fs::create_dir_all(&entities_dir).unwrap();
        std::fs::write(
            entities_dir.join("rust.md"),
            "# Rust\n\nRust is a systems language.\n",
        )
        .unwrap();
        std::fs::write(entities_dir.join("empty.md"), "").unwrap();

        let indexer = MemvidIndexer::new(tmp.path().to_path_buf());
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m3_semantic(&mut store).unwrap();
        assert_eq!(files, 2); // scanned both (even empty)
        assert_eq!(chunks, 1); // only non-empty indexed
    }

    // ── MemvidIndexer — M4 wisdom ───────────────────────────────────

    #[test]
    fn m4_indexes_wisdom_subdirs() {
        let tmp = TempDir::new().unwrap();

        // Create all three wisdom subdirs with files
        for subdir in &["reflections", "anti-patterns", "models"] {
            let dir = tmp.path().join("wiki").join("wisdom").join(subdir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("item.md"), format!("# {subdir}\n\nSome wisdom.\n")).unwrap();
        }

        let indexer = MemvidIndexer::new(tmp.path().to_path_buf());
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m4_wisdom(&mut store).unwrap();
        assert_eq!(files, 3);
        assert_eq!(chunks, 3);
    }

    // ── MemvidIndexer — empty files ─────────────────────────────────

    #[test]
    fn indexer_skips_empty_files() {
        let tmp = TempDir::new().unwrap();

        let journal_dir = tmp.path().join("memories").join("journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(journal_dir.join("2026-06-01.md"), "").unwrap();

        let indexer = MemvidIndexer::new(tmp.path().to_path_buf());
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m2_episodic(&mut store).unwrap();
        assert_eq!(files, 1); // file was scanned
        assert_eq!(chunks, 0); // but no chunks written
    }

    // ── MemvidIndexer — full index_all ──────────────────────────────

    #[test]
    fn index_all_collects_across_tiers() {
        let tmp = TempDir::new().unwrap();

        // M2: one journal file
        let journal_dir = tmp.path().join("memories").join("journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(
            journal_dir.join("2026-06-01.md"),
            "## Facts\n- fact\n\n## Reflections\n- ref\n",
        )
        .unwrap();

        // M3: one notion file
        let entities_dir = tmp.path().join("wiki").join("notions");
        std::fs::create_dir_all(&entities_dir).unwrap();
        std::fs::write(entities_dir.join("topic.md"), "# Topic\n\nContent.\n").unwrap();

        // M4: one wisdom file
        let wisdom_dir = tmp.path().join("wiki").join("wisdom").join("reflections");
        std::fs::create_dir_all(&wisdom_dir).unwrap();
        std::fs::write(wisdom_dir.join("lesson.md"), "# Lesson\n\nWisdom.\n").unwrap();

        let indexer = MemvidIndexer::new(tmp.path().to_path_buf());
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let report = indexer.index_all(&mut store).unwrap();
        assert_eq!(report.files_scanned, 3); // 1 + 1 + 1
        assert_eq!(report.chunks_indexed, 4); // 2 + 1 + 1
        assert!(report.errors.is_empty());
    }
}
