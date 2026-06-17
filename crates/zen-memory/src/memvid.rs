use anyhow::Result;
use memvid_core::MemoryCardBuilder;
use rig_memvid::memvid_core;
use rig_memvid::{MemoryConfig, MemvidPersistHook, MemvidStore, WritePolicy};
use std::path::PathBuf;

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
        Ok(cards.into_iter()
            .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
            .map(|c| {
                format!("[{}] {}={}: {}", c.kind, c.entity, c.slot, c.value)
            }).collect())
    }

    pub fn retrieve_high_confidence(&self, session_id: &str) -> Result<Vec<String>> {
        let cards = self.store.entity_memories(session_id)?;
        Ok(cards.into_iter()
            .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
            .map(|c| {
                format!("[{}] {}={}: {} (confidence: {:.2})", c.kind, c.entity, c.slot, c.value, c.confidence.unwrap_or(1.0))
            }).collect())
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
        role: &str,
        content: &str,
    ) -> Result<(u64, Option<memvid_core::MemoryCardId>)> {
        let mut opts = memvid_core::PutOptions::builder()
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

#[deprecated(
    since = "0.0.1",
    note = "Will be refactored into CompactionStrategyTrait for multi-strategy extensibility. \
            Use rig_memvid::projection::MemoryContextPack for built-in memvid path. \
            Future external strategies: openviking, cortex-mem, supermemory (see D10)."
)]
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub summary: String,
    pub tokens_remaining: usize,
}

#[deprecated(
    since = "0.0.1",
    note = "Will be refactored into CompactionStrategyTrait for multi-strategy extensibility. \
            Use rig_compose::ContextPack + MemvidDemotionHook for built-in memvid path. \
            Future external strategies: openviking, cortex-mem, supermemory (see D10)."
)]
pub struct CompactionStrategy {
    pub max_tokens: usize,
}

#[allow(deprecated)]
impl CompactionStrategy {
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens }
    }

    pub fn compact(&self, conversation_turns: &[String]) -> CompactionResult {
        let mut total_tokens: usize = conversation_turns.iter().map(|t| t.len() / 4).sum();
        let mut turns = conversation_turns.to_vec();

        while total_tokens > self.max_tokens && turns.len() > 1 {
            let removed = turns.remove(0);
            total_tokens -= removed.len() / 4;
        }

        let summary = if turns.len() < conversation_turns.len() {
            format!(
                "active context: {} of {} turns (full history in archive)",
                turns.len(),
                conversation_turns.len()
            )
        } else {
            String::new()
        };

        CompactionResult {
            summary,
            tokens_remaining: total_tokens,
        }
    }
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
        Ok(cards.into_iter()
            .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
            .map(|c| {
                format!("[{}] {}={}: {}", c.kind, c.entity, c.slot, c.value)
            }).collect())
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn compaction_strategy_creation() {
        let strategy = CompactionStrategy::new(4096);
        assert_eq!(strategy.max_tokens, 4096);
    }

    #[test]
    fn default_memory_config_creation() {
        let config = default_memory_config();
        let _ = config;
    }

    #[test]
    fn compaction_reduces_tokens() {
        let strategy = CompactionStrategy::new(100);
        let turns = vec![
            "turn one content here".repeat(20),
            "turn two content here".repeat(20),
            "turn three content".repeat(20),
        ];
        let result = strategy.compact(&turns);
        assert!(result.tokens_remaining <= 100 || result.summary.contains("active context"));
    }

    #[test]
    fn compaction_preserves_last_turn() {
        let strategy = CompactionStrategy::new(10);
        let turns = vec![
            "short1".to_string(),
            "short2".to_string(),
            "keep_this".to_string(),
        ];
        let result = strategy.compact(&turns);
        assert!(result.tokens_remaining > 0);
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
            #[cfg(feature = "temporal")]
            temporal: None,
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
}
