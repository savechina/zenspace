use anyhow::Result;
use memvid_core::MemoryCardBuilder;
use rig_memvid::memvid_core;
use rig_memvid::{MemoryConfig, MemvidPersistHook, MemvidStore, WritePolicy};
use std::collections::HashMap;
use std::sync::Arc;

use zen_core::notion_graph::NotionGraphProvider;

/// Minimum confidence threshold for auto-extracted triplets (D9).
/// Cards from `extract_triplets` below this threshold are filtered out on retrieval.
/// The RulesEngine typically produces 0.7-0.9 confidence; we keep >=0.8.
pub const TRIPLET_MIN_CONFIDENCE: f32 = 0.8;

pub struct ZenMemvidStore {
    store: MemvidStore,
    notion_graph: Option<Arc<dyn NotionGraphProvider>>,
}

impl ZenMemvidStore {
    pub fn new(memory_path: std::path::PathBuf) -> Result<Self> {
        let store = MemvidStore::builder()
            .path(memory_path)
            .enable_lex()
            .open_or_create()?;

        Ok(Self {
            store,
            notion_graph: None,
        })
    }

    pub fn with_notion_graph(mut self, provider: Arc<dyn NotionGraphProvider>) -> Self {
        self.notion_graph = Some(provider);
        self
    }

    pub fn into_inner(self) -> MemvidStore {
        self.store
    }

    pub fn store(&self) -> &MemvidStore {
        &self.store
    }

    pub fn from_store(store: MemvidStore) -> Self {
        Self {
            store,
            notion_graph: None,
        }
    }

    pub fn retrieve(&self, session_id: &str) -> Result<Vec<String>> {
        let cards = self.store.entity_memories(session_id)?;
        Ok(cards
            .into_iter()
            .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
            .map(|c| format!("[{}] {}={}: {}", c.kind, c.entity, c.slot, c.value))
            .collect())
    }

    pub fn retrieve_high_confidence(&self, session_id: &str) -> Result<Vec<String>> {
        let cards = self.store.entity_memories(session_id)?;
        Ok(cards
            .into_iter()
            .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
            .map(|c| {
                format!(
                    "[{}] {}={}: {} (confidence: {:.2})",
                    c.kind,
                    c.entity,
                    c.slot,
                    c.value,
                    c.confidence.unwrap_or(1.0)
                )
            })
            .collect())
    }

    /// Persist a conversation turn with both full-text and structured card storage.
    ///
    /// Writes the turn text via `put_text()` with enriched `PutOptions` for full-text
    /// search, then writes a `MemoryCard` via `put_memory_card()` linked to the frame
    /// from the text write. The card enables notion/slot/value graph queries.
    ///
    /// # Arguments
    /// * `session_id` - The session scope for this turn
    /// * `role` - The speaker role ("user" or "assistant")
    /// * `content` - The turn content to persist
    ///
    /// # Returns
    /// `Ok((frame_id, card_id))` on success. If `put_memory_card()` fails, a warning
    /// is logged but the frame_id from the text write is still returned.
    /// Persist a structured [`MemoryEntry`] into the memvid store.
    ///
    /// Serializes the entry as JSON and writes it via [`MemvidStore::put_text`]
    /// with the session ID as URI and an `"entry"` tag. Also writes a
    /// [`MemoryCard`] for graph-level queries.
    ///
    /// Returns the frame ID of the persisted text frame.
    pub fn put_entry(&self, entry: &MemoryEntry) -> Result<u64> {
        let json = serde_json::to_string(entry).map_err(|e| {
            anyhow::anyhow!("Failed to serialize MemoryEntry: {e}")
        })?;

        let opts = memvid_core::PutOptions::builder()
            .uri(&entry.session_id)
            .push_tag("entry")
            .push_tag(match entry.entity_type {
                EntityType::Session => "session",
                EntityType::User => "user",
                EntityType::Knowledge => "knowledge",
            })
            .extract_triplets(true)
            .build();

        let frame_id = self.store.put_text(&json, opts)?;

        let card_result = MemoryCardBuilder::new()
            .event()
            .entity(&entry.session_id)
            .slot("memory_entry")
            .value(&entry.content)
            .source(frame_id, Some(entry.session_id.clone()))
            .engine("zen-memory", "1")
            .build(0);

        if let Ok(card) = card_result
            && let Err(e) = self.store.put_memory_card(card)
        {
            tracing::warn!(
                session_id = %entry.session_id,
                frame_id,
                error = %e,
                "Failed to persist structured memory card for entry (frame write succeeded)"
            );
        }

        Ok(frame_id)
    }

