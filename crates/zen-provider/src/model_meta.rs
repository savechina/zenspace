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
            ComplexityLevel::Simple => {
                metadata.iter().min_by(|(_, a), (_, b)| {
                    let cost_cmp =
                        a.total_cost().partial_cmp(&b.total_cost()).unwrap_or(std::cmp::Ordering::Equal);
                    if cost_cmp != std::cmp::Ordering::Equal {
                        return cost_cmp;
                    }
                    b.is_local.cmp(&a.is_local)
                })
            }
            ComplexityLevel::Standard => {
                metadata.iter().max_by(|(_, a), (_, b)| {
                    a.cost_efficiency()
                        .partial_cmp(&b.cost_efficiency())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            }
            ComplexityLevel::Complex => {
                metadata.iter().max_by(|(_, a), (_, b)| {
                    let cap_cmp = a.capability_score().cmp(&b.capability_score());
                    if cap_cmp != std::cmp::Ordering::Equal {
                        return cap_cmp;
                    }
                    b.total_cost()
                        .partial_cmp(&a.total_cost())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            }
            ComplexityLevel::Critical => {
                metadata
                    .iter()
                    .max_by_key(|(_, m)| m.capability_score())
            }
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
        router.register_model("cloud", expensive_cloud_model()).await;
        router.register_model("local", cheap_local_model()).await;

        let chosen = router.route_task(ComplexityLevel::Simple).await.unwrap();
        assert_eq!(chosen, "local");
    }

    #[tokio::test]
    async fn route_critical_picks_most_capable() {
        let router = ModelRouter::new("fallback");
        router.register_model("cloud", expensive_cloud_model()).await;
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
}
