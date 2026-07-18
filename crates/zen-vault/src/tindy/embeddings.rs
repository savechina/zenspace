use std::sync::OnceLock;

use anyhow::Result;
use serde_json::{Value, json};
#[allow(unused_imports)]
use tracing::{info, warn};
use zen_provider::DefaultEmbeddingRouter;
#[allow(unused_imports)]
use zen_provider::EmbeddingRouter;

use crate::tools::{
    ZenTool, ZenToolError, ZenToolResult, args_schema_string, result_schema_string,
};

pub struct EmbeddingResult {
    pub dimensions: usize,
    pub embedding: Vec<f32>,
}

pub struct ComputeEmbeddings;

// ---------------------------------------------------------------------------
// Embedding router — single gateway for all provider calls
// ---------------------------------------------------------------------------

/// Lazily-initialized embedding router backed by zen-provider.
///
/// The router is created once from the project's `ZenConfig` and reused for
/// all subsequent embedding calls. If config loading fails, an empty router
/// is created so the hash fallback still works.
static EMBEDDING_ROUTER: OnceLock<DefaultEmbeddingRouter> = OnceLock::new();

#[cfg(test)]
fn get_embedding_router() -> &'static DefaultEmbeddingRouter {
    // In test mode, skip config loading (ZenPaths::detect() can hang)
    // and return an empty router so hash fallback is used
    EMBEDDING_ROUTER.get_or_init(|| {
        info!("Embedding router: test mode, using empty router (hash fallback)");
        DefaultEmbeddingRouter::with_providers(vec![])
    })
}

#[cfg(not(test))]
fn get_embedding_router() -> &'static DefaultEmbeddingRouter {
    EMBEDDING_ROUTER.get_or_init(|| {
        match zen_core::config::load_config() {
            Ok(config) => {
                let router = DefaultEmbeddingRouter::from_config(config);
                info!(
                    providers = router.list_providers().len(),
                    "Embedding router initialized from config"
                );
                router
            }
            Err(e) => {
                warn!("Failed to load config for embedding router: {e}");
                DefaultEmbeddingRouter::with_providers(vec![])
            }
        }
    })
}

/// Try to compute an embedding via zen-provider, falling back to local
/// fastembed, then to hash-based embedding.
fn compute_embedding_with_fallback(text: &str) -> Vec<f32> {
    let router = get_embedding_router();

    // 1. Try configured providers via zen-provider
    if let Ok(embedding) = router.embed_sync(text) {
        info!("ComputeEmbeddings: provider returns {}-dim", embedding.len());
        return embedding;
    }

    // 2. Try local fastembed model
    if let Some(embedding) = super::local_embedder::try_local_embed(text) {
        return embedding;
    }

    // 3. Hash-based fallback (last resort)
    info!("ComputeEmbeddings: falling back to hash-based embedding");
    hash_embedding(text)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn compute_embeddings(text: &str) -> Result<EmbeddingResult> {
    if text.trim().is_empty() {
        return Ok(EmbeddingResult {
            dimensions: 384,
            embedding: vec![0.0; 384],
        });
    }

    let embedding = compute_embedding_with_fallback(text);

    Ok(EmbeddingResult {
        dimensions: embedding.len(),
        embedding,
    })
}

fn chunk_content(content: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    let words: Vec<&str> = content.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }

    let step = chunk_size.saturating_sub(overlap);
    if step == 0 {
        return vec![content.to_string()];
    }

    words
        .chunks(chunk_size)
        .enumerate()
        .filter_map(|(i, chunk)| {
            if i == 0 || words.len() > i * step {
                Some(chunk.join(" "))
            } else {
                None
            }
        })
        .collect()
}

fn aggregate_chunks(chunks: &[String]) -> Vec<f32> {
    let dim = 384;
    let mut aggregated = vec![0.0f32; dim];

    for chunk in chunks {
        let embedding = compute_embedding_with_fallback(chunk);

        let len = embedding.len().min(dim);
        for (i, v) in embedding.iter().enumerate().take(len) {
            aggregated[i] += v;
        }
    }

    if !chunks.is_empty() {
        for v in aggregated.iter_mut() {
            *v /= chunks.len() as f32;
        }
    }
    l2_normalize(&mut aggregated);
    aggregated
}

