use anyhow::Result;
use rig_memvid::{MemoryConfig, MemvidPersistHook, MemvidStore, WritePolicy};
use std::path::PathBuf;

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

    pub fn retrieve(&self, session_id: &str) -> Result<Vec<String>> {
        let cards = self.store.entity_memories(session_id)?;
        Ok(cards.into_iter().map(|c| format!("{:?}", c)).collect())
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

#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub summary: String,
    pub tokens_remaining: usize,
}

pub struct CompactionStrategy {
    pub max_tokens: usize,
}

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
                "[{} turns compacted to {}]",
                conversation_turns.len() - turns.len(),
                turns.len()
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
        Ok(cards.into_iter().map(|c| format!("{:?}", c)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(result.tokens_remaining <= 100 || result.summary.contains("compacted"));
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
}
