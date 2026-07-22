use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use zen_core::config::ZenConfig;

use crate::router::resolve_api_key;
use rig::client::{EmbeddingsClient, Nothing};
use rig::embeddings::EmbeddingModel;
use rig::providers::{ollama, openai};

pub trait EmbeddingProvider: Send + Sync + std::fmt::Debug {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
    fn provider_name(&self) -> &str;
    fn expected_dimension(&self) -> usize {
        0
    }
}

pub trait EmbeddingRouter: Send + Sync + std::fmt::Debug {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
    fn list_providers(&self) -> Vec<(String, String)>;
}

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("embedding provider unavailable: {provider} — {reason}")]
    ProviderUnavailable { provider: String, reason: String },
    #[error("embedding request failed: {reason}")]
    RequestFailed { reason: String },
    #[error("embedding response parse error: {reason}")]
    ParseError { reason: String },
    #[error("no embedding provider configured")]
    NoProvider,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
struct OpenAiEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OpenAiEmbedResponse {
    data: Vec<OpenAiEmbedData>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OpenAiEmbedData {
    embedding: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct OllamaEmbeddingProvider {
    base_url: String,
    model: String,
}

impl OllamaEmbeddingProvider {
    pub fn new(base_url: String, model: String) -> Self {
        Self { base_url, model }
    }
}

impl EmbeddingProvider for OllamaEmbeddingProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let base_url = self.base_url.clone();
        let model = self.model.clone();
        let text = text.to_string();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let client = ollama::Client::builder()
                    .api_key(Nothing)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| EmbeddingError::RequestFailed {
                        reason: format!("Ollama client init error: {e}"),
                    })?;

                let emb_model = client.embedding_model_with_ndims(&model, 384);
                let embedding = emb_model.embed_text(&text).await.map_err(|e| {
                    EmbeddingError::RequestFailed {
                        reason: format!("Ollama embed error: {e}"),
                    }
                })?;

                Ok(embedding.vec.into_iter().map(|x| x as f32).collect())
            })
        })
        .join()
        .map_err(|e| EmbeddingError::RequestFailed {
            reason: format!("Ollama embed thread panic: {:?}", e),
        })?
    }

    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn expected_dimension(&self) -> usize {
        384
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiEmbeddingProvider {
    api_key: String,
    model: String,
    base_url: String,
}

impl OpenAiEmbeddingProvider {
    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            api_key,
            model,
            base_url,
        }
    }
}

impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let model = self.model.clone();
        let text = text.to_string();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let mut builder = openai::Client::builder().api_key(&api_key);
                if base_url != zen_core::constants::OPENAI_API_URL {
                    builder = builder.base_url(&base_url);
                }
                let client = builder.build().map_err(|e| EmbeddingError::RequestFailed {
                    reason: format!("OpenAI client init error: {e}"),
                })?;

                let emb_model = client.embedding_model(&model);
                let embedding = emb_model.embed_text(&text).await.map_err(|e| {
                    EmbeddingError::RequestFailed {
                        reason: format!("OpenAI embed error: {e}"),
                    }
                })?;

                Ok(embedding.vec.into_iter().map(|x| x as f32).collect())
            })
        })
        .join()
        .map_err(|e| EmbeddingError::RequestFailed {
            reason: format!("OpenAI embed thread panic: {:?}", e),
        })?
    }

    fn provider_name(&self) -> &str {
        "openai"
    }

    fn expected_dimension(&self) -> usize {
        384
    }
}

#[derive(Debug)]
pub struct DefaultEmbeddingRouter {
    providers: Vec<Box<dyn EmbeddingProvider>>,
}

impl DefaultEmbeddingRouter {
    pub fn from_config(config: &ZenConfig) -> Self {
        let mut local: Vec<Box<dyn EmbeddingProvider>> = Vec::new();
        let mut cloud: Vec<Box<dyn EmbeddingProvider>> = Vec::new();

        for (name, cfg) in &config.providers {
            let provider_type = cfg.provider_type.as_deref().unwrap_or("openai-compatible");

            match provider_type {
                "ollama" => {
                    let base_url = cfg
                        .base_url
                        .clone()
                        .unwrap_or_else(|| zen_core::constants::OLLAMA_BASE_URL.into());
                    let model = cfg
                        .default_model
                        .clone()
                        .unwrap_or_else(|| "nomic-embed-text".into());
                    info!(
                        provider = name,
                        base_url = %base_url,
                        model = %model,
                        "DefaultEmbeddingRouter: registered Ollama"
                    );
                    let p: Box<dyn EmbeddingProvider> =
                        Box::new(OllamaEmbeddingProvider::new(base_url, model));
                    local.push(p);
                }
                "openai" | "openai-compatible" => {
                    if let Some(api_key) = resolve_api_key(cfg, name) {
                        let base_url = cfg
                            .base_url
                            .clone()
                            .unwrap_or_else(|| zen_core::constants::OPENAI_BASE_URL.into());
                        let model = cfg
                            .default_model
                            .clone()
                            .unwrap_or_else(|| "text-embedding-3-small".into());
                        info!(
                            provider = name,
                            model = %model,
                            "DefaultEmbeddingRouter: registered OpenAI"
                        );
                        let p: Box<dyn EmbeddingProvider> =
                            Box::new(OpenAiEmbeddingProvider::new(api_key, model, base_url));
                        cloud.push(p);
                    } else {
                        warn!(
                            provider = name,
                            "DefaultEmbeddingRouter: no API key, skipping"
                        );
                    }
                }
                other => {
                    warn!(
                        provider = name,
                        provider_type = other,
                        "DefaultEmbeddingRouter: unknown type, skipping"
                    );
                }
            }
        }

        local.append(&mut cloud);
        Self { providers: local }
    }