pub fn compute_embeddings_for_text(text: &str) -> Result<Vec<f32>> {
    let chunks = chunk_content(text, 400, 80);
    if chunks.is_empty() {
        info!("compute_embeddings_for_text: no chunks");
        return Ok(vec![0.0; 384]);
    }

    if chunks.len() == 1 {
        let result = compute_embeddings(&chunks[0])?;
        Ok(result.embedding)
    } else {
        Ok(aggregate_chunks(&chunks))
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-8 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

fn hash_embedding(content: &str) -> Vec<f32> {
    const DIM: usize = 384;
    let bytes = content.as_bytes();
    let mut embedding = vec![0.0f32; DIM];

    let mut hasher1 = u64::MAX;
    let mut hasher2 = 0x811C9DC5u64;

    for (i, &byte) in bytes.iter().enumerate() {
        hasher1 = hasher1.wrapping_mul(31).wrapping_add(byte as u64);
        hasher2 = hasher2
            .wrapping_mul(37)
            .wrapping_add((byte as u64).wrapping_add(i as u64));

        let bin1 = (hasher1 % DIM as u64) as usize;
        let bin2 = (hasher2 % DIM as u64) as usize;
        let sign = if hasher1 & 1 == 0 { 1.0 } else { -1.0 };
        let weight = (hasher2 & 0xFF) as f32 / 255.0 * 2.0 - 1.0;

        embedding[bin1] += sign * weight;
        embedding[bin2] -= sign * weight * 0.5;
    }

    #[allow(clippy::needless_range_loop)]
    for i in 0..DIM {
        let pos_hash = (i as u64).wrapping_mul(2654435761);
        embedding[i] += ((pos_hash & 0xFF) as f32 / 255.0 - 0.5) * 0.1;
    }

    l2_normalize(&mut embedding);
    embedding
}

impl std::fmt::Debug for ComputeEmbeddings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputeEmbeddings").finish()
    }
}

impl ZenTool for ComputeEmbeddings {
    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "compute_embeddings".to_string(),
            description:
                "Compute text embeddings using LLM providers (Ollama/OpenAI) with hash fallback."
                    .to_string(),
            args_schema: args_schema_string(),
            result_schema: result_schema_string(),
        }
    }

    async fn invoke(&self, args: Value) -> ZenToolResult {
        let text = args.get("query").and_then(Value::as_str).ok_or_else(|| {
            ZenToolError::InvalidArgs("missing required field: query".to_string())
        })?;

        let result =
            compute_embeddings(text).map_err(|e| ZenToolError::ExecutionFailed(e.to_string()))?;

        let sample: Vec<f32> = result.embedding.iter().take(8).copied().collect();

        Ok(json!({
            "dimensions": result.dimensions,
            "sample": sample,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_embeddings_empty_returns_zero() {
        let result = compute_embeddings("").unwrap();
        assert_eq!(result.dimensions, 384);
        assert!(result.embedding.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn test_hash_embedding_produces_384_dim() {
        let result = compute_embeddings("hello world this is a test").unwrap();
        assert_eq!(result.dimensions, 384);
        let norm: f32 = result.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_hash_embedding_deterministic() {
        let r1 = compute_embeddings("deterministic content test").unwrap();
        let r2 = compute_embeddings("deterministic content test").unwrap();
        assert_eq!(r1.embedding, r2.embedding);
    }

    #[test]
    fn test_hash_embedding_different_content_different_vectors() {
        let r1 = compute_embeddings("alpha beta gamma delta epsilon").unwrap();
        let r2 = compute_embeddings("zeta eta theta iota kappa lambda").unwrap();
        assert!(r1.embedding != r2.embedding);
    }

    #[test]
    fn test_chunk_content_basic() {
        let content = "one two three four five six seven eight nine ten".repeat(20);
        let chunks = chunk_content(&content, 10, 2);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_chunk_content_empty() {
        let chunks = chunk_content("", 10, 2);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_tool_schema() {
        let tool = ComputeEmbeddings;
        let schema = tool.schema();
        assert_eq!(schema.name, "compute_embeddings");
        assert!(schema.description.contains("LLM"));
    }
}
