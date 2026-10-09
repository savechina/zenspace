use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use zen_core::types::ComplexityLevel;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub name: String,
    pub provider: String,
    pub context_window: usize,
    pub input_cost_per_million: f64,
    pub output_cost_per_million: f64,
    pub capabilities: Vec<String>,
    pub is_local: bool,
}

impl ModelMetadata {
    pub fn capability_score(&self) -> usize {
        self.context_window / 1000 + self.capabilities.len() * 10
    }

    pub fn total_cost(&self) -> f64 {
        self.input_cost_per_million + self.output_cost_per_million
    }

    pub fn cost_efficiency(&self) -> f64 {
        let cap = self.capability_score().max(1) as f64;
        let cost = self.total_cost().max(0.001);
        cap / cost
    }
}

/// Convert reported token usage into USD cost using a model's pricing.
///
/// # Parameters
/// - `metadata` — the model's pricing (`input_cost_per_million` /
///   `output_cost_per_million`, USD per 1M tokens) and locality flag.
/// - `input_tokens` — prompt/input tokens consumed by the call(s).
/// - `output_tokens` — completion/output tokens produced by the call(s).
///
/// # Returns
/// The cost in USD: `in/1M × input_price + out/1M × output_price`, with
/// negative prices clamped to 0 (a pricing typo must never produce a
/// negative cost that cancels real spend).
///
/// # Local models
/// `metadata.is_local == true` returns exactly `0.0` regardless of the
/// pricing fields — local inference (e.g. Ollama) has no per-token cost, and
/// the structural zero keeps the scheduler cost cap from ever tripping on a
/// local-first deployment even if stale pricing is configured.
///
/// # Example
/// ```
/// use zen_provider::model_meta::{ModelMetadata, usage_to_cost_usd};
/// let meta = ModelMetadata {
///     name: "metered".into(),
///     provider: "openai".into(),
///     context_window: 0,
///     input_cost_per_million: 1.0,
///     output_cost_per_million: 2.0,
///     capabilities: vec![],
///     is_local: false,
/// };
/// assert!((usage_to_cost_usd(&meta, 500_000, 250_000) - 1.0).abs() < 1e-9);
/// ```
pub fn usage_to_cost_usd(metadata: &ModelMetadata, input_tokens: u64, output_tokens: u64) -> f64 {
    if metadata.is_local {
        return 0.0;
    }
    let input = (input_tokens as f64 / 1_000_000.0) * metadata.input_cost_per_million.max(0.0);
    let output = (output_tokens as f64 / 1_000_000.0) * metadata.output_cost_per_million.max(0.0);
    input + output
}

/// Anthropic 5-minute cache-write price, as a multiple of the base input
/// token price (1.25x). Source: Anthropic prompt-caching pricing table.
///
/// The 1-hour TTL tier is deliberately absent: zen only ever sends
/// `cache_control: {"type": "ephemeral"}` with **no** `ttl` field, and the
/// API documents 5 minutes as the default. A future explicit-TTL feature must
/// split this constant per TTL rather than silently billing 1h writes at the
/// 5m rate.
pub const CACHE_WRITE_MULTIPLIER: f64 = 1.25;

/// Anthropic cache-read price, as a multiple of the base input token price
/// (0.1x). A cache hit is **not** free — it is billed at a tenth of base.
/// The 5.1 and Opus 5.5 families use cheaper tiers (0.025x / 0.05x); those
/// are covered by the per-model pricing table, not here.
pub const CACHE_READ_MULTIPLIER: f64 = 0.1;

/// The four disjoint buckets of one cached completion's token usage.
///
/// **These must never be summed into `input_tokens` before being split.**
/// Anthropic's API reference is explicit: *"Total input tokens for a request
/// are calculated by summing `input_tokens`, `cache_creation_input_tokens`,
/// and `cache_read_input_tokens`."* Each field counts a **disjoint** slice —
/// `input_tokens` is only the *uncached remainder*. Subtracting one bucket
/// from another double-counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CachedTokenUsage {
    /// Uncached prompt tokens (`usage.input_tokens`).
    pub uncached_input_tokens: u64,
    /// Prompt tokens written to cache (`usage.cache_creation_input_tokens`).
    pub cache_write_tokens: u64,
    /// Prompt tokens served from cache (`usage.cache_read_input_tokens`).
    pub cache_read_tokens: u64,
    /// Completion tokens (`usage.output_tokens`).
    pub output_tokens: u64,
}