    pub fn persist_structured_turn(
        &self,
        session_id: &str,
        _role: &str,
        content: &str,
    ) -> Result<(u64, Option<memvid_core::MemoryCardId>)> {
        let opts = memvid_core::PutOptions::builder()
            .uri(session_id)
            .push_tag("turn")
            .extract_triplets(true)
            .build();

        let frame_id = self.store.put_text(content, opts)?;
        let card_result = MemoryCardBuilder::new()
            .event()
            .entity(session_id)
            .slot("conversation")
            .value(content)
            .source(frame_id, Some(session_id.to_string()))
            .engine("zen-memory", "1")
            .build(0);

        match card_result {
            Ok(card) => match self.store.put_memory_card(card) {
                Ok(card_id) => Ok((frame_id, Some(card_id))),
                Err(e) => {
                    tracing::warn!(
                        session_id,
                        frame_id,
                        error = %e,
                        "Failed to persist structured memory card (full-text write succeeded)"
                    );
                    Ok((frame_id, None))
                }
            },
            Err(e) => {
                tracing::warn!(
                    session_id,
                    frame_id,
                    error = %e,
                    "Failed to build memory card (full-text write succeeded)"
                );
                Ok((frame_id, None))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct NotionContext {
    pub notion_id: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub importance_score: f64,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EnrichedMemory {
    pub kind: String,
    pub notion: String,
    pub slot: String,
    pub value: String,
    pub confidence: f64,
    pub entity_context: Option<NotionContext>,
}

impl EnrichedMemory {
    pub fn format_enriched(&self) -> String {
        match &self.entity_context {
            Some(ctx) => format!(
                "[{}] {}={}: {} (notion: {}, importance: {:.2})",
                self.kind, self.notion, self.slot, self.value, ctx.name, ctx.importance_score
            ),
            None => format!("[{}] {}={}: {}", self.kind, self.notion, self.slot, self.value),
        }
    }
}

impl ZenMemvidStore {
    /// Retrieve memory cards enriched with KB notion context.
    pub async fn retrieve_with_entity_context(
        &self,
        session_id: &str,
    ) -> Result<Vec<EnrichedMemory>> {
        let cards = self.store.entity_memories(session_id)?;

        let filtered: Vec<_> = cards
            .into_iter()
            .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
            .collect();

        let Some(ref graph) = self.notion_graph else {
            return Ok(filtered
                .into_iter()
                .map(|c| EnrichedMemory {
                    kind: c.kind.to_string(),
                    notion: c.entity,
                    slot: c.slot,
                    value: c.value,
                    confidence: f64::from(c.confidence.unwrap_or(1.0)),
                    entity_context: None,
                })
                .collect());
        };

        let importance_map: HashMap<String, f64> = match graph.compute_importance(100, 0.85).await {
            Ok(results) => results
                .into_iter()
                .map(|r| (r.notion_id.clone(), r.score))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "PageRank computation failed, using empty importance map");
                HashMap::new()
            }
        };

        let mut enriched = Vec::with_capacity(filtered.len());
        for card in filtered {
            let candidates = extract_candidate_names(&card.entity, &card.slot, &card.value);

            let mut entity_ctx = None;
            for name in &candidates {
                if let Ok(Some(summary)) = graph.find_entity_by_name(name).await {
                    let aliases = graph
                        .load_aliases(&summary.id)
                        .await
                        .unwrap_or_default();
                    let importance = importance_map.get(&summary.name).copied().unwrap_or(0.0);
                    entity_ctx = Some(NotionContext {
                        notion_id: summary.id,
                        name: summary.name,
                        kind: summary.kind,
                        description: summary.description,
                        importance_score: importance,
                        aliases,
                    });
                    break;
                }
            }

            enriched.push(EnrichedMemory {
                kind: card.kind.to_string(),
                notion: card.entity,
                slot: card.slot,
                value: card.value,
                confidence: f64::from(card.confidence.unwrap_or(1.0)),
                entity_context: entity_ctx,
            });
        }

        Ok(enriched)
    }
}

/// Heuristic: extracts candidate notion names from a memory card's fields.
///
/// Sources: notion field (non-session), slot field, capitalized words in value.
fn extract_candidate_names(notion: &str, slot: &str, value: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    if !notion.contains('-') && !notion.starts_with("session") && !notion.starts_with("user") {
        candidates.push(notion.to_string());
    }

    if !slot.is_empty() && slot != "conversation" {
        candidates.push(slot.to_string());
    }

    for word in value.split_whitespace() {
        let clean: String = word.chars().filter(|c| c.is_alphanumeric() || *c == '-').collect();
        if clean.len() >= 2
            && let Some(first) = clean.chars().next()
            && first.is_uppercase()
            && clean.chars().skip(1).all(|c| c.is_lowercase() || c == '-')
            && !candidates.iter().any(|c| c.eq_ignore_ascii_case(&clean))
        {
            candidates.push(clean);
        }
    }

    candidates
}

/// A structured memory entry with typed entity classification and arbitrary metadata.
///
/// Wraps the essential fields of a conversation turn or knowledge artifact so
/// callers can persist richly typed records through [`ZenMemvidStore::put_entry`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryEntry {
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub entity_type: EntityType,
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
}

/// Classification for a [`MemoryEntry`]'s provenance.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    /// A conversation turn produced during an agent session.
    Session,
    /// A fact or preference attributed to the user.
    User,
    /// A knowledge-base artifact (note, wiki page, consolidated fact).
    Knowledge,
}

/// Persists context items evicted by [`rig_compose::context::ContextPack`]
/// into the memvid store for later retrieval.
///
/// When `ContextPack::pack()` omits items due to budget or item-count
/// overflow, this hook writes them to the backing `.mv2` archive so they
/// remain queryable even after being evicted from the active context window.
pub struct MemvidDemotionHook {
    store: ZenMemvidStore,
}

impl MemvidDemotionHook {
    /// Create a hook backed by the given store.
    pub fn new(store: ZenMemvidStore) -> Self {
        Self { store }
    }

