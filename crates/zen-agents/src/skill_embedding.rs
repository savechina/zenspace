//! On-disk embedding cache and a cache-backed [`crate::skill_hit_router::SkillScorer`]
//! (Voyager pattern: skill reuse keyed by meaning rather than wording).
//!
//! # Dormant by design (T075 / T134)
//!
//! [`EmbeddingSkillScorer`] and [`SkillHitRouter::with_scorer`] are **dormant**:
//! embedding work must stay OFF the per-turn routing path (T075), so the scorer
//! is deliberately not wired into production routing and `with_scorer` is
//! therefore test-only. A reader must not mistake this module for delivered
//! functionality.
//!
//! Activation requires a *non-per-turn* call site — session start or skill
//! discovery — where the caller computes a query embedding and keeps skill
//! embeddings fresh in the cache, then hands both to [`EmbeddingSkillScorer`].
//! Until that wiring exists the router stays purely lexical, so no embedding
//! runtime is pulled into turn handling (see `skill_hit_router` module docs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::skill_hit_router::SkillScorer;
use crate::skill_loader::SkillDefinition;

/// One skill's cached embedding plus the content hash it was computed from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedEmbedding {
    pub content_hash: String,
    pub embedding: Vec<f32>,
}

/// Persistent skill→embedding map (JSON). A corrupt file degrades to empty so
/// a bad write can never block routing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillEmbeddingCache {
    pub entries: HashMap<String, CachedEmbedding>,
}

impl SkillEmbeddingCache {
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str(&raw) {
            Ok(cache) => cache,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "skill embedding cache corrupt — starting empty");
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create cache dir {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize skill embedding cache")?;
        std::fs::write(path, json).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    /// Store (or refresh) a skill embedding.
    pub fn upsert(&mut self, skill: &str, content_hash: String, embedding: Vec<f32>) {
        self.entries.insert(
            skill.to_string(),
            CachedEmbedding {
                content_hash,
                embedding,
            },
        );
    }

    /// Cached embedding for `skill`, but only when it still matches the
    /// skill's current content hash (stale entries are ignored).
    pub fn get_fresh(&self, skill: &str, content_hash: &str) -> Option<&[f32]> {
        let entry = self.entries.get(skill)?;
        (entry.content_hash == content_hash && !entry.embedding.is_empty())
            .then_some(entry.embedding.as_slice())
    }
}

/// Default cache location next to the skills themselves.
pub fn cache_path(skills_dir: &Path) -> PathBuf {
    skills_dir.join(".embeddings.json")
}

/// Stable content hash (FNV-1a, 64-bit) for cache invalidation. Chosen over a
/// crypto digest because stability across runs is the only requirement and it
/// adds no dependency.
pub fn content_hash(content: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Cosine similarity, `0.0` when either vector is empty or non-finite.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(0.0, 1.0)
}

/// Scores skills by cosine similarity between a query embedding and cached
/// skill-derived embeddings. `content_hashes` maps skill name → current
/// content hash so stale cache entries are skipped (score `None`).
pub struct EmbeddingSkillScorer {
    pub query_embedding: Vec<f32>,
    pub cache: SkillEmbeddingCache,
    pub content_hashes: HashMap<String, String>,
}

impl SkillScorer for EmbeddingSkillScorer {
    fn score_skill(&self, _query_norm: &str, skill: &SkillDefinition) -> Option<f32> {
        let hash = self.content_hashes.get(&skill.name)?;
        let embedding = self.cache.get_fresh(&skill.name, hash)?;
        Some(cosine(&self.query_embedding, embedding))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn skill(name: &str) -> SkillDefinition {
        SkillDefinition {
            name: name.to_string(),
            description: format!("{name} description"),
            triggers: vec!["t".to_string()],
            auto_route: None,
            tools: Vec::new(),
            context_files: Vec::new(),
            prompt: String::new(),
            body: String::new(),
        }
    }

    #[test]
    fn cache_roundtrip_and_stale_invalidation() {
        let dir = TempDir::new().unwrap();
        let path = cache_path(dir.path());
        let mut cache = SkillEmbeddingCache::load(&path);
        assert!(cache.get_fresh("weekly-review", "h1").is_none());

        cache.upsert("weekly-review", "h1".to_string(), vec![1.0, 0.0, 0.0]);
        cache.save(&path).unwrap();

        let reloaded = SkillEmbeddingCache::load(&path);
        assert_eq!(
            reloaded.get_fresh("weekly-review", "h1"),
            Some([1.0, 0.0, 0.0].as_slice())
        );
        assert!(
            reloaded.get_fresh("weekly-review", "h2").is_none(),
            "content change invalidates the cached embedding"
        );
    }

    #[test]
    fn corrupt_cache_loads_empty() {
        let dir = TempDir::new().unwrap();
        let path = cache_path(dir.path());
        std::fs::write(&path, "not json").unwrap();
        assert!(SkillEmbeddingCache::load(&path).entries.is_empty());
    }

    #[test]
    fn content_hash_is_stable_and_content_sensitive() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
    }

    #[test]
    fn cosine_bounds_and_degenerate_inputs() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[], &[1.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0, "length mismatch");
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0, "zero norm");
    }

    #[test]
    fn scorer_scores_only_fresh_entries() {
        let mut cache = SkillEmbeddingCache::default();
        cache.upsert("related", "h1".to_string(), vec![1.0, 0.0]);
        cache.upsert("stale", "old".to_string(), vec![1.0, 0.0]);
        let scorer = EmbeddingSkillScorer {
            query_embedding: vec![1.0, 0.0],
            cache,
            content_hashes: HashMap::from([
                ("related".to_string(), "h1".to_string()),
                ("stale".to_string(), "new".to_string()),
                ("missing".to_string(), "h1".to_string()),
            ]),
        };

        assert_eq!(scorer.score_skill("q", &skill("related")), Some(1.0));
        assert_eq!(scorer.score_skill("q", &skill("stale")), None);
        assert_eq!(scorer.score_skill("q", &skill("missing")), None);
    }
}