impl CachedTokenUsage {
    /// Every token the request processed, across all four buckets.
    ///
    /// This is the **throughput** figure (context consumed, budget ceiling),
    /// not the cost figure — a cache read costs 0.1x but still occupies
    /// context and still consumed provider work. Cost lives in
    /// [`cached_usage_to_cost_usd`], which weights each bucket.
    pub fn total_tokens(&self) -> u64 {
        self.uncached_input_tokens
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.output_tokens)
    }

    /// Total *input* tokens across the three input buckets (excludes output).
    pub fn total_input_tokens(&self) -> u64 {
        self.uncached_input_tokens
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
    }

    /// True when the echo carried no token data at all, so the caller should
    /// fall back to its estimate rather than bill a structural zero.
    pub fn is_empty(&self) -> bool {
        self.total_tokens() == 0
    }
}

/// Convert a cached completion's four usage buckets into USD.
///
/// Unlike [`usage_to_cost_usd`], this weights each input bucket by its
/// prompt-cache price: uncached at base, cache writes at
/// [`CACHE_WRITE_MULTIPLIER`]×, cache reads at [`CACHE_READ_MULTIPLIER`]×.
///
/// # Errors
/// Never — negative prices are clamped to 0, and local models return `0.0`.
///
/// # Example
/// ```
/// use zen_provider::model_meta::{CachedTokenUsage, ModelMetadata, cached_usage_to_cost_usd};
/// let meta = ModelMetadata {
///     name: "m".into(), provider: "anthropic".into(), context_window: 0,
///     input_cost_per_million: 4.0, output_cost_per_million: 20.0,
///     capabilities: vec![], is_local: false,
/// };
/// // 1000 uncached + 200 writes (1.25x) + 800 reads (0.1x) + 50 out.
/// let cost = cached_usage_to_cost_usd(&meta, &CachedTokenUsage {
///     uncached_input_tokens: 1000, cache_write_tokens: 200,
///     cache_read_tokens: 800, output_tokens: 50,
/// });
/// // 1000/1M*4 + 200/1M*4*1.25 + 800/1M*4*0.1 + 50/1M*20 = 0.00632
/// assert!((cost - 0.00632).abs() < 1e-12, "got {cost}");
/// ```
pub fn cached_usage_to_cost_usd(metadata: &ModelMetadata, usage: &CachedTokenUsage) -> f64 {
    if metadata.is_local {
        return 0.0;
    }
    let base = metadata.input_cost_per_million.max(0.0);
    let uncached = (usage.uncached_input_tokens as f64) * base;
    let write = (usage.cache_write_tokens as f64) * base * CACHE_WRITE_MULTIPLIER;
    let read = (usage.cache_read_tokens as f64) * base * CACHE_READ_MULTIPLIER;
    let output = (usage.output_tokens as f64) * metadata.output_cost_per_million.max(0.0);
    (uncached + write + read + output) / 1_000_000.0
}

pub struct ModelRouter {
    models: Arc<RwLock<HashMap<String, String>>>,
    metadata: Arc<RwLock<HashMap<String, ModelMetadata>>>,
    default_model: String,
}

impl ModelRouter {
    pub fn new(default_model: &str) -> Self {
        Self {
            models: Arc::new(RwLock::new(HashMap::new())),
            metadata: Arc::new(RwLock::new(HashMap::new())),
            default_model: default_model.to_string(),
        }
    }

    pub async fn register_model(&self, name: &str, metadata: ModelMetadata) {
        self.models
            .write()
            .await
            .insert(name.to_string(), name.to_string());
        self.metadata
            .write()
            .await
            .insert(name.to_string(), metadata);
    }