    /// Persist context items evicted by `ContextPack::pack()`.
    ///
    /// Only items with omission reason [`rig_compose::context::ContextOmissionReason::OverBudget`]
    /// or [`rig_compose::context::ContextOmissionReason::MaxItems`] are persisted.
    /// Items omitted for other reasons (e.g. score filtering) are intentionally
    /// skipped.
    ///
    /// Returns the count of items successfully persisted.
    pub fn persist_evicted(
        &self,
        session_id: &str,
        omitted: &[rig_compose::context::OmittedContextItem],
    ) -> Result<usize> {
        use rig_compose::context::ContextOmissionReason;

        let mut persisted = 0usize;
        for omitted_item in omitted {
            let should_persist = matches!(
                omitted_item.reason,
                ContextOmissionReason::OverBudget | ContextOmissionReason::MaxItems
            );
            if !should_persist {
                continue;
            }

            let mut metadata = HashMap::new();
            metadata.insert(
                "source".to_string(),
                serde_json::Value::String("demoted".to_string()),
            );
            metadata.insert(
                "original_source_id".to_string(),
                serde_json::Value::String(omitted_item.item.source_id.clone()),
            );
            metadata.insert(
                "omission_reason".to_string(),
                serde_json::to_value(&omitted_item.reason).unwrap_or(serde_json::Value::Null),
            );

            let serialized = serde_json::to_vec(&metadata).unwrap_or_default();
            let text = format!(
                "[demoted:{}] {}",
                omitted_item.item.source_id,
                String::from_utf8_lossy(&serialized)
            );

            let opts = memvid_core::PutOptions::builder()
                .uri(session_id)
                .push_tag("demoted")
                .build();

            match self.store.store.put_text(&text, opts) {
                Ok(_frame_id) => {
                    persisted += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        session_id,
                        source_id = %omitted_item.item.source_id,
                        error = %e,
                        "Failed to persist demoted context item"
                    );
                }
            }
        }

