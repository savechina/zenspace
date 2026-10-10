//! Nightly M2-M4 knowledge indexer.
//!
//! Reads journal, wiki, and wisdom files from the filesystem and writes them into the
//! [`ZenMemvidStore`] for unified vector retrieval.  This is a batch indexer — it does NOT
//! extract or generate content, it only sinks existing Markdown into the memvid store.
//!
//! Roots follow Path Spec v2 (T195/W1): the indexer takes the two real
//! locations explicitly — `memory_root` = `ZenPaths::memory()` (e.g.
//! `~/.zen/memories`) and `wiki_root` = `ZenPaths::wiki()` (e.g.
//! `~/.zen/vault/wiki`). No single workspace root can satisfy both joins.
//!
//! Indexing tiers:
//! - **M2 (Episodic)** — `memory_root/journal/*.md`, chunked by `## ` headers
//! - **M3 (Semantic)** — `wiki_root/notions/**/*.md` (recursive — production
//!   pages live under `notions/technology/`), full content per file
//! - **M4 (Wisdom)** — `wiki_root/wisdom/{reflections,anti-patterns,models,preferences}/*.md`,
//!   full content per file

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use chrono::NaiveDate;
use tracing::{debug, info, warn};

use crate::memvid::ZenMemvidStore;

// ─── Data types ────────────────────────────────────────────────────────

/// Frame tag applied to time-anchored chunks (T071, FR-021 Pi point 3).
/// Tag-only metadata: no new index, no schema change.
pub const TEMPORAL_ENTITY_TAG: &str = "temporal_entity";

/// `extra_metadata` key carrying the indexed file's path RELATIVE to the
/// root that owns it (Phase 31 T208, design F1-D2). Static provenance —
/// written once at put time (frames are append-only), read back on search
/// hits via `SearchHitMetadata.extra_metadata`. Consumers join it to the
/// reward sidecars / file mtime for retention-strength anchoring.
pub const PROVENANCE_SOURCE_PATH_KEY: &str = "source_path";

/// `extra_metadata` key carrying the M-tier the file was indexed from
/// (`"m2"` | `"m3"` | `"m4"`, Phase 31 T208).
pub const PROVENANCE_TIER_KEY: &str = "tier";

/// Recency half-life for time-anchored content (days), reusing the FR-025
/// 30-day confidence half-life so all decay in the system shares one constant.
pub const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;

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
    memory_root: PathBuf,
    wiki_root: PathBuf,
    provenance_metadata: bool,
}

impl MemvidIndexer {
    /// Create a new indexer from the two Path Spec v2 roots:
    /// `memory_root` = `ZenPaths::memory()` (journal + checksum sidecar live
    /// under it), `wiki_root` = `ZenPaths::wiki()` (notions + wisdom live
    /// under it). Provenance metadata ships OFF — see
    /// [`Self::with_provenance_metadata`].
    pub fn new(memory_root: PathBuf, wiki_root: PathBuf) -> Self {
        Self {
            memory_root,
            wiki_root,
            provenance_metadata: false,
        }
    }

    /// Additive builder (Phase 31 T208): when enabled, M2/M3/M4 puts carry
    /// `extra_metadata {source_path, tier}` (memvid-core `PutOptions` write
    /// channel, echoed on search hits). The caller wires this from
    /// `[agentic.memory_strength] enabled` — the indexer itself reads no
    /// config. Disabled (the default) produces byte-identical puts to the
    /// pre-Phase-31 indexer.
    pub fn with_provenance_metadata(mut self, enabled: bool) -> Self {
        self.provenance_metadata = enabled;
        self
    }

    /// Whether provenance metadata writes are enabled.
    pub fn provenance_metadata_enabled(&self) -> bool {
        self.provenance_metadata
    }

