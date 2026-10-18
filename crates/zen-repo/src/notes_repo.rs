use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{FtsResult, IndexNoteRequest};

/// Per-hit canonical-file read cap for the T222 snippet population.
///
/// The value is zen's established cap for reading a markdown file into
/// agent knowledge context — the T093 identity-file cap
/// (`zen-agents/src/zen_agent.rs IDENTITY_FILE_MAX_BYTES`) and the T214
/// wake-up-brief/note-file cap (`zen-agents/src/orchestrator.rs
/// WAKE_UP_BRIEF_MAX_BYTES`), both 256 KiB; the orchestrator's
/// `retrieve_knowledge_notes` already applies exactly this cap to these same
/// note files, so the data layer and its downstream consumer agree. Files
/// over the cap are NOT read (snippet stays empty, fail-open) — a const, not
/// a config key (T192 no-dormant-key discipline).
pub const SNIPPET_FILE_MAX_BYTES: u64 = 256 * 1024;

/// Read the canonical markdown file behind a search hit (T222).
///
/// `notes_fts` is a CONTENTLESS fts5 table (`content=''`, migration
/// 001:27-31): it is the ranking structure only, so FTS-side snippets are
/// structurally empty. Markdown files are the canonical source of truth
/// (AGENTS.md), so hit content comes from `notes_meta.file_path`.
///
/// Fail-open by contract: a missing, unreadable, non-UTF-8 or oversized file
/// yields an empty string — never an error and never a log flood (a vault on
/// an unmounted volume must degrade search content, not fail the query; same
/// direction as the T214 injection fail-open).
///
/// `pub(crate)` because [`crate::EmbeddingsRepo::search`] shares it: both tiers
/// must agree on one cap, and a second copy would drift silently.
pub(crate) fn read_canonical_snippet(file_path: &str) -> String {
    std::fs::metadata(file_path)
        .ok()
        .filter(|m| m.is_file() && m.len() <= SNIPPET_FILE_MAX_BYTES)
        .and_then(|_| std::fs::read_to_string(file_path).ok())
        .unwrap_or_default()
}

