use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use tracing::{debug, info, warn};
use walkdir::WalkDir;
use zen_repo::{EmbeddingsRepo, IndexNoteRequest, NotesRepo, SqliteClient};

use super::checksum;
use super::embeddings::{
    SYSTEM_EMBEDDING_DIM, compute_embeddings_for_text_tagged, embedding_engine_descriptor,
};
use crate::note;
use crate::search::Tier3Search;

// ── Walk exclusions ──────────────────────────────────────────────────

/// Directory name(s) pruned from the reindex walk.
///
/// `vault/iterations/**` holds E5 page-version snapshots written by
/// `PageIterations` before a wiki page is overwritten. They are derived
/// backup artifacts, NOT notes: in production 2303 of 2474 `notes_meta` rows
/// (94.8%) were iterations, they never parse as notes (a frontmatter-parse
/// warning every cycle) and their content is already carried by the wiki
/// page they snapshot. Mirrors the `wisdom`/`communities` exclusion precedent
/// in `graph_verify::concept_page_inventory`.
const EXCLUDED_DIR_COMPONENTS: &[&str] = &["iterations"];

/// Whether a walk entry lies under an excluded directory.
///
/// Matching is by COMPONENT of the path relative to the walk root — never a
/// string prefix on the display path — so only a real `iterations` directory
/// segment is pruned; a file named `iterations.md` or a directory named
/// `my-iterations` is indexed normally.
fn is_excluded_entry(entry: &walkdir::DirEntry, root: &Path) -> bool {
    let Ok(rel) = entry.path().strip_prefix(root) else {
        return false;
    };
    rel.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        EXCLUDED_DIR_COMPONENTS.iter().any(|d| *d == name)
    })
}

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
    /// Files whose vector was refreshed without rewriting `content_hash` or
    /// `notes_fts`: the content was already indexed but `embedded_hash`
    /// showed the stored vector was built from older bytes. A subset of
    /// [`Self::files_embedded`], disjoint from [`Self::files_updated`].
    pub files_reembedded: usize,
    /// Files successfully embedded into `note_embeddings` (vec0).
    pub files_embedded: usize,
    /// Files whose stored vector came from the hash fallback (degraded — a
    /// non-semantic pseudo-vector, not a real semantic embedding). A subset of
    /// [`Self::files_embedded`]; report-only, never gates reindex work.
    ///
    /// A degraded vector is still written into vec0 (byte-overlap retrieval
    /// beats nothing), but the page is deliberately NOT marked fresh — its
    /// `embedded_hash` stays `''` — so these files read as dirty and a later
    /// run with a working provider re-embeds them.
    pub embeddings_degraded: usize,
    /// Files where embedding generation was skipped (fallback exhausted or no client).
    pub embeddings_skipped: usize,
    /// Non-fatal error messages encountered while processing files.
    pub errors: Vec<String>,
}

// ── Note identity ────────────────────────────────────────────────────

/// Resolve `(note_id, body, tags)` for a markdown file.
///
/// Falls back to `auto-{file_stem}` when the frontmatter has no `id` (an
/// empty primary key would collide in `note_embeddings`) and indexes the raw
/// file text when frontmatter parsing fails. Shared by the full index path and
/// the vector-only refresh path so both address the same `notes_meta` row.
fn derive_note_identity(
    content: &str,
    file_name: &str,
    file_display: &str,
) -> (String, String, String) {
    match note::parse_frontmatter(content) {
        Ok(parsed_note) => {
            let id = if parsed_note.id.is_empty() {
                format!("auto-{}", file_name)
            } else {
                parsed_note.id
            };
            let tags_str = parsed_note.tags.join(",");
            (id, parsed_note.content, tags_str)
        }
        Err(e) => {
            // No front matter at all is normal for plain Markdown notes and the
            // generated `index.md`/`log.md` that OKF §6 exempts; the raw fallback
            // below is their intended handling, so only a present-but-malformed
            // front matter warns (this used to be ~14 WARNs per cycle forever).
            if note::has_frontmatter(content) {
                warn!(
                    "Frontmatter parse failed for {}: {} — indexing as raw",
                    file_display, e
                );
            } else {
                debug!("No frontmatter for {} — indexing as raw", file_display);
            }
            (
                format!("auto-{}", file_name),
                content.to_string(),
                String::new(),
            )
        }
    }
}