    /// Build the `extra_metadata` map for one source file: `source_path`
    /// relative to the root that owns the file + the `tier` it was scanned
    /// from. Returns an EMPTY map when provenance is disabled or the file
    /// belongs to none of the indexed subtrees (`memory_root/journal` → m2,
    /// `wiki_root/notions` → m3, `wiki_root/wisdom` → m4) — an empty map
    /// keeps the put byte-identical to the metadata-free path.
    fn provenance_map(&self, path: &Path) -> BTreeMap<String, String> {
        fn entry(tier: &str, rel: &Path) -> BTreeMap<String, String> {
            let mut map = BTreeMap::new();
            map.insert(
                PROVENANCE_SOURCE_PATH_KEY.to_string(),
                rel.to_string_lossy().into_owned(),
            );
            map.insert(PROVENANCE_TIER_KEY.to_string(), tier.to_string());
            map
        }

        if !self.provenance_metadata {
            return BTreeMap::new();
        }
        // wiki_root subtrees are probed FIRST so a wiki_root nested under
        // memory_root could never mis-tag m3/m4 content as m2.
        if path.starts_with(self.wiki_root.join("notions"))
            && let Ok(rel) = path.strip_prefix(&self.wiki_root)
        {
            return entry("m3", rel);
        }
        if path.starts_with(self.wiki_root.join("wisdom"))
            && let Ok(rel) = path.strip_prefix(&self.wiki_root)
        {
            return entry("m4", rel);
        }
        if path.starts_with(self.memory_root.join("journal"))
            && let Ok(rel) = path.strip_prefix(&self.memory_root)
        {
            return entry("m2", rel);
        }
        BTreeMap::new()
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

    /// Incremental indexing: only re-index files changed since the last run.
    ///
    /// Uses a checksum sidecar file (`memories/.index-checksums.json`) to track
    /// file modification times. Files not in the checksum file or with changed
    /// mtime are re-indexed. Falls back to `index_all()` if the checksum file
    /// is missing or unreadable.
    pub fn index_incremental(&self, store: &mut ZenMemvidStore) -> Result<MemvidIndexReport> {
        let checksum_path = self.memory_root.join(".index-checksums.json");
        let previous = load_checksums(&checksum_path);

        if previous.is_empty() {
            info!("memvid: no previous checksums, falling back to full index_all");
            let report = self.index_all(store)?;
            save_checksums(
                &checksum_path,
                &collect_current_checksums(&self.memory_root, &self.wiki_root)?,
            );
            return Ok(report);
        }

        let current = collect_current_checksums(&self.memory_root, &self.wiki_root)?;
        let changed: Vec<PathBuf> = current
            .iter()
            .filter(|(path, mtime)| match previous.get(*path) {
                Some(prev_mtime) => **mtime != *prev_mtime,
                None => true,
            })
            .map(|(path, _)| path.clone())
            .collect();

        if changed.is_empty() {
            info!("memvid: incremental index — no changed files, skipping");
            return Ok(MemvidIndexReport::default());
        }

        info!(changed_files = changed.len(), "memvid: incremental index");
        let mut report = MemvidIndexReport::default();

        for path in &changed {
            report.files_scanned += 1;
            match self.index_single_file(store, path) {
                Ok(chunks) => report.chunks_indexed += chunks,
                Err(e) => report.errors.push(format!("{}: {e}", path.display())),
            }
        }

        save_checksums(&checksum_path, &current);
        info!(
            files = report.files_scanned,
            chunks = report.chunks_indexed,
            errors = report.errors.len(),
            "memvid incremental indexing complete"
        );
        Ok(report)
    }

    fn index_single_file(&self, store: &mut ZenMemvidStore, path: &Path) -> Result<usize> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        if content.trim().is_empty() {
            return Ok(0);
        }

        // Strip against the root that owns the file: a journal file reduces to
        // `journal/YYYY-MM-DD.md` → session id `journal-YYYY-MM-DD`, the same
        // id scheme the M2 batch path uses (so the temporal tag can fire).
        let relative = path
            .strip_prefix(&self.memory_root)
            .or_else(|_| path.strip_prefix(&self.wiki_root))
            .unwrap_or(path);
        let session_id = relative
            .to_string_lossy()
            .replace('/', "-")
            .replace(".md", "");
        let extra_tag = extract_anchor_date(&session_id).map(|_| TEMPORAL_ENTITY_TAG);
        let provenance = self.provenance_map(path);

        let chunks = chunk_by_headers(&content);
        let mut indexed = 0usize;
        for chunk in &chunks {
            if chunk.text.trim().is_empty() {
                continue;
            }
            let label = format!("[{}] {}", chunk.header, chunk.text.trim());
            if store
                .persist_structured_turn_meta(&session_id, "system", &label, extra_tag, &provenance)
                .is_ok()
            {
                indexed += 1;
            }
        }
        Ok(indexed)
    }

    // ─── M2 (Episodic) ──────────────────────────────────────────────

