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
    pub degraded: bool,
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
    EMBEDDING_ROUTER.get_or_init(|| match zen_core::config::load_config() {
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
    })
}

/// Active embedding engine `(backend, model)`; `None` when the router is
/// empty (test env / config load failure).
pub fn embedding_engine_descriptor() -> Option<(String, String)> {
    get_embedding_router().engine_descriptor()
}

/// System-wide embedding dimension. All vectors are padded to this
/// dimension before insert / search to support multiple providers with
/// different native dimensionalities.
/// Migration-locked: the stored vec0 `FLOAT[4096]` DDL is fixed in zen-repo
/// migrations 002 and the reset path, so changing this value requires a vec0
/// reset (drop/recreate via `zen wiki reindex --reset-embeddings`).
pub const SYSTEM_EMBEDDING_DIM: usize = 4096;

/// Pad or trim a vector to the target dimension.
/// If the vector is shorter, trailing zeros are appended.
/// If it is longer, the first `target` elements are kept.
pub fn pad_to_dim(v: &[f32], target: usize) -> Vec<f32> {
    let mut out = v.to_vec();
    out.resize(target, 0.0);
    out
}

fn compute_embedding_with_fallback(text: &str) -> (Vec<f32>, bool) {
    let router = get_embedding_router();

    if let Ok(embedding) = router.embed_sync(text) {
        // Degenerate provider output must never reach vec0: a zero-norm or
        // non-finite vector inserts successfully but poisons L2-distance KNN
        // (HTTP-200 zero vectors, ollama#17878 class).
        if embedding.iter().any(|v| !v.is_finite())
            || embedding.iter().map(|v| v * v).sum::<f32>() < 1e-12
        {
            warn!(
                "degenerate embedding (zero-norm or non-finite); falling back to hash — see ollama#17878 class"
            );
            return (hash_embedding(text), true);
        }
        info!(
            "ComputeEmbeddings: provider returns {}-dim",
            embedding.len()
        );
        let mut v = pad_to_dim(&embedding, SYSTEM_EMBEDDING_DIM);
        l2_normalize(&mut v);
        return (v, false);
    }

    warn!("ComputeEmbeddings: provider embedding failed, using hash fallback (degraded)");
    (hash_embedding(text), true)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn compute_embeddings(text: &str) -> Result<EmbeddingResult> {
    if text.trim().is_empty() {
        return Ok(EmbeddingResult {
            dimensions: SYSTEM_EMBEDDING_DIM,
            embedding: vec![0.0; SYSTEM_EMBEDDING_DIM],
            degraded: false,
        });
    }

    let (embedding, degraded) = compute_embedding_with_fallback(text);

    Ok(EmbeddingResult {
        dimensions: embedding.len(),
        embedding,
        degraded,
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

/// Average the per-chunk embeddings into one vector and report whether ANY
/// chunk fell back to the hash provider.
///
/// Aggregation math (mean of the per-chunk vectors, then L2-normalize) is
/// byte-identical to the pre-tagged implementation; the only addition is the
/// `degraded` union so a mixed run — some chunks real, some hash — is still
/// reported as degraded.
fn aggregate_chunks_tagged(chunks: &[String]) -> (Vec<f32>, bool) {
    let dim = SYSTEM_EMBEDDING_DIM;
    let mut aggregated = vec![0.0f32; dim];
    let mut degraded = false;

    for chunk in chunks {
        let (embedding, chunk_degraded) = compute_embedding_with_fallback(chunk);
        degraded |= chunk_degraded;

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
    (aggregated, degraded)
}

/// Compute an embedding for `text` and report whether the hash fallback was used.
///
/// # Description
/// Splits `text` at the shared 400-word / 80-word-overlap boundary and either
/// embeds the single chunk via [`compute_embeddings`] or averages the
/// per-chunk vectors via [`aggregate_chunks_tagged`]. The returned flag is
/// `true` when ANY contributing chunk was served by the byte-hash
/// pseudo-embedding (the provider chain was exhausted) instead of a real
/// semantic model.
///
/// # Parameters
/// - `text`: source text; whitespace-only input yields a zero vector.
///
/// # Returns
/// `Ok((embedding, degraded))` where `embedding` is always
/// `SYSTEM_EMBEDDING_DIM` long and `degraded` is `true` iff the hash fallback
/// contributed to it.
///
/// # Errors
/// Never fails today — the provider chain ends in the infallible hash
/// fallback — but the `Result` is preserved for parity with the untagged
/// wrapper and future providers.
///
/// # Example
/// ```
/// let (vector, _degraded) =
///     zen_vault::tindy::embeddings::compute_embeddings_for_text_tagged("hello").unwrap();
/// assert_eq!(vector.len(), 4096);
/// ```
pub fn compute_embeddings_for_text_tagged(text: &str) -> Result<(Vec<f32>, bool)> {
    let chunks = chunk_content(text, 400, 80);
    if chunks.is_empty() {
        info!("compute_embeddings_for_text: no chunks");
        return Ok((vec![0.0; SYSTEM_EMBEDDING_DIM], false));
    }

    if chunks.len() == 1 {
        let result = compute_embeddings(&chunks[0])?;
        Ok((result.embedding, result.degraded))
    } else {
        Ok(aggregate_chunks_tagged(&chunks))
    }
}

/// Compute an embedding for `text`, dropping the degradation flag.
///
/// # Description
/// Thin wrapper over [`compute_embeddings_for_text_tagged`] that keeps the
/// original signature for existing callers (search tiers, notion service,
/// gateway warm-up). Behaviour is byte-identical to the pre-tagged
/// implementation.
///
/// # Parameters
/// - `text`: source text.
///
/// # Returns
/// `Ok(embedding)` — a `SYSTEM_EMBEDDING_DIM`-long vector.
///
/// # Errors
/// See [`compute_embeddings_for_text_tagged`].
///
/// # Example
/// ```
/// let vector = zen_vault::tindy::compute_embeddings_for_text("hello").unwrap();
/// assert_eq!(vector.len(), 4096);
/// ```
pub fn compute_embeddings_for_text(text: &str) -> Result<Vec<f32>> {
    compute_embeddings_for_text_tagged(text).map(|(embedding, _degraded)| embedding)
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
    const DIM: usize = SYSTEM_EMBEDDING_DIM;
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
            "degraded": result.degraded,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_embeddings_empty_returns_zero() {
        let result = compute_embeddings("").unwrap();
        assert_eq!(result.dimensions, SYSTEM_EMBEDDING_DIM);
        assert!(result.embedding.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn test_compute_embeddings_for_text_is_fail_open() {
        // Pins the daemon embedding-warmup contract: this never errors (hash
        // fallback), so a failed warmup can only warn, never abort startup.
        let v = compute_embeddings_for_text("zen embedding warmup").unwrap();
        assert_eq!(v.len(), SYSTEM_EMBEDDING_DIM);
    }

    #[test]
    fn test_hash_embedding_produces_system_dim() {
        let result = compute_embeddings("hello world this is a test").unwrap();
        assert_eq!(result.dimensions, SYSTEM_EMBEDDING_DIM);
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
