//! Zen-native MemvidStore over the memvid-core engine (replaces rig-memvid 0.4.2). Codex-style: engine stays external (memvid-core), domain wrapper is ours.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

pub use memvid_core;

/// A persistent, file-backed lexical index over a memvid `.mv2` archive.
///
/// Thin `Arc<Mutex<memvid_core::Memvid>>` wrapper replacing the rig-memvid
/// crate. Cheap to clone (shares the `Arc` with every clone); writes are
/// serialised through the inner mutex. Every public method locks the mutex,
/// delegates to the inner engine, and maps engine errors via `?` into
/// `anyhow::Result`.
///
/// The lock is [`std::sync::Mutex`] (not `tokio::sync::Mutex`): no `.await`
/// ever happens while a guard is live, so the clippy `await_holding_lock`
/// lint stays satisfied.
#[derive(Clone)]
pub struct MemvidStore {
    inner: Arc<Mutex<memvid_core::Memvid>>,
}

impl std::fmt::Debug for MemvidStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemvidStore").finish_non_exhaustive()
    }
}

impl MemvidStore {
    /// Wraps an already-open [`memvid_core::Memvid`] handle.
    pub fn from_memvid(memvid: memvid_core::Memvid) -> Self {
        Self {
            inner: Arc::new(Mutex::new(memvid)),
        }
    }

    /// Open an existing `.mv2` file. Errors if the file does not exist.
    ///
    /// Enables the lexical (BM25/Tantivy) index — zen always uses default
    /// lex features and never the `vec`/ACL configuration.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut memvid = memvid_core::Memvid::open(path)?;
        memvid.enable_lex()?;
        Ok(Self::from_memvid(memvid))
    }

    /// Open the file if it exists, otherwise create it.
    ///
    /// Enables the lexical (BM25/Tantivy) index — zen always uses default
    /// lex features and never the `vec`/ACL configuration.
    pub fn open_or_create(path: &Path) -> anyhow::Result<Self> {
        let mut memvid = if path.exists() {
            memvid_core::Memvid::open(path)?
        } else {
            memvid_core::Memvid::create(path)?
        };
        memvid.enable_lex()?;
        Ok(Self::from_memvid(memvid))
    }

    /// Open the file read-only.
    ///
    /// Enables the lexical (BM25/Tantivy) index — zen always uses default
    /// lex features and never the `vec`/ACL configuration.
    pub fn open_read_only(path: &Path) -> anyhow::Result<Self> {
        let mut memvid = memvid_core::Memvid::open_read_only(path)?;
        memvid.enable_lex()?;
        Ok(Self::from_memvid(memvid))
    }

    /// Acquire the inner mutex. Returns an error if a prior holder of the
    /// lock panicked (poisoned).
    fn lock(&self) -> anyhow::Result<MutexGuard<'_, memvid_core::Memvid>> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("memvid store lock poisoned"))
    }

    /// Number of frames currently stored in the underlying `.mv2` file.
    pub fn frame_count(&self) -> anyhow::Result<usize> {
        Ok(self.lock()?.frame_count())
    }

    /// Aggregate statistics for the underlying memory.
    pub fn stats(&self) -> anyhow::Result<memvid_core::types::frame::Stats> {
        Ok(self.lock()?.stats()?)
    }

    /// Append a UTF-8 text payload to the archive and immediately commit.
    ///
    /// Returns the assigned `frame_id`.
    pub fn put_text(&self, text: &str, options: memvid_core::PutOptions) -> anyhow::Result<u64> {
        let mut guard = self.lock()?;
        let id = guard.put_bytes_with_options(text.as_bytes(), options)?;
        guard.commit()?;
        Ok(id)
    }

    /// Insert a fully-built [`memvid_core::MemoryCard`] onto the memories
    /// track. The card's `id` field is overwritten with a freshly assigned
    /// [`memvid_core::MemoryCardId`], which is returned.
    pub fn put_memory_card(
        &self,
        card: memvid_core::MemoryCard,
    ) -> anyhow::Result<memvid_core::MemoryCardId> {
        let mut guard = self.lock()?;
        Ok(guard.put_memory_card(card)?)
    }

    /// Run a [`memvid_core::SearchRequest`] directly.
    ///
    /// Acquires the store's inner [`Mutex`] for the duration of the call.
    /// Do **not** invoke this (or any other `MemvidStore` method) from
    /// within a write path that already holds the lock — a re-entrant call
    /// would deadlock.
    pub fn search(
        &self,
        request: memvid_core::SearchRequest,
    ) -> anyhow::Result<memvid_core::SearchResponse> {
        let mut guard = self.lock()?;
        let resp = guard.search(request)?;
        Ok(resp)
    }

    /// All memory cards associated with `entity`, returned as owned values
    /// (the underlying lock is released before returning).
    ///
    /// Returns an empty `Vec` if the entity is unknown.
    pub fn entity_memories(&self, entity: &str) -> anyhow::Result<Vec<memvid_core::MemoryCard>> {
        let guard = self.lock()?;
        Ok(guard
            .get_entity_memories(entity)
            .into_iter()
            .cloned()
            .collect())
    }
}

/// How to select memory cards for context injection.
#[derive(Debug, Clone)]
pub enum CardSelection {
    /// The principal's cards regardless of query.
    ForPrincipal(String),
}

/// Select memory cards from `store` according to `selection`.
///
/// `ForPrincipal` returns the principal's cards ignoring `query`, ordered
/// most-recent-first (upstream orders by relevance with recency tie-break;
/// the query is ignored for `ForPrincipal`, so recency ordering is the
/// faithful minimal port).
pub fn select_cards(
    store: &MemvidStore,
    selection: &CardSelection,
    query: &str,
) -> anyhow::Result<Vec<memvid_core::MemoryCard>> {
    match selection {
        CardSelection::ForPrincipal(principal) => {
            let _ = query;
            let mut cards = store.entity_memories(principal)?;
            cards.sort_by_key(|b| std::cmp::Reverse(b.effective_timestamp()));
            Ok(cards)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_put_text_and_search() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("roundtrip.mv2");
        let store = MemvidStore::open_or_create(&path).unwrap();

        let frame_id = store
            .put_text(
                "zen-native memvid store roundtrip",
                memvid_core::PutOptions::default(),
            )
            .unwrap();
        assert!(frame_id > 0);

        let response = store
            .search(memvid_core::SearchRequest {
                query: "zen-native memvid store roundtrip".to_string(),
                top_k: 1,
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
        assert!(!response.hits.is_empty());
    }

    #[test]
    fn entity_memories_empty_for_unknown_entity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("unknown.mv2");
        let store = MemvidStore::open_or_create(&path).unwrap();

        let cards = store.entity_memories("no-such-entity").unwrap();
        assert!(cards.is_empty());
    }
}