        Ok(persisted)
    }
}

/// Sliding-window conversation compactor that shadow-writes evicted turns to memvid.
///
/// Keeps at most `window_size` turns in memory. When the window overflows,
/// the oldest turns are persisted to the memvid store via
/// [`ZenMemvidStore::persist_structured_turn`] and dropped from the active
/// window. This provides Tier 4 persistence for the 4-tier progressive
/// compression strategy.
pub struct MemvidStoringCompactor {
    store: ZenMemvidStore,
    session_id: String,
    window_size: usize,
    turns: Vec<(String, String)>,
}

impl MemvidStoringCompactor {
    /// Create a compactor with the given store, session scope, and window size.
    pub fn new(store: ZenMemvidStore, session_id: String, window_size: usize) -> Self {
        Self {
            store,
            session_id,
            window_size,
            turns: Vec::new(),
        }
    }

    /// Append a conversation turn to the active window.
    pub fn append(&mut self, role: &str, content: &str) {
        self.turns.push((role.to_string(), content.to_string()));
    }

    /// Compact the active window, persisting evicted turns to memvid.
    ///
    /// Returns `(active_turns, persisted_count)` where `active_turns` is the
    /// remaining window and `persisted_count` is the number of turns written
    /// to the store.
    pub fn compact(&mut self) -> Result<(Vec<(String, String)>, usize)> {
        if self.turns.len() <= self.window_size {
            return Ok((self.turns.clone(), 0));
        }

        let evict_count = self.turns.len() - self.window_size;
        let evicted: Vec<(String, String)> = self.turns.drain(..evict_count).collect();

        let mut persisted = 0usize;
        for (role, content) in &evicted {
            match self
                .store
                .persist_structured_turn(&self.session_id, role, content)
            {
                Ok(_ids) => {
                    persisted += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %self.session_id,
                        role = %role,
                        error = %e,
                        "Failed to persist evicted turn to memvid"
                    );
                }
            }
        }

        Ok((self.turns.clone(), persisted))
    }
}

pub fn create_persist_hook(
    store: MemvidStore,
    config: MemoryConfig,
) -> MemvidPersistHook<rig_core::completion::CompletionRequest> {
    MemvidPersistHook::new(store, config)
}

pub fn default_memory_config() -> MemoryConfig {
    MemoryConfig::builder()
        .policy(WritePolicy::Raw)
        .commit_each_turn(true)
        .build()
}

pub struct ContextProjector {
    store: MemvidStore,
}