// ── Reindexer ────────────────────────────────────────────────────────

/// Outcome of a vector-only reindex pass (see
/// [`Reindexer::refresh_embedding`]).
enum VectorRefresh {
    /// `embedded_hash` already matches the current content hash.
    Fresh,
    /// A new vector was stored. `embedded_hash` is marked fresh only for a
    /// real (non-degraded) embedding; a degraded hash-fallback vector is
    /// stored but left dirty.
    Reembedded,
    /// Embedding was skipped or failed; the row stays dirty for the next run.
    Skipped,
}

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
    force_full: bool,
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
            force_full: false,
        }
    }

    /// Disable vector embedding generation (FTS5-only mode).
    pub fn without_embeddings(mut self) -> Self {
        self.embed = false;
        self
    }

    /// Force a full reindex for one run: bypass the DB-backed change gate so
    /// every eligible file is reprocessed regardless of the stored
    /// `notes_meta.content_hash`.
    ///
    /// REQUIRED after [`rebuild_fts_indexes`](Self::rebuild_fts_indexes):
    /// that empties the derived `notes_fts` index while `notes_meta` hashes
    /// persist, so an ordinarily-gated reindex would read every file as
    /// "unchanged" and leave search silently broken.
    pub fn force_full(mut self) -> Self {
        self.force_full = true;
        self.known_checksums.clear();
        self
    }

    /// Register a known checksum for a file.
    ///
    /// Call this once per file before invoking [`Reindexer::reindex`] if
    /// you maintain a `notes_metadata` table from disk / SQLite.
    pub fn set_known_checksum(&mut self, file_path: String, checksum: String) {
        self.known_checksums.insert(file_path, checksum);
    }

    /// Rebuild FTS5 indexes when sync triggers have drifted (manual SQL
    /// edits, partial rollbacks, trigger failures). Required by Constitution
    /// Principle XIII #8.
    ///
    /// Each table gets its OWN statement: `notes_fts` is contentless
    /// (`content=''`), and a contentless fts5 table REJECTS `'rebuild'`
    /// ("'rebuild' may not be used with a contentless fts5 table"). The
    /// previous single `execute_batch` aborted on that first statement, so the
    /// normal `notions_fts` never rebuilt either. `'delete-all'` is the
    /// contentless equivalent — it EMPTIES the index, so callers must follow
    /// it with a forced full reindex ([`Reindexer::force_full`]) to repopulate.
    pub async fn rebuild_fts_indexes(&self) -> Result<()> {
        let Some(client) = self.db_client.as_ref() else {
            return Err(anyhow::anyhow!(
                "rebuild_fts_indexes requires a database client"
            ));
        };
        client
            .writer()
            .call(|conn| {
                conn.execute("INSERT INTO notes_fts(notes_fts) VALUES('delete-all')", [])?;
                conn.execute("INSERT INTO notions_fts(notions_fts) VALUES('rebuild')", [])?;
                Ok(())
            })
            .await
            .map_err(anyhow::Error::from)?;
        Ok(())
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

        // ── DB-backed change gate preload ───────────────────────────
        // `set_known_checksum` has no production caller, so the durable
        // source of "what is already indexed" is `notes_meta.content_hash`
        // (whole-file SHA-256, identical to `compute_file_checksum`).
        // Production constructs a FRESH `Reindexer` every cycle, so without
        // this preload the gate is always empty and every file re-reads as
        // new (`scanned=2435 updated=2435 unchanged=0`). Fail-open: a DB read
        // error degrades to always-reindex and must NEVER abort a reindex.
        let mut known = if self.force_full {
            HashMap::new()
        } else {
            self.known_checksums.clone()
        };
        if !self.force_full
            && known.is_empty()
            && let Some(client) = &self.db_client
        {
            match NotesRepo::new(client).all_file_checksums().await {
                Ok(rows) => {
                    debug!("Reindex gate: preloaded {} stored checksum(s)", rows.len());
                    known = rows.into_iter().collect();
                }
                Err(e) => {
                    warn!(
                        "Reindex gate: could not preload stored checksums ({e}) — \
                         reindexing all files"
                    );
                }
            }
        }

        // The vector-freshness signal is separate: a matching `content_hash`
        // only proves the FTS row agrees with the file, so `embedded_hash`
        // decides whether the stored vec0 vector was built from the same
        // bytes. Fail-open: a read error treats every vector as stale and
        // re-embeds it (idempotent) rather than aborting the reindex.
        let mut embedded: HashMap<String, String> = HashMap::new();
        if self.embed
            && !self.force_full
            && let Some(client) = &self.db_client
        {
            match NotesRepo::new(client).all_embedded_hashes().await {
                Ok(rows) => {
                    debug!("Reindex gate: preloaded {} embedded hash(es)", rows.len());
                    embedded = rows;
                }
                Err(e) => {
                    warn!(
                        "Reindex gate: could not preload embedded hashes ({e}) — \
                         every vector will be treated as stale"
                    );
                }
            }
        }

        // ── Engine-fingerprint gate ───────────────────────────────────────
        if let Some(client) = &self.db_client {
            let active = embedding_engine_descriptor()
                .map(|(backend, model)| (backend, model, SYSTEM_EMBEDDING_DIM as i64));
            match EmbeddingsRepo::new(client).engine_fingerprint().await {
                Ok(stored) => match fingerprint_gate(stored, active.clone()) {
                    GateDecision::Proceed => {}
                    GateDecision::Write => {
                        if let Some((backend, model, dim)) = active.as_ref() {
                            let set = EmbeddingsRepo::new(client)
                                .set_engine_fingerprint(backend, model, *dim)
                                .await;
                            if let Err(e) = set {
                                warn!("Fingerprint write failed ({e}) — proceeding unrecorded");
                            }
                        }
                    }
                    GateDecision::UnknownEngine => {
                        warn!("Embedding engine unknown (empty router) — gate skipped");
                    }
                    GateDecision::Mismatch(stored_fp, active_fp) => {
                        return Err(anyhow::anyhow!(
                            "embedding engine fingerprint mismatch: stored {stored_fp:?} \
                             vs active {active_fp:?} — mixing embedding engines in one vec0 \
                             table makes KNN distances meaningless; run \
                             `zen wiki reindex --reset-embeddings` to reset the vec0 tables, \
                             then reindex to rebuild every vector with the active engine"
                        ));
                    }
                },
                Err(e) => {
                    warn!("Engine fingerprint read failed ({e}) — gate skipped");
                }
            }
        }

        // Collect all .md files (recursive), pruning derived-artifact dirs.
        // Walk errors go to the report: an unreadable subtree would otherwise
        // silently vanish from FTS + vec0 (neither scanned nor counted).
        let md_files: Vec<_> = WalkDir::new(knowledge_dir)
            .into_iter()
            .filter_entry(|e| !is_excluded_entry(e, knowledge_dir))
            .filter_map(|e| match e {
                Ok(entry) => Some(entry),
                Err(err) => {
                    warn!("Reindex walk error: {}", err);
                    report.errors.push(format!("walk: {}", err));
                    None
                }
            })
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

            // Decide: unchanged, changed, or vector-only refresh.
            let stored = known.get(&file_display);
            let needs_index = match stored {
                None => true, // new file
                Some(sc) => sc != &current_checksum,
            };

            if !needs_index && (!self.embed || self.db_client.is_none()) {
                report.files_unchanged += 1;
                continue;
            }

            if !needs_index {
                // Content already indexed: only the vector can be stale.
                match self
                    .refresh_embedding(
                        file_path,
                        &file_display,
                        &current_checksum,
                        &embedded,
                        &mut report,
                    )
                    .await
                {
                    Ok(VectorRefresh::Fresh) => report.files_unchanged += 1,
                    Ok(VectorRefresh::Reembedded) => {
                        info!("Re-embedded (vector-only): {}", file_display);
                        report.files_reembedded += 1;
                    }
                    // Skipped/failed: the row keeps its stale hash and the next
                    // embedding-enabled run retries it.
                    Ok(VectorRefresh::Skipped) => {}
                    Err(e) => {
                        warn!("Vector refresh failed for {}: {}", file_display, e);
                        report
                            .errors
                            .push(format!("reembed {}: {}", file_display, e));
                    }
                }
                continue;
            }

            // Process: parse frontmatter, FTS5 index, embedding index, update checksum.
            match self
                .process_file(file_path, &current_checksum, &mut report)
                .await
            {
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
            "Reindex complete: scanned={} updated={} reembedded={} unchanged={} embedded={} embed_skipped={} errors={} degraded={}",
            report.files_scanned,
            report.files_updated,
            report.files_reembedded,
            report.files_unchanged,
            report.files_embedded,
            report.embeddings_skipped,
            report.errors.len(),
            report.embeddings_degraded
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

        let (note_id, body_content, tags) =
            derive_note_identity(&content, &file_name, &file_display);

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
                self.store_embedding(
                    client,
                    &note_id,
                    &body_content,
                    &file_display,
                    checksum,
                    report,
                )
                .await;
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

    /// Refresh only the vec0 vector for a file whose content is already
    /// indexed but whose `embedded_hash` is stale.
    ///
    /// Never calls `index_note`, so `content_hash` and `notes_fts` are left
    /// untouched. Returns [`VectorRefresh::Fresh`] when the stored vector is
    /// already current, [`VectorRefresh::Reembedded`] after a vector write
    /// (a degraded hash-fallback vector is written but left dirty), and
    /// [`VectorRefresh::Skipped`] when generation/insert failed (the row
    /// stays dirty and is retried). `Err` is reserved for a file that cannot
    /// be read.
    async fn refresh_embedding(
        &self,
        file_path: &Path,
        file_display: &str,
        checksum: &str,
        embedded: &HashMap<String, String>,
        report: &mut ReindexReport,
    ) -> Result<VectorRefresh> {
        let Some(client) = &self.db_client else {
            if self.embed {
                report.embeddings_skipped += 1;
            }
            return Ok(VectorRefresh::Skipped);
        };
        let content = fs::read_to_string(file_path)?;
        let file_name = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();
        let (note_id, body_content, _tags) =
            derive_note_identity(&content, &file_name, file_display);

        if embedded.get(&note_id).is_some_and(|h| h == checksum) {
            return Ok(VectorRefresh::Fresh);
        }

        if self
            .store_embedding(
                client,
                &note_id,
                &body_content,
                file_display,
                checksum,
                report,
            )
            .await
        {
            Ok(VectorRefresh::Reembedded)
        } else {
            Ok(VectorRefresh::Skipped)
        }
    }

    /// Compute and store the vec0 vector for `body_content`. Returns `true`
    /// when the vector reached vec0. A DEGRADED (hash-fallback) embedding is
    /// still inserted — byte-overlap retrieval beats nothing — but it is
    /// deliberately NOT marked fresh: `embedded_hash` stays `''` so the page
    /// reads as dirty and a later run with a working provider re-embeds it.
    /// Only a non-degraded (semantic) vector calls
    /// [`mark_embedded`](NotesRepo::mark_embedded); an insert or mark failure
    /// likewise leaves `embedded_hash` dirty. An all-zero vector (empty /
    /// whitespace-only body) is neither inserted nor marked: it would pollute
    /// KNN with a meaningless zero-norm row while reading as a healthy fresh
    /// semantic vector.
    async fn store_embedding(
        &self,
        client: &SqliteClient,
        note_id: &str,
        body_content: &str,
        file_display: &str,
        checksum: &str,
        report: &mut ReindexReport,
    ) -> bool {
        let content_owned = body_content.to_string();
        // Provider embedding is sync and can hang; a reindex must not park the
        // async worker driving it, so compute on a blocking thread.
        let computed =
            tokio::task::spawn_blocking(move || compute_embeddings_for_text_tagged(&content_owned))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("embedding task join failed: {e}")));
        match computed {
            Ok((embedding, degraded))
                if !embedding.is_empty() && embedding.iter().any(|v| *v != 0.0) =>
            {
                match Tier3Search
                    .insert_embedding(client, note_id, &embedding)
                    .await
                {
                    Ok(()) => {
                        if degraded {
                            report.files_embedded += 1;
                            report.embeddings_degraded += 1;
                            debug!(
                                "Embedded (degraded, left dirty): file={} id={} dim={}",
                                file_display,
                                note_id,
                                embedding.len()
                            );
                            return true;
                        }
                        match NotesRepo::new(client)
                            .mark_embedded(note_id, checksum)
                            .await
                        {
                            Ok(()) => {
                                report.files_embedded += 1;
                                debug!(
                                    "Embedded: file={} id={} dim={}",
                                    file_display,
                                    note_id,
                                    embedding.len()
                                );
                                true
                            }
                            Err(e) => {
                                warn!(
                                    "Embedding freshness mark failed for {} (id={}): {} — vector stored but treated as stale",
                                    file_display, note_id, e
                                );
                                // Operator-visible on purpose (unlike the other
                                // skip branches): the CLI never prints the
                                // skip counter, so this would be invisible.
                                report
                                    .errors
                                    .push(format!("embed-mark {}: {}", file_display, e));
                                report.embeddings_skipped += 1;
                                false
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Embedding insert failed for {} (id={}): {} — FTS5 still indexed",
                            file_display, note_id, e
                        );
                        report.embeddings_skipped += 1;
                        false
                    }
                }
            }
            Ok(_) => {
                report.embeddings_skipped += 1;
                false
            }
            Err(e) => {
                warn!(
                    "Embedding generation failed for {}: {} — this should not happen (3-fallback chain)",
                    file_display, e
                );
                report.embeddings_skipped += 1;
                false
            }
        }
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

// ── Engine-fingerprint gate (pure decision seam) ─────────────────────

#[derive(Debug, PartialEq)]
enum GateDecision {
    Write,
    Proceed,
    Mismatch((String, String, i64), (String, String, i64)),
    UnknownEngine,
}

fn fingerprint_gate(
    stored: Option<(String, String, i64)>,
    active: Option<(String, String, i64)>,
) -> GateDecision {
    match (stored, active) {
        (None, Some(_)) => GateDecision::Write,
        (Some(stored), Some(active)) if stored == active => GateDecision::Proceed,
        (Some(stored), Some(active)) => GateDecision::Mismatch(stored, active),
        (None, None) | (Some(_), None) => GateDecision::UnknownEngine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(backend: &str, model: &str) -> (String, String, i64) {
        (backend.to_string(), model.to_string(), 4096)
    }

    #[test]
    fn fingerprint_gate_writes_when_stored_missing() {
        assert_eq!(
            fingerprint_gate(None, Some(fp("ollama", "bge-m3"))),
            GateDecision::Write
        );
    }

    #[test]
    fn fingerprint_gate_proceeds_on_matching_fingerprint() {
        let engine = fp("ollama", "bge-m3");
        assert_eq!(
            fingerprint_gate(Some(engine.clone()), Some(engine)),
            GateDecision::Proceed
        );
    }

    #[test]
    fn fingerprint_gate_rejects_mismatched_engines() {
        let stored = fp("ollama", "bge-m3");
        let active = fp("openai", "text-embedding-3-small");
        assert_eq!(
            fingerprint_gate(Some(stored.clone()), Some(active.clone())),
            GateDecision::Mismatch(stored, active)
        );
    }

    #[test]
    fn fingerprint_gate_fails_open_when_engine_unknown() {
        assert_eq!(
            fingerprint_gate(Some(fp("ollama", "bge-m3")), None),
            GateDecision::UnknownEngine
        );
        assert_eq!(fingerprint_gate(None, None), GateDecision::UnknownEngine);
    }
}