pub struct NotesRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> NotesRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    /// BM25 full-text search over indexed notes.
    ///
    /// Ranking is pure FTS5: `notes_fts MATCH` + `bm25()` ordering are
    /// untouched by T222. Each hit's `snippet` is populated from the
    /// canonical markdown file at `notes_meta.file_path` (capped at
    /// [`SNIPPET_FILE_MAX_BYTES`], fail-open to empty — see
    /// [`read_canonical_snippet`]); one data-layer fix serving every tier-2
    /// consumer (gateway `knowledge/search`, TUI Fast, the `tier2_search`
    /// agent tool and RRF fusion). The external-content FTS migration was
    /// REJECTED by the Phase 31 review verdict — FTS stays ranking-only.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.max(1) as i64;
        let mut results = sqlx::query_as::<_, FtsResult>(
            "SELECT nm.file_path as path, bm25(notes_fts) as score, '' as snippet \
             FROM notes_fts nf \
             JOIN notes_meta nm ON nf.rowid = nm.rowid \
             WHERE notes_fts MATCH ?1 \
             ORDER BY score LIMIT ?2",
        )
        .bind(query)
        .bind(limit)
        .fetch_all(self.client.pool())
        .await?;
        // File reads are blocking std::fs (capped at 256KiB each) — take them
        // off the async executor so a burst of hits cannot stall runtime workers.
        let paths: Vec<String> = results.iter().map(|r| r.path.clone()).collect();
        let snippets = tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .map(|p| read_canonical_snippet(p))
                .collect::<Vec<String>>()
        })
        .await
        .unwrap_or_default();
        for (result, snippet) in results.iter_mut().zip(snippets) {
            result.snippet = snippet;
        }
        Ok(results)
    }

    /// Index (or re-index) one note into `notes_meta` + the contentless
    /// `notes_fts` ranking table.
    ///
    /// **Deterministic rowid (2026-10-17 data-bloat fix).** Both tables are
    /// written with an EXPLICIT rowid derived from `req.id`
    /// (`sha256(id)[..8]` big-endian masked to the sign bit). The previous
    /// implementation let `notes_meta`'s `INSERT OR REPLACE` allocate a fresh
    /// hidden rowid on every write (its PK is `id TEXT`), then bound
    /// `notes_fts` to `last_insert_rowid()`. Because the rowid therefore
    /// changed on every call, the FTS rowid conflict never happened and every
    /// re-index merely APPENDED: production reached ~554k orphan FTS rows
    /// against ~2.5k live notes. A stable rowid is only safe together with
    /// migration 011's `contentless_delete=1` — without it, `INSERT OR REPLACE`
    /// on a bare contentless table leaves every historical version's terms in
    /// the index, which the search JOIN would surface as false positives for
    /// the CURRENT note.
    ///
    /// **`content_hash`** is the SHA-256 of the WHOLE file at `req.file_path`
    /// (lowercase hex), so it is directly comparable to
    /// `zen_vault::tindy::checksum::compute_file_checksum` — the value the
    /// reindex change-gate compares against via [`all_file_checksums`]. When
    /// `file_path` is empty or the file cannot be read, it falls back to the
    /// hash of `content` (the body). A missing file must never break indexing.
    ///
    /// **`embedded_hash`** is reset to `''` on every call (explicitly, not via
    /// the column default): a re-index means the bytes changed from the search
    /// index's point of view, so any vector that existed was built from
    /// different bytes. The caller marks it fresh again only after a
    /// successful vector write ([`mark_embedded`]).
    pub async fn index_note(&self, req: IndexNoteRequest<'_>) -> Result<()> {
        let id = req.id.to_string();
        let title = req.title.to_string();
        let content = req.content.to_string();
        let tags = req.tags.to_string();
        let file_path = req.file_path.to_string();
        let source = req.source.to_string();

        // One deterministic rowid per note id, shared by both tables so the
        // search JOIN (`notes_fts.rowid = notes_meta.rowid`) holds forever.
        let digest = Sha256::digest(req.id.as_bytes());
        let rowid_bytes: [u8; 8] = digest[..8].try_into().expect("sha256 has 32 bytes");
        // mask the sign bit: rowid must be non-negative
        let rowid: i64 = i64::from_be_bytes(rowid_bytes) & i64::MAX;

        // Whole-file hash first (matches the reindex gate); body hash fallback.
        let body_hash: String = Sha256::digest(content.as_bytes())
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        let content_hash: String = if file_path.is_empty() {
            body_hash
        } else {
            match std::fs::read(&file_path) {
                Ok(bytes) => Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect(),
                Err(_) => body_hash,
            }
        };

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO notes_meta (rowid, id, file_path, source, domain, project, created_at, updated_at, content_hash, embedded_hash) \
                     VALUES (?1, ?2, ?3, ?4, '', '', datetime('now'), datetime('now'), ?5, '')",
                    rusqlite::params![rowid, id, file_path, source, content_hash],
                )?;
                conn.execute(
                    "INSERT OR REPLACE INTO notes_fts (rowid, id, title, content, tags) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![rowid, id, title, content, tags],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Record that the vec0 vector for `id` was built from `hash`'s bytes.
    ///
    /// **Description:** sets `notes_meta.embedded_hash = hash`, claiming the
    /// stored vector is current. Callers MUST invoke this only AFTER the
    /// vector write succeeded; a failure between the two leaves the row dirty
    /// so the next embedding-enabled reindex retries it.
    ///
    /// **Parameters:** `id` — `notes_meta.id`; `hash` — the SHA-256 of the
    /// file bytes the vector was built from (the same value stored in
    /// `content_hash`).
    ///
    /// **Returns:** `Ok(())` after the update. A missing `id` updates zero rows
    /// and is not an error — the note may have been removed between indexing
    /// and embedding.
    ///
    /// **Errors:** [`SqliteError`] when the write fails.
    pub async fn mark_embedded(&self, id: &str, hash: &str) -> Result<()> {
        let id = id.to_string();
        let hash = hash.to_string();
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE notes_meta SET embedded_hash = ?2 WHERE id = ?1",
                    rusqlite::params![id, hash],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Return `id -> embedded_hash` for every indexed note.
    ///
    /// **Description:** the durable source of vector freshness for the reindex
    /// change gate. Empty values are included: `''` means "vector dirty or
    /// unknown", which the gate must see rather than infer.
    ///
    /// **Parameters:** none.
    ///
    /// **Returns:** one entry per `notes_meta` row, keyed by `id`.
    ///
    /// **Errors:** [`SqliteError`] when the query fails; callers fail open by
    /// treating every vector as stale instead of aborting the reindex.
    pub async fn all_embedded_hashes(&self) -> Result<HashMap<String, String>> {
        let rows =
            sqlx::query_as::<_, (String, String)>("SELECT id, embedded_hash FROM notes_meta")
                .fetch_all(self.client.pool())
                .await?;
        Ok(rows.into_iter().collect())
    }

    /// Return `(file_path, content_hash)` for every indexed note whose hash is non-empty.
    /// Used by the reindex change-gate so it survives across processes.
    pub async fn all_file_checksums(&self) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT file_path, content_hash FROM notes_meta WHERE content_hash != ''",
        )
        .fetch_all(self.client.pool())
        .await?;
        Ok(rows)
    }
}
