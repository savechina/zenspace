//! In-memory store for Zen agent memories.
//!
//! Provides basic memory storage and retrieval for agent conversation history
//! and knowledge retention. Will be replaced by rig-memvid in Phase 7.

use std::cmp::Reverse;
use std::sync::{Arc, Mutex};

use anyhow::Result;

/// Memory entry stored in the in-memory store.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub text: String,
    pub tags: Vec<String>,
    pub scope: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// In-memory store for Zen agent memories.
pub struct MemoryStore {
    entries: Arc<Mutex<Vec<MemoryEntry>>>,
    max_entries: usize,
}

impl MemoryStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            max_entries,
        }
    }

    pub fn write_memory(&self, text: &str, tags: Vec<&str>, scope: Option<&str>) -> Result<u64> {
        let mut entries = self.entries.lock().unwrap();

        let entry = MemoryEntry {
            text: text.to_string(),
            tags: tags.into_iter().map(|s| s.to_string()).collect(),
            scope: scope.map(|s| s.to_string()),
            created_at: chrono::Utc::now(),
        };

        entries.push(entry);

        if entries.len() > self.max_entries {
            let drain_to = entries.len() - self.max_entries;
            entries.drain(0..drain_to);
        }

        Ok((entries.len() - 1) as u64)
    }

    pub fn search_memories(&self, query: &str, top_k: usize) -> Result<Vec<String>> {
        let entries = self.entries.lock().unwrap();

        let query_lower = query.to_lowercase();
        let mut results: Vec<(usize, &MemoryEntry)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.text.to_lowercase().contains(&query_lower))
            .collect();

        results.sort_by_key(|b| Reverse(b.1.created_at));

        let results: Vec<String> = results
            .into_iter()
            .take(top_k)
            .map(|(_, e)| e.text.clone())
            .collect();

        Ok(results)
    }

    pub fn stats(&self) -> Result<MemoryStats> {
        let entries = self.entries.lock().unwrap();
        Ok(MemoryStats {
            entry_count: entries.len(),
            max_entries: self.max_entries,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub entry_count: usize,
    pub max_entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_store_creation() {
        let store = MemoryStore::new(100);
        let stats = store.stats().unwrap();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.max_entries, 100);
    }

    #[test]
    fn test_write_and_search_memory() {
        let store = MemoryStore::new(100);

        store
            .write_memory(
                "Alice prefers Rust over Python for systems programming.",
                vec!["preference", "user_profile"],
                Some("user/alice"),
            )
            .unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.entry_count, 1);

        let results = store.search_memories("Rust", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].contains("Rust"));
    }

    #[test]
    fn test_memory_trim() {
        let store = MemoryStore::new(3);

        store.write_memory("Memory 1", vec![], None).unwrap();
        store.write_memory("Memory 2", vec![], None).unwrap();
        store.write_memory("Memory 3", vec![], None).unwrap();
        store.write_memory("Memory 4", vec![], None).unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.entry_count, 3);
    }
}