    /// Index journal files under `memory_root/journal/*.md`.
    ///
    /// Each file is chunked by `## ` headers (Facts, Reflections, Commitments, etc.)
    /// and each chunk is written to the store with a `"journal-{date}"` session id.
    ///
    /// T071: files are written oldest→newest (recency-weighted reorder — in the
    /// append-order store, newer chunks land at higher frame ids) and every
    /// time-anchored chunk is tagged `temporal_entity`.
    pub fn index_m2_episodic(&self, store: &mut ZenMemvidStore) -> Result<(usize, usize)> {
        let journal_dir = self.memory_root.join("journal");

        let files = reorder_by_recency(list_md_files(&journal_dir)?);
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
            let temporal = extract_anchor_date(&session_id).is_some();
            let provenance = self.provenance_map(path);

            for chunk in &chunks {
                if chunk.text.trim().is_empty() {
                    continue;
                }
                let label = format!("[{}] {}", chunk.header, chunk.text.trim());
                let extra_tag = temporal.then_some(TEMPORAL_ENTITY_TAG);
                match store.persist_structured_turn_meta(
                    &session_id,
                    "system",
                    &label,
                    extra_tag,
                    &provenance,
                ) {
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

    /// Index wiki notion files under `wiki_root/notions/**/*.md`.
    ///
    /// Recursive walk: production compile output lands in category
    /// subdirectories (`notions/technology/…`) while the scheduler's
    /// wiki-compiler writes flat `notions/*.md` — the walk covers both.
    /// Each file is stored in full with a `"knowledge-base"` session id.
    pub fn index_m3_semantic(&self, store: &mut ZenMemvidStore) -> Result<(usize, usize)> {
        let entities_dir = self.wiki_root.join("notions");

        let files = list_md_files_recursive(&entities_dir)?;
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

            let provenance = self.provenance_map(path);
            match store.persist_structured_turn_meta(
                "knowledge-base",
                "system",
                &content,
                None,
                &provenance,
            ) {
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

    /// Index wisdom files across four subdirectories:
    /// - `wiki_root/wisdom/reflections/*.md`
    /// - `wiki_root/wisdom/anti-patterns/*.md`
    /// - `wiki_root/wisdom/models/*.md`
    /// - `wiki_root/wisdom/preferences/*.md` (FR-021 Pi point 2, M4)
    ///
    /// Each file is stored in full with a `"knowledge-base"` session id.
    pub fn index_m4_wisdom(&self, store: &mut ZenMemvidStore) -> Result<(usize, usize)> {
        let wisdom_root = self.wiki_root.join("wisdom");

        let subdirs = ["reflections", "anti-patterns", "models", "preferences"];

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

                let provenance = self.provenance_map(path);
                match store.persist_structured_turn_meta(
                    "knowledge-base",
                    "system",
                    &content,
                    None,
                    &provenance,
                ) {
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

/// Parse the time anchor from a `journal-YYYY-MM-DD` session id (T071).
/// Returns `None` for non-date-scoped ids (e.g. `knowledge-base`).
pub fn extract_anchor_date(session_id: &str) -> Option<NaiveDate> {
    let date = session_id.strip_prefix("journal-")?;
    if date.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

/// Recency weight of a time-anchored chunk: exponential decay with the
/// FR-025 30-day confidence half-life (`0.5^(age_days/30)`). Undated
/// (evergreen) content weighs a neutral 1.0.
pub fn recency_weight(anchor: Option<NaiveDate>, now: NaiveDate) -> f64 {
    match anchor {
        Some(d) => {
            let age = (now - d).num_days().max(0) as f64;
            0.5_f64.powf(age / RECENCY_HALF_LIFE_DAYS)
        }
        None => 1.0,
    }
}

/// Recency-weighted reorder (T071): oldest date-anchored files first so the
/// newest content lands at the highest frame ids in the append-order store;
/// undated files keep their relative order at the end. Weight-only — no new
/// index, no schema change.
pub fn reorder_by_recency(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let keyed: Vec<(Option<NaiveDate>, usize, PathBuf)> = files
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            (
                extract_date_from_filename(&p)
                    .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()),
                i,
                p,
            )
        })
        .collect();
    let mut keyed = keyed;
    keyed.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(&b.1),
    });
    keyed.into_iter().map(|(_, _, p)| p).collect()
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

// ─── Incremental indexing helpers ─────────────────────────────────────

type ChecksumMap = std::collections::HashMap<PathBuf, u64>;

fn load_checksums(path: &Path) -> ChecksumMap {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => ChecksumMap::default(),
    }
}

fn save_checksums(path: &Path, checksums: &ChecksumMap) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(checksums) {
        let _ = std::fs::write(path, json);
    }
}

fn collect_current_checksums(memory_root: &Path, wiki_root: &Path) -> Result<ChecksumMap> {
    fn record_mtime(map: &mut ChecksumMap, path: &Path) {
        if let Ok(meta) = std::fs::metadata(path)
            && let Ok(mtime) = meta.modified()
        {
            let mtime_ms = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            map.insert(path.to_path_buf(), mtime_ms);
        }
    }

    let mut map = ChecksumMap::new();
    let flat_dirs = [
        memory_root.join("journal"),
        wiki_root.join("wisdom").join("reflections"),
        wiki_root.join("wisdom").join("anti-patterns"),
        wiki_root.join("wisdom").join("models"),
        wiki_root.join("wisdom").join("preferences"),
    ];
    for dir in &flat_dirs {
        if !dir.exists() {
            continue;
        }
        for path in list_md_files(dir)? {
            record_mtime(&mut map, &path);
        }
    }
    // Notions mirrors index_m3_semantic's recursive walk so the checksum
    // sidecar tracks exactly the set that incremental indexing reads.
    for path in list_md_files_recursive(&wiki_root.join("notions"))? {
        record_mtime(&mut map, &path);
    }
    Ok(map)
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

/// List `.md` files under a directory tree recursively (sorted; empty vec if
/// the dir doesn't exist).
fn list_md_files_recursive(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "md"))
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
        let _guard = crate::memvid::lock_and_reset_singletons();
        let tmp = TempDir::new().unwrap();
        let indexer = MemvidIndexer::new(tmp.path().join("memories"), tmp.path().join("wiki"));

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
        let _guard = crate::memvid::lock_and_reset_singletons();
        let tmp = TempDir::new().unwrap();

        // Create journal directory with one file
        let journal_dir = tmp.path().join("memories").join("journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(
            journal_dir.join("2026-06-01.md"),
            "---\nfrontmatter\n---\n# Session\n\n## Facts\n- learned rust\n\n## Reflections\n- coding is fun\n",
        )
        .unwrap();

        let indexer = MemvidIndexer::new(tmp.path().join("memories"), tmp.path().join("wiki"));
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m2_episodic(&mut store).unwrap();
        assert_eq!(files, 1);
        assert_eq!(chunks, 2); // Facts + Reflections
    }

    // ── MemvidIndexer — M3 semantic ─────────────────────────────────

    #[test]
    fn m3_indexes_entity_files() {
        let _guard = crate::memvid::lock_and_reset_singletons();
        let tmp = TempDir::new().unwrap();

        let entities_dir = tmp.path().join("wiki").join("notions");
        std::fs::create_dir_all(&entities_dir).unwrap();
        std::fs::write(
            entities_dir.join("rust.md"),
            "# Rust\n\nRust is a systems language.\n",
        )
        .unwrap();
        std::fs::write(entities_dir.join("empty.md"), "").unwrap();

        let indexer = MemvidIndexer::new(tmp.path().join("memories"), tmp.path().join("wiki"));
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m3_semantic(&mut store).unwrap();
        assert_eq!(files, 2); // scanned both (even empty)
        assert_eq!(chunks, 1); // only non-empty indexed
    }

    // ── MemvidIndexer — M4 wisdom ───────────────────────────────────

    #[test]
    fn m4_indexes_wisdom_subdirs() {
        let _guard = crate::memvid::lock_and_reset_singletons();
        let tmp = TempDir::new().unwrap();

        // Create all three wisdom subdirs with files
        for subdir in &["reflections", "anti-patterns", "models"] {
            let dir = tmp.path().join("wiki").join("wisdom").join(subdir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("item.md"), format!("# {subdir}\n\nSome wisdom.\n")).unwrap();
        }

        let indexer = MemvidIndexer::new(tmp.path().join("memories"), tmp.path().join("wiki"));
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m4_wisdom(&mut store).unwrap();
        assert_eq!(files, 3);
        assert_eq!(chunks, 3);
    }

    // ── MemvidIndexer — empty files ─────────────────────────────────

    #[test]
    fn indexer_skips_empty_files() {
        let _guard = crate::memvid::lock_and_reset_singletons();
        let tmp = TempDir::new().unwrap();

        let journal_dir = tmp.path().join("memories").join("journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(journal_dir.join("2026-06-01.md"), "").unwrap();

        let indexer = MemvidIndexer::new(tmp.path().join("memories"), tmp.path().join("wiki"));
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m2_episodic(&mut store).unwrap();
        assert_eq!(files, 1); // file was scanned
        assert_eq!(chunks, 0); // but no chunks written
    }

    // ── MemvidIndexer — full index_all ──────────────────────────────

    #[test]
    fn index_all_collects_across_tiers() {
        let _guard = crate::memvid::lock_and_reset_singletons();
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

        let indexer = MemvidIndexer::new(tmp.path().join("memories"), tmp.path().join("wiki"));
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let report = indexer.index_all(&mut store).unwrap();
        assert_eq!(report.files_scanned, 3); // 1 + 1 + 1
        assert_eq!(report.chunks_indexed, 4); // 2 + 1 + 1
        assert!(report.errors.is_empty());
    }

    // ── T071: temporal_entity tag + recency-weighted reorder ────────

    #[test]
    fn extract_anchor_date_parses_journal_ids_only() {
        assert_eq!(
            extract_anchor_date("journal-2026-06-01"),
            Some(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap())
        );
        assert_eq!(extract_anchor_date("knowledge-base"), None);
        assert_eq!(extract_anchor_date("journal-unknown"), None);
        assert_eq!(extract_anchor_date("journal-2026-6-1"), None);
    }

    #[test]
    fn recency_weight_decays_with_30_day_half_life() {
        let now = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        assert!((recency_weight(Some(today), now) - 1.0).abs() < 1e-9);

        let thirty_days_ago = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();
        assert!((recency_weight(Some(thirty_days_ago), now) - 0.5).abs() < 0.01);

        let evergreen = recency_weight(None, now);
        assert!((evergreen - 1.0).abs() < 1e-9);

        // Future-dated (clock skew) never exceeds 1.0.
        let future = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
        assert!((recency_weight(Some(future), now) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn reorder_by_recency_puts_oldest_first_undated_last() {
        let dir = PathBuf::from("/journal");
        let files = vec![
            dir.join("2026-08-01.md"),
            dir.join("notes.md"),
            dir.join("2026-06-01.md"),
            dir.join("2026-07-01.md"),
        ];
        let reordered = reorder_by_recency(files);
        assert_eq!(
            reordered,
            vec![
                dir.join("2026-06-01.md"),
                dir.join("2026-07-01.md"),
                dir.join("2026-08-01.md"),
                dir.join("notes.md"),
            ]
        );
    }

    #[test]
    fn m4_indexes_preferences_dir() {
        let _guard = crate::memvid::lock_and_reset_singletons();
        let tmp = TempDir::new().unwrap();

        let pref_dir = tmp.path().join("wiki").join("wisdom").join("preferences");
        std::fs::create_dir_all(&pref_dir).unwrap();
        std::fs::write(
            pref_dir.join("user-likes-rust.md"),
            "# Preference\n\nBody.\n",
        )
        .unwrap();

        let indexer = MemvidIndexer::new(tmp.path().join("memories"), tmp.path().join("wiki"));
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let (files, chunks) = indexer.index_m4_wisdom(&mut store).unwrap();
        assert_eq!(files, 1);
        assert_eq!(chunks, 1);
    }

    // ── T195/W1 regression: real Path Spec v2 layout ─────────────────
    // Each tier dir holds exactly one .md file, so a total of 3 scanned /
    // 3 indexed is only reachable when EVERY tier resolves its real v2
    // location ({global}/memories/journal, {global}/vault/wiki/…) — a
    // stranded tier reads 0 files and the totals drop. The pre-fix indexer
    // (single workspace_root) fails this test by construction.
    #[test]
    fn index_all_reads_path_spec_v2_layout() {
        let _guard = crate::memvid::lock_and_reset_singletons();
        let tmp = TempDir::new().unwrap();
        let paths = zen_core::paths::ZenPaths::for_testing(tmp.path().to_path_buf());

        let journal_dir = paths.journal_entries();
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(journal_dir.join("2026-06-01.md"), "## Facts\n- v2 fact\n").unwrap();

        let notions_dir = paths.wiki().join("notions").join("technology");
        std::fs::create_dir_all(&notions_dir).unwrap();
        std::fs::write(notions_dir.join("x.md"), "# X\n\nNotion body.\n").unwrap();

        let reflections_dir = paths.wiki().join("wisdom").join("reflections");
        std::fs::create_dir_all(&reflections_dir).unwrap();
        std::fs::write(reflections_dir.join("y.md"), "# Y\n\nWisdom body.\n").unwrap();

        let indexer = MemvidIndexer::new(paths.memory(), paths.wiki());
        let db_path = tmp.path().join("test.mv2");
        let mut store = ZenMemvidStore::new(db_path).unwrap();

        let report = indexer.index_all(&mut store).unwrap();
        assert_eq!(
            report.files_scanned, 3,
            "M2+M3+M4 must each find their v2 dir"
        );
        assert_eq!(report.chunks_indexed, 3);
        assert!(report.errors.is_empty());
    }

    // ── Phase 31 T208: provenance extra_metadata ──────────────────────

    fn provenance_fixture(tmp: &TempDir) -> (PathBuf, PathBuf) {
        let memory_root = tmp.path().join("memories");
        let wiki_root = tmp.path().join("wiki");

        let journal_dir = memory_root.join("journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(
            journal_dir.join("2026-06-01.md"),
            "## Facts\n- m2provtoken journal fact\n",
        )
        .unwrap();

        let notions_dir = wiki_root.join("notions").join("technology");
        std::fs::create_dir_all(&notions_dir).unwrap();
        std::fs::write(
            notions_dir.join("rust.md"),
            "# Rust\n\nm3provtoken notion body.\n",
        )
        .unwrap();

        let reflections_dir = wiki_root.join("wisdom").join("reflections");
        std::fs::create_dir_all(&reflections_dir).unwrap();
        std::fs::write(
            reflections_dir.join("lesson.md"),
            "# Lesson\n\nm4provtoken wisdom body.\n",
        )
        .unwrap();

        (memory_root, wiki_root)
    }

    fn find_hit(
        store: &ZenMemvidStore,
        token: &str,
        uri: &str,
    ) -> memvid_core::types::search::SearchHit {
        let resp = store
            .store()
            .search(memvid_core::SearchRequest {
                query: token.to_string(),
                top_k: 10,
                snippet_chars: 400,
                uri: None,
                scope: None,
                cursor: None,
                as_of_frame: None,
                as_of_ts: None,
                no_sketch: false,
                acl_context: None,
                acl_enforcement_mode: Default::default(),
            })
            .unwrap();
        resp.hits
            .into_iter()
            .find(|h| h.text.contains(token) && h.uri == uri)
            .unwrap_or_else(|| panic!("no hit for {token} at uri {uri}"))
    }

    fn extra_meta(hit: &memvid_core::types::search::SearchHit) -> BTreeMap<String, String> {
        hit.metadata
            .as_ref()
            .map(|m| m.extra_metadata.clone())
            .unwrap_or_default()
    }

    #[test]
    fn provenance_map_classifies_tiers_and_respects_gate() {
        let tmp = TempDir::new().unwrap();
        let (memory_root, wiki_root) = (tmp.path().join("memories"), tmp.path().join("wiki"));

        let off = MemvidIndexer::new(memory_root.clone(), wiki_root.clone());
        assert!(!off.provenance_metadata_enabled());
        assert!(
            off.provenance_map(&memory_root.join("journal").join("2026-06-01.md"))
                .is_empty(),
            "gate off ⇒ empty map ⇒ byte-identical puts"
        );

        let on = off.with_provenance_metadata(true);
        assert!(on.provenance_metadata_enabled());
        let m2 = on.provenance_map(&memory_root.join("journal").join("2026-06-01.md"));
        assert_eq!(m2.get(PROVENANCE_TIER_KEY).map(String::as_str), Some("m2"));
        assert_eq!(
            m2.get(PROVENANCE_SOURCE_PATH_KEY).map(String::as_str),
            Some("journal/2026-06-01.md")
        );
        let m3 = on.provenance_map(&wiki_root.join("notions").join("technology").join("rust.md"));
        assert_eq!(m3.get(PROVENANCE_TIER_KEY).map(String::as_str), Some("m3"));
        assert_eq!(
            m3.get(PROVENANCE_SOURCE_PATH_KEY).map(String::as_str),
            Some("notions/technology/rust.md")
        );
        let m4 = on.provenance_map(&wiki_root.join("wisdom").join("reflections").join("l.md"));
        assert_eq!(m4.get(PROVENANCE_TIER_KEY).map(String::as_str), Some("m4"));
        assert_eq!(
            m4.get(PROVENANCE_SOURCE_PATH_KEY).map(String::as_str),
            Some("wisdom/reflections/l.md")
        );
        // Files outside every indexed subtree get no metadata.
        assert!(
            on.provenance_map(&tmp.path().join("elsewhere.md"))
                .is_empty()
        );
    }

    #[test]
    fn provenance_disabled_by_default_frames_carry_no_extra_metadata() {
        let tmp = TempDir::new().unwrap();
        let (memory_root, wiki_root) = provenance_fixture(&tmp);
        let indexer = MemvidIndexer::new(memory_root, wiki_root);
        let mut store = ZenMemvidStore::from_store(
            crate::memvid_store::MemvidStore::open_or_create(&tmp.path().join("off.mv2")).unwrap(),
        );

        indexer.index_all(&mut store).unwrap();

        let tokens = [
            ("m2provtoken", "journal-2026-06-01"),
            ("m3provtoken", "knowledge-base"),
            ("m4provtoken", "knowledge-base"),
        ];
        for (token, uri) in tokens {
            let hit = find_hit(&store, token, uri);
            let meta = extra_meta(&hit);
            // Gate-off pin: the indexer writes NO provenance keys. (The map
            // itself may carry memvid-core's own enrichment entries, e.g.
            // `extractous_metadata` — engine-side writes, not the indexer's
            // channel, and timing-dependent under the extraction budget.)
            assert!(
                !meta.contains_key(PROVENANCE_SOURCE_PATH_KEY)
                    && !meta.contains_key(PROVENANCE_TIER_KEY),
                "gate-off pin: {token} frame must carry no zen provenance keys, got {meta:?}"
            );
        }
    }

    #[test]
    fn provenance_enabled_attaches_source_path_and_tier_per_tier() {
        let tmp = TempDir::new().unwrap();
        let (memory_root, wiki_root) = provenance_fixture(&tmp);
        let indexer = MemvidIndexer::new(memory_root, wiki_root).with_provenance_metadata(true);
        let mut store = ZenMemvidStore::from_store(
            crate::memvid_store::MemvidStore::open_or_create(&tmp.path().join("on.mv2")).unwrap(),
        );

        indexer.index_all(&mut store).unwrap();

        let expected = [
            (
                "m2provtoken",
                "journal-2026-06-01",
                "journal/2026-06-01.md",
                "m2",
            ),
            (
                "m3provtoken",
                "knowledge-base",
                "notions/technology/rust.md",
                "m3",
            ),
            (
                "m4provtoken",
                "knowledge-base",
                "wisdom/reflections/lesson.md",
                "m4",
            ),
        ];
        for (token, uri, source_path, tier) in expected {
            let hit = find_hit(&store, token, uri);
            let meta = extra_meta(&hit);
            assert_eq!(
                meta.get(PROVENANCE_SOURCE_PATH_KEY).map(String::as_str),
                Some(source_path),
                "token {token}"
            );
            assert_eq!(
                meta.get(PROVENANCE_TIER_KEY).map(String::as_str),
                Some(tier),
                "token {token}"
            );
        }
    }

    #[test]
    fn provenance_enabled_index_incremental_attaches_metadata() {
        let tmp = TempDir::new().unwrap();
        let (memory_root, wiki_root) = provenance_fixture(&tmp);
        let indexer = MemvidIndexer::new(memory_root, wiki_root).with_provenance_metadata(true);
        let mut store = ZenMemvidStore::from_store(
            crate::memvid_store::MemvidStore::open_or_create(&tmp.path().join("inc.mv2")).unwrap(),
        );

        // No previous checksums ⇒ incremental falls back to the full path,
        // which must carry the same provenance metadata.
        indexer.index_incremental(&mut store).unwrap();

        let hit = find_hit(&store, "m3provtoken", "knowledge-base");
        let meta = extra_meta(&hit);
        assert_eq!(
            meta.get(PROVENANCE_SOURCE_PATH_KEY).map(String::as_str),
            Some("notions/technology/rust.md")
        );
        assert_eq!(
            meta.get(PROVENANCE_TIER_KEY).map(String::as_str),
            Some("m3")
        );
    }
}