    pub fn with_providers(providers: Vec<Box<dyn EmbeddingProvider>>) -> Self {
        Self { providers }
    }

    fn embed_with_providers(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        for provider in &self.providers {
            match provider.embed(text) {
                Ok(embedding) => {
                    info!(
                        provider = provider.provider_name(),
                        dim = embedding.len(),
                        "DefaultEmbeddingRouter: computed"
                    );
                    return Ok(embedding);
                }
                Err(e) => {
                    warn!(
                        provider = provider.provider_name(),
                        error = %e,
                        "DefaultEmbeddingRouter: failed, trying next"
                    );
                }
            }
        }
        Err(EmbeddingError::NoProvider)
    }

    pub fn embed_sync(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_with_providers(text)
    }
}

impl EmbeddingRouter for DefaultEmbeddingRouter {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_with_providers(text)
    }

    fn list_providers(&self) -> Vec<(String, String)> {
        self.providers
            .iter()
            .map(|p| (p.provider_name().to_string(), String::new()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_provider_name() {
        let p = OllamaEmbeddingProvider::new(
            "http://localhost:11434".into(),
            "nomic-embed-text".into(),
        );
        assert_eq!(p.provider_name(), "ollama");
        assert_eq!(p.expected_dimension(), 384);
    }

    #[test]
    fn test_openai_provider_name() {
        let p = OpenAiEmbeddingProvider::new(
            "test-key".into(),
            "text-embedding-3-small".into(),
            "https://api.openai.com".into(),
        );
        assert_eq!(p.provider_name(), "openai");
        assert_eq!(p.expected_dimension(), 384);
    }

    #[test]
    fn test_empty_config_creates_empty_router() {
        let config = ZenConfig::default();
        let router = DefaultEmbeddingRouter::from_config(&config);
        assert!(router.providers.is_empty());
    }

    #[test]
    fn test_empty_router_returns_no_provider_error() {
        let router = DefaultEmbeddingRouter::with_providers(vec![]);
        let result = router.embed("hello");
        assert!(matches!(result, Err(EmbeddingError::NoProvider)));
    }

    #[test]
    fn test_router_tries_providers_in_order() {
        let p1: Box<dyn EmbeddingProvider> = Box::new(FailingProvider("first"));
        let p2: Box<dyn EmbeddingProvider> = Box::new(FailingProvider("second"));
        let router = DefaultEmbeddingRouter::with_providers(vec![p1, p2]);
        let result = router.embed("hello");
        assert!(matches!(result, Err(EmbeddingError::NoProvider)));
    }

    #[test]
    fn test_router_returns_first_success() {
        let p1: Box<dyn EmbeddingProvider> = Box::new(FailingProvider("first"));
        let p2: Box<dyn EmbeddingProvider> = Box::new(SuccessProvider(vec![1.0, 2.0, 3.0]));
        let p3: Box<dyn EmbeddingProvider> = Box::new(SuccessProvider(vec![4.0, 5.0, 6.0]));
        let router = DefaultEmbeddingRouter::with_providers(vec![p1, p2, p3]);
        let result = router.embed("hello").unwrap();
        assert_eq!(result, vec![1.0, 2.0, 3.0]);
    }

    #[derive(Debug)]
    struct FailingProvider(&'static str);

    impl EmbeddingProvider for FailingProvider {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::ProviderUnavailable {
                provider: self.0.to_string(),
                reason: "intentional failure".to_string(),
            })
        }

        fn provider_name(&self) -> &str {
            self.0
        }
    }

    #[derive(Debug)]
    struct SuccessProvider(Vec<f32>);

    impl EmbeddingProvider for SuccessProvider {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(self.0.clone())
        }

        fn provider_name(&self) -> &str {
            "success"
        }
    }
}
