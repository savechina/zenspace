use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use zen_core::config::ZenConfig;

use crate::router::resolve_api_key;

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

#[derive(Debug, Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Serialize)]
struct OpenAiEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedResponse {
    data: Vec<OpenAiEmbedData>,
}

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
        let url = format!("{}/api/embed", self.base_url.trim_end_matches('/'));
        let payload = OllamaEmbedRequest {
            model: self.model.clone(),
            input: vec![text.to_string()],
        };

        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(&url)
            .json(&payload)
            .send()
            .map_err(|e| EmbeddingError::RequestFailed {
                reason: format!("Ollama request error: {e}"),
            })?;

        if !resp.status().is_success() {
            return Err(EmbeddingError::RequestFailed {
                reason: format!("Ollama HTTP {}", resp.status()),
            });
        }

        let response: OllamaEmbedResponse = resp.json().map_err(|e| EmbeddingError::ParseError {
            reason: format!("Ollama response parse error: {e}"),
        })?;

        response
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::ParseError {
                reason: "Ollama returned empty embeddings".to_string(),
            })
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
        let url = format!("{}/v1/embeddings", self.base_url.trim_end_matches('/'));
        let payload = OpenAiEmbedRequest {
            model: self.model.clone(),
            input: vec![text.to_string()],
        };

        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&payload)
            .send()
            .map_err(|e| EmbeddingError::RequestFailed {
                reason: format!("OpenAI request error: {e}"),
            })?;

        if !resp.status().is_success() {
            return Err(EmbeddingError::RequestFailed {
                reason: format!("OpenAI HTTP {}", resp.status()),
            });
        }

        let response: OpenAiEmbedResponse = resp.json().map_err(|e| EmbeddingError::ParseError {
            reason: format!("OpenAI response parse error: {e}"),
        })?;

        response
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| EmbeddingError::ParseError {
                reason: "OpenAI returned empty embeddings".to_string(),
            })
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
