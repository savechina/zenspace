use anyhow::Result;
use memvid_core::MemoryCardBuilder;
use rig_memvid::memvid_core;
use rig_memvid::{MemoryConfig, MemvidPersistHook, MemvidStore, WritePolicy};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Minimum confidence threshold for auto-extracted triplets (D9).
/// Cards from `extract_triplets` below this threshold are filtered out on retrieval.
/// The RulesEngine typically produces 0.7-0.9 confidence; we keep >=0.8.
pub const TRIPLET_MIN_CONFIDENCE: f32 = 0.8;

pub struct ZenMemvidStore {
    store: MemvidStore,
}

impl ZenMemvidStore {
    pub fn new(memory_path: PathBuf) -> Result<Self> {
        let store = MemvidStore::builder()
            .path(memory_path)
            .enable_lex()
            .open_or_create()?;

        Ok(Self { store })
    }

    pub fn into_inner(self) -> MemvidStore {
        self.store
    }

    pub fn store(&self) -> &MemvidStore {
        &self.store
    }

    pub fn from_store(store: MemvidStore) -> Self {
        Self { store }
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
    /// from the text write. The card enables entity/slot/value graph queries.
    ///
    /// # Arguments
    /// * `session_id` - The session scope for this turn
    /// * `role` - The speaker role ("user" or "assistant")
    /// * `content` - The turn content to persist
    ///
    /// # Returns
    /// `Ok((frame_id, card_id))` on success. If `put_memory_card()` fails, a warning
    /// is logged but the frame_id from the text write is still returned.
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
pub struct EntityContext {
    pub entity_id: String,
    pub name: String,
    pub entity_type: String,
    pub description: String,
    pub importance_score: f64,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EnrichedMemory {
    pub kind: String,
    pub entity: String,
    pub slot: String,
    pub value: String,
    pub confidence: f64,
    pub entity_context: Option<EntityContext>,
}

impl EnrichedMemory {
    pub fn format_enriched(&self) -> String {
        match &self.entity_context {
            Some(ctx) => format!(
                "[{}] {}={}: {} (entity: {}, importance: {:.2})",
                self.kind, self.entity, self.slot, self.value, ctx.name, ctx.importance_score
            ),
            None => format!("[{}] {}={}: {}", self.kind, self.entity, self.slot, self.value),
        }
    }
}

impl ZenMemvidStore {
    /// Retrieve memory cards enriched with KB entity context.
    ///
    /// For each card, attempts to resolve its entity name against the KB
    /// entity graph (via `EntitiesRepo::find_entity_by_name()`). On match,
    /// loads description, aliases, and PageRank importance score.
    ///
    /// **Graceful degradation**: if `state_db_path` doesn't exist or can't be
    /// opened, returns cards with `entity_context: None` — the bridge is a
    /// bonus, not a hard requirement.
    ///
    /// # Arguments
    /// * `session_id` - Session scope for memory retrieval
    /// * `state_db_path` - Path to state.db (KB entity graph)
    pub async fn retrieve_with_entity_context(
        &self,
        session_id: &str,
        state_db_path: &Path,
    ) -> Result<Vec<EnrichedMemory>> {
        let cards = self.store.entity_memories(session_id)?;

        let filtered: Vec<_> = cards
            .into_iter()
            .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
            .collect();

        if !state_db_path.exists() {
            return Ok(filtered
                .into_iter()
                .map(|c| EnrichedMemory {
                    kind: c.kind.to_string(),
                    entity: c.entity,
                    slot: c.slot,
                    value: c.value,
                    confidence: f64::from(c.confidence.unwrap_or(1.0)),
                    entity_context: None,
                })
                .collect());
        }

        let client = match zen_repo::SqliteClient::open(state_db_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    path = %state_db_path.display(),
                    error = %e,
                    "Failed to open state.db for entity enrichment, falling back to plain retrieval"
                );
                return Ok(filtered
                    .into_iter()
                    .map(|c| EnrichedMemory {
                        kind: c.kind.to_string(),
                        entity: c.entity,
                        slot: c.slot,
                        value: c.value,
                        confidence: f64::from(c.confidence.unwrap_or(1.0)),
                        entity_context: None,
                    })
                    .collect());
            }
        };

        let repo = zen_repo::EntitiesRepo::new(&client);

        let importance_map: HashMap<String, f64> = match repo.compute_importance(100, 0.85).await {
            Ok(results) => results.into_iter().map(|r| (r.entity, r.score)).collect(),
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
                if let Ok(Some(entity)) = repo.find_entity_by_name(name).await {
                    let aliases = repo
                        .load_aliases_for_entity(&entity.id)
                        .await
                        .unwrap_or_default();
                    let importance = importance_map.get(&entity.name).copied().unwrap_or(0.0);
                    entity_ctx = Some(EntityContext {
                        entity_id: entity.id,
                        name: entity.name,
                        entity_type: entity.entity_type,
                        description: entity.description,
                        importance_score: importance,
                        aliases,
                    });
                    break;
                }
            }

            enriched.push(EnrichedMemory {
                kind: card.kind.to_string(),
                entity: card.entity,
                slot: card.slot,
                value: card.value,
                confidence: f64::from(card.confidence.unwrap_or(1.0)),
                entity_context: entity_ctx,
            });
        }

        Ok(enriched)
    }
}

/// Heuristic: extracts candidate entity names from a memory card's fields.
///
/// Sources: entity field (non-session), slot field, capitalized words in value.
fn extract_candidate_names(entity: &str, slot: &str, value: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    if !entity.contains('-') && !entity.starts_with("session") && !entity.starts_with("user") {
        candidates.push(entity.to_string());
    }

    if !slot.is_empty() && slot != "conversation" {
        candidates.push(slot.to_string());
    }

    for word in value.split_whitespace() {
        let clean: String = word.chars().filter(|c| c.is_alphanumeric() || *c == '-').collect();
        if clean.len() >= 2 {
            let mut chars = clean.chars();
            if let Some(first) = chars.next() {
                if first.is_uppercase() && chars.all(|c| c.is_lowercase() || c == '-') {
                    if !candidates.iter().any(|c| c.eq_ignore_ascii_case(&clean)) {
                        candidates.push(clean);
                    }
                }
            }
        }
    }

    candidates
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

        let db_path = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&db_path).await.unwrap();
        let repo = zen_repo::EntitiesRepo::new(&client);
        let now = chrono::Utc::now().to_rfc3339();
        repo.insert_entity_with(
            "ent-rust",
            "Rust",
            "technology",
            &now,
            "A systems programming language",
            "test",
            0.9,
        )
        .await
        .unwrap();

        let enriched = store
            .retrieve_with_entity_context("session-1", &db_path)
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
        assert_eq!(ctx.entity_type, "technology");
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

        let fake_path = dir.path().join("nonexistent.db");
        let enriched = store
            .retrieve_with_entity_context("session-2", &fake_path)
            .await
            .unwrap();

        assert!(!enriched.is_empty());
        assert!(
            enriched.iter().all(|e| e.entity_context.is_none()),
            "all memories should have None context when DB is missing"
        );
    }
}