impl ContextProjector {
    pub fn new(store: MemvidStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MemvidStore {
        &self.store
    }

    pub fn project_relevant(&self, session_id: &str) -> Result<Vec<String>> {
        let cards = self.store.entity_memories(session_id)?;
        Ok(cards
            .into_iter()
            .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
            .map(|c| format!("[{}] {}={}: {}", c.kind, c.entity, c.slot, c.value))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use zen_core::notion_graph::{NotionGraphProvider, NotionSummary, ImportanceScore, SimpleNotion};

    struct MockEntityGraph {
        notions: std::collections::HashMap<String, NotionSummary>,
        importance: std::collections::HashMap<String, f64>,
    }

    impl MockEntityGraph {
        fn new() -> Self {
            Self {
                notions: std::collections::HashMap::new(),
                importance: std::collections::HashMap::new(),
            }
        }

        fn with_entity(mut self, name: &str, summary: NotionSummary, score: f64) -> Self {
            self.importance.insert(name.to_string(), score);
            self.notions.insert(name.to_string(), summary);
            self
        }
    }

    #[async_trait::async_trait]
    impl NotionGraphProvider for MockEntityGraph {
        async fn upsert_entity(&self, _entity: &SimpleNotion) -> anyhow::Result<()> {
            Ok(())
        }
        async fn insert_alias(&self, _alias: &str, _canonical_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn find_entity_by_name(&self, name: &str) -> anyhow::Result<Option<NotionSummary>> {
            Ok(self.notions.get(name).cloned())
        }
        async fn apply_confidence_decay(&self, _half_life_days: f64) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn auto_promote_entities(&self, _threshold: i64) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn compute_importance(
            &self,
            _iterations: usize,
            _damping: f64,
        ) -> anyhow::Result<Vec<ImportanceScore>> {
            Ok(self
                .importance
                .iter()
                .map(|(k, &v)| ImportanceScore {
                    notion_id: k.clone(),
                    score: v,
                })
                .collect())
        }
        async fn load_aliases(&self, _notion_id: &str) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    #[test]
    fn default_memory_config_creation() {
        let config = default_memory_config();
        let _ = config;
    }

    #[test]
    fn persist_structured_turn_creates_frame_and_card() {
        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();

        let result = store.persist_structured_turn("session-123", "user", "Hello, world!");
        assert!(result.is_ok());

        let (frame_id, card_id) = result.unwrap();
        assert!(frame_id > 0);
        assert!(card_id.is_some());

        let cards = store.store.entity_memories("session-123").unwrap();
        assert!(!cards.is_empty());
        let card = &cards[0];
        assert_eq!(card.entity, "session-123");
        assert_eq!(card.slot, "conversation");
        assert_eq!(card.value, "Hello, world!");
    }

    #[test]
    fn persist_structured_turn_enriched_options() {
        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test_enriched.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();

        let (_, _) = store
            .persist_structured_turn("sess-456", "assistant", "Response text")
            .unwrap();

        let search_result = store.store.search(memvid_core::SearchRequest {
            query: "Response text".to_string(),
            top_k: 1,
            snippet_chars: 400,
            uri: Some("sess-456".to_string()),
            scope: None,
            cursor: None,
            as_of_frame: None,
            as_of_ts: None,
            no_sketch: false,
            acl_context: None,
            acl_enforcement_mode: Default::default(),
        });
        assert!(search_result.is_ok());
        let resp = search_result.unwrap();
        assert!(!resp.hits.is_empty());
    }

    #[tokio::test]
    async fn retrieve_with_entity_context_enriches() {
        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test_enrich.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();

        store
            .persist_structured_turn("session-1", "user", "I love Rust programming")
            .unwrap();

        let mock = MockEntityGraph::new().with_entity(
            "Rust",
            NotionSummary {
                id: "ent-rust".to_string(),
                name: "Rust".to_string(),
                kind: "technology".to_string(),
                description: "A systems programming language".to_string(),
                confidence: 0.9,
            },
            0.85,
        );

        let store = store.with_notion_graph(std::sync::Arc::new(mock));
        let enriched = store
            .retrieve_with_entity_context("session-1")
            .await
            .unwrap();

        assert!(!enriched.is_empty());
        let has_ctx = enriched.iter().any(|e| e.entity_context.is_some());
        assert!(has_ctx, "expected at least one enriched memory");

        let ctx = enriched
            .iter()
            .find(|e| e.entity_context.is_some())
            .unwrap()
            .entity_context
            .as_ref()
            .unwrap();
        assert_eq!(ctx.name, "Rust");
        assert_eq!(ctx.kind, "technology");
        assert_eq!(ctx.description, "A systems programming language");
        assert!(ctx.importance_score > 0.0);
    }

    #[tokio::test]
    async fn retrieve_with_entity_context_graceful_degradation() {
        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test_grace.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();

        store
            .persist_structured_turn("session-2", "user", "Hello world")
            .unwrap();

        let enriched = store
            .retrieve_with_entity_context("session-2")
            .await
            .unwrap();

        assert!(!enriched.is_empty());
        assert!(
            enriched.iter().all(|e| e.entity_context.is_none()),
            "all memories should have None context when no provider is set"
        );
    }

    #[test]
    fn put_entry_persists_and_retrievable() {
        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test_entry.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();

        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "key".to_string(),
            serde_json::Value::String("value".to_string()),
        );

        let entry = MemoryEntry {
            session_id: "sess-entry-1".to_string(),
            role: "user".to_string(),
            content: "Test memory entry".to_string(),
            entity_type: EntityType::Session,
            metadata,
        };

        let frame_id = store.put_entry(&entry).unwrap();
        assert!(frame_id > 0);

        let search = store.store.search(memvid_core::SearchRequest {
            query: "Test memory entry".to_string(),
            top_k: 1,
            snippet_chars: 400,
            uri: Some("sess-entry-1".to_string()),
            scope: None,
            cursor: None,
            as_of_frame: None,
            as_of_ts: None,
            no_sketch: false,
            acl_context: None,
            acl_enforcement_mode: Default::default(),
        });
        assert!(search.is_ok());
        assert!(!search.unwrap().hits.is_empty());
    }

    #[test]
    fn put_entry_with_knowledge_entity_type() {
        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test_entry_knowledge.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();

        let entry = MemoryEntry {
            session_id: "sess-knowledge".to_string(),
            role: "system".to_string(),
            content: "Rust borrow checker enforces ownership".to_string(),
            entity_type: EntityType::Knowledge,
            metadata: std::collections::HashMap::new(),
        };

        let frame_id = store.put_entry(&entry).unwrap();
        assert!(frame_id > 0);
    }

    #[test]
    fn memvid_demotion_hook_persists_overbudget_items() {
        use rig_compose::context::{
            ContextItem, ContextOmissionReason, ContextSourceKind, OmittedContextItem,
        };

        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test_demotion.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();
        let hook = MemvidDemotionHook::new(store);

        let items = vec![
            OmittedContextItem {
                item: ContextItem::new(
                    ContextSourceKind::Memory,
                    "frame-1",
                    "demoted content A",
                ),
                reason: ContextOmissionReason::OverBudget,
            },
            OmittedContextItem {
                item: ContextItem::new(
                    ContextSourceKind::Memory,
                    "frame-2",
                    "demoted content B",
                ),
                reason: ContextOmissionReason::MaxItems,
            },
            OmittedContextItem {
                item: ContextItem::new(
                    ContextSourceKind::Memory,
                    "frame-3",
                    "should not persist",
                ),
                reason: ContextOmissionReason::OverBudget,
            },
        ];

        let count = hook.persist_evicted("test-session", &items).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn memvid_storing_compactor_no_eviction_under_window() {
        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test_compact_no_evict.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();
        let mut compactor = MemvidStoringCompactor::new(store, "sess-compact".to_string(), 5);

        compactor.append("user", "Hello");
        compactor.append("assistant", "Hi there");

        let (turns, persisted) = compactor.compact().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(persisted, 0);
    }

    #[test]
    fn memvid_storing_compactor_evicts_excess() {
        let dir = tempdir().unwrap();
        let memory_path = dir.path().join("test_compact_evict.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();
        let mut compactor = MemvidStoringCompactor::new(store, "sess-compact2".to_string(), 2);

        compactor.append("user", "Turn 1");
        compactor.append("assistant", "Turn 2");
        compactor.append("user", "Turn 3");
        compactor.append("assistant", "Turn 4");

        let (turns, persisted) = compactor.compact().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(persisted, 2);
        assert_eq!(turns[0].0, "user");
        assert_eq!(turns[0].1, "Turn 3");
        assert_eq!(turns[1].0, "assistant");
        assert_eq!(turns[1].1, "Turn 4");
    }

    #[test]
    fn entity_type_serialization_roundtrip() {
        let session = EntityType::Session;
        let user = EntityType::User;
        let knowledge = EntityType::Knowledge;

        let json_session = serde_json::to_string(&session).unwrap();
        assert_eq!(json_session, "\"session\"");

        let json_user = serde_json::to_string(&user).unwrap();
        assert_eq!(json_user, "\"user\"");

        let json_knowledge = serde_json::to_string(&knowledge).unwrap();
        assert_eq!(json_knowledge, "\"knowledge\"");

        let deserialized: EntityType = serde_json::from_str(&json_session).unwrap();
        assert_eq!(deserialized, EntityType::Session);
    }
}