    pub async fn route_task(&self, complexity: ComplexityLevel) -> anyhow::Result<String> {
        let metadata = self.metadata.read().await;

        if metadata.is_empty() {
            return Ok(self.default_model.clone());
        }

        let target = match complexity {
            ComplexityLevel::Simple => metadata.iter().min_by(|(_, a), (_, b)| {
                let cost_cmp = a
                    .total_cost()
                    .partial_cmp(&b.total_cost())
                    .unwrap_or(std::cmp::Ordering::Equal);
                if cost_cmp != std::cmp::Ordering::Equal {
                    return cost_cmp;
                }
                b.is_local.cmp(&a.is_local)
            }),
            ComplexityLevel::Standard => metadata.iter().max_by(|(_, a), (_, b)| {
                a.cost_efficiency()
                    .partial_cmp(&b.cost_efficiency())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            ComplexityLevel::Complex => metadata.iter().max_by(|(_, a), (_, b)| {
                let cap_cmp = a.capability_score().cmp(&b.capability_score());
                if cap_cmp != std::cmp::Ordering::Equal {
                    return cap_cmp;
                }
                b.total_cost()
                    .partial_cmp(&a.total_cost())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            ComplexityLevel::Critical => metadata.iter().max_by_key(|(_, m)| m.capability_score()),
        };

        Ok(target
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| self.default_model.clone()))
    }

    pub async fn get_metadata(&self, name: &str) -> Option<ModelMetadata> {
        self.metadata.read().await.get(name).cloned()
    }

    pub async fn list_models(&self) -> Vec<String> {
        self.metadata.read().await.keys().cloned().collect()
    }

    pub async fn swap_model(
        &self,
        name: &str,
        new_metadata: ModelMetadata,
    ) -> Option<ModelMetadata> {
        let mut metadata = self.metadata.write().await;
        metadata.insert(name.to_string(), new_metadata)
    }

    pub async fn remove_model(&self, name: &str) -> Option<ModelMetadata> {
        let mut models = self.models.write().await;
        let mut metadata = self.metadata.write().await;

        models.remove(name);
        metadata.remove(name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTelemetry {
    pub model: String,
    pub prompt_length: usize,
    pub response_length: usize,
    pub tokens_used: u64,
    pub latency_ms: u64,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

pub struct PromptHookTelemetry {
    records: Arc<RwLock<Vec<PromptTelemetry>>>,
    max_records: usize,
}

impl PromptHookTelemetry {
    pub fn new(max_records: usize) -> Self {
        Self {
            records: Arc::new(RwLock::new(Vec::new())),
            max_records,
        }
    }

    pub async fn record(&self, model: &str, prompt: &str, response: &str, latency_ms: u64) {
        let mut records = self.records.write().await;
        if records.len() >= self.max_records {
            records.remove(0);
        }
        records.push(PromptTelemetry {
            model: model.to_string(),
            prompt_length: prompt.len(),
            response_length: response.len(),
            tokens_used: (prompt.len() + response.len()) as u64 / 4,
            latency_ms,
            timestamp: chrono::Utc::now(),
        });
    }

    pub async fn get_stats(&self, model: &str) -> ModelStats {
        let records = self.records.read().await;
        let model_records: Vec<_> = records.iter().filter(|r| r.model == model).collect();

        if model_records.is_empty() {
            return ModelStats::default();
        }

        let total_tokens: u64 = model_records.iter().map(|r| r.tokens_used).sum();
        let avg_latency =
            model_records.iter().map(|r| r.latency_ms).sum::<u64>() / model_records.len() as u64;

        ModelStats {
            total_requests: model_records.len(),
            total_tokens,
            avg_latency_ms: avg_latency,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelStats {
    pub total_requests: usize,
    pub total_tokens: u64,
    pub avg_latency_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn model_metadata_capability_score() {
        let meta = ModelMetadata {
            name: "test".to_string(),
            provider: "mock".to_string(),
            context_window: 8192,
            input_cost_per_million: 1.0,
            output_cost_per_million: 2.0,
            capabilities: vec!["text".to_string(), "code".to_string()],
            is_local: true,
        };
        assert_eq!(meta.capability_score(), 8 + 20);
    }

    #[tokio::test]
    async fn model_router_registers_and_lists() {
        let router = ModelRouter::new("default");
        let meta = ModelMetadata {
            name: "test-model".to_string(),
            provider: "mock".to_string(),
            context_window: 4096,
            input_cost_per_million: 1.0,
            output_cost_per_million: 2.0,
            capabilities: vec!["text".to_string()],
            is_local: true,
        };

        router.register_model("test-model", meta).await;
        let models = router.list_models().await;
        assert!(models.contains(&"test-model".to_string()));
    }

    #[tokio::test]
    async fn model_hot_swap() {
        let router = ModelRouter::new("default");
        let meta1 = ModelMetadata {
            name: "v1".to_string(),
            provider: "mock".to_string(),
            context_window: 4096,
            input_cost_per_million: 1.0,
            output_cost_per_million: 2.0,
            capabilities: vec![],
            is_local: true,
        };

        router.register_model("model", meta1).await;
        assert_eq!(router.list_models().await.len(), 1);

        let meta2 = ModelMetadata {
            name: "v2".to_string(),
            provider: "mock".to_string(),
            context_window: 8192,
            input_cost_per_million: 2.0,
            output_cost_per_million: 4.0,
            capabilities: vec![],
            is_local: true,
        };

        let old = router.swap_model("model", meta2).await;
        assert!(old.is_some());
        assert_eq!(old.unwrap().name, "v1");
    }

    #[tokio::test]
    async fn telemetry_records_and_stats() {
        let telemetry = PromptHookTelemetry::new(100);
        telemetry.record("test-model", "hello", "world", 50).await;
        telemetry.record("test-model", "foo", "bar", 30).await;

        let stats = telemetry.get_stats("test-model").await;
        assert_eq!(stats.total_requests, 2);
        assert_eq!(stats.avg_latency_ms, 40);
    }

    fn cheap_local_model() -> ModelMetadata {
        ModelMetadata {
            name: "qwen3:8b".to_string(),
            provider: "ollama".to_string(),
            context_window: 4096,
            input_cost_per_million: 0.0,
            output_cost_per_million: 0.0,
            capabilities: vec!["text".to_string()],
            is_local: true,
        }
    }

    fn expensive_cloud_model() -> ModelMetadata {
        ModelMetadata {
            name: "claude-sonnet".to_string(),
            provider: "anthropic".to_string(),
            context_window: 200_000,
            input_cost_per_million: 3.0,
            output_cost_per_million: 15.0,
            capabilities: vec!["text", "code", "vision", "tool_use"]
                .into_iter()
                .map(String::from)
                .collect(),
            is_local: false,
        }
    }

    #[tokio::test]
    async fn route_simple_picks_cheapest() {
        let router = ModelRouter::new("fallback");
        router
            .register_model("cloud", expensive_cloud_model())
            .await;
        router.register_model("local", cheap_local_model()).await;

        let chosen = router.route_task(ComplexityLevel::Simple).await.unwrap();
        assert_eq!(chosen, "local");
    }

    #[tokio::test]
    async fn route_critical_picks_most_capable() {
        let router = ModelRouter::new("fallback");
        router
            .register_model("cloud", expensive_cloud_model())
            .await;
        router.register_model("local", cheap_local_model()).await;

        let chosen = router.route_task(ComplexityLevel::Critical).await.unwrap();
        assert_eq!(chosen, "cloud");
    }

    #[tokio::test]
    async fn route_empty_falls_back_to_default() {
        let router = ModelRouter::new("default-model");
        let chosen = router.route_task(ComplexityLevel::Standard).await.unwrap();
        assert_eq!(chosen, "default-model");
    }

    fn metered_model(is_local: bool) -> ModelMetadata {
        ModelMetadata {
            name: "metered".to_string(),
            provider: "openai".to_string(),
            context_window: 0,
            input_cost_per_million: 1.0,
            output_cost_per_million: 2.0,
            capabilities: vec![],
            is_local,
        }
    }

    #[test]
    fn usage_cost_multiplies_tokens_by_pricing() {
        let meta = metered_model(false);
        let cost = usage_to_cost_usd(&meta, 500_000, 250_000);
        assert!(
            (cost - 1.0).abs() < 1e-9,
            "0.5M×$1 + 0.25M×$2 = $1, got {cost}"
        );
        let full = usage_to_cost_usd(&meta, 1_000_000, 1_000_000);
        assert!((full - 3.0).abs() < 1e-9);
        assert_eq!(usage_to_cost_usd(&meta, 0, 0), 0.0);
    }

    #[test]
    fn local_model_costs_zero_even_with_pricing_configured() {
        let meta = metered_model(true);
        assert_eq!(usage_to_cost_usd(&meta, 10_000_000, 10_000_000), 0.0);
    }

    #[test]
    fn zero_or_negative_pricing_never_yields_negative_cost() {
        let mut meta = metered_model(false);
        meta.input_cost_per_million = 0.0;
        meta.output_cost_per_million = 0.0;
        assert_eq!(usage_to_cost_usd(&meta, 1_000_000, 1_000_000), 0.0);

        meta.input_cost_per_million = -5.0;
        meta.output_cost_per_million = 2.0;
        let cost = usage_to_cost_usd(&meta, 1_000_000, 1_000_000);
        assert!(
            (cost - 2.0).abs() < 1e-9,
            "negative price clamps to 0, got {cost}"
        );
    }
}
