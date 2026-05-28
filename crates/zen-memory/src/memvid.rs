use std::path::PathBuf;
use anyhow::Result;
use rig_memvid::{MemvidStore, MemvidPersistHook, MemoryConfig, WritePolicy};

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

pub fn create_persist_hook(store: MemvidStore, config: MemoryConfig) -> MemvidPersistHook<rig_core::completion::CompletionRequest> {
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
}

pub struct CompactionStrategy {
    pub max_tokens: usize,
}

impl CompactionStrategy {
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens }
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
}
