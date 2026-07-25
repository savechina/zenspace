use std::str::FromStr;
use std::sync::Mutex;

use fastembed::{EmbeddingModel as FastEmbedModel, TextInitOptions};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn};
use zen_core::config::ZenConfig;

use crate::router::resolve_api_key;
use rig::client::{EmbeddingsClient, Nothing};
use rig::embeddings::EmbeddingModel as RigEmbeddingModel;
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

                let emb_model = client.embedding_model_with_ndims(&model, 4096);
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
        // System dimension is 4096 — shorter vectors (e.g. 384-dim
        // from nomic-embed-text) are zero-padded at the application layer.
        4096
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiEmbeddingProvider {
    api_key: String,
    model: String,
    base_url: String,
    /// Human-readable name (e.g., "openai", "aliyun", "deepseek").
    name: String,
}

impl OpenAiEmbeddingProvider {
    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            api_key,
            model,
            base_url,
            name: "openai".into(),
        }
    }

    /// Override the provider name (used for log/error attribution).
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
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
        &self.name
    }

    fn expected_dimension(&self) -> usize {
        384
    }
}

// ---------------------------------------------------------------------------
// Local fastembed provider — ONNX-based local inference (macOS M4 etc.)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LocalFastembedProvider {
    /// Optional override model name; `None` uses fastembed's default (BGESmallENV15).
    model: Option<String>,
    /// Optional HuggingFace mirror endpoint (e.g., `https://hf-mirror.com`).
    hf_endpoint: Option<String>,
    /// Cache directory for downloaded model files.
    /// When `None`, fastembed defaults to `./.fastembed_cache`.
    cache_dir: Option<PathBuf>,
}

impl LocalFastembedProvider {
    pub fn new(
        model: Option<String>,
        hf_endpoint: Option<String>,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            model,
            hf_endpoint,
            cache_dir,
        }
    }

    /// Resolve the effective cache directory:
    ///   - configured path (with `~/` expansion) if set,
    ///   - otherwise `~/.cache/fastembed/` for global sharing across projects.
    fn effective_cache_dir(&self) -> PathBuf {
        match &self.cache_dir {
            Some(dir) => {
                // Expand leading ~/ to home directory
                if let Some(rest) = dir.to_str().and_then(|s| s.strip_prefix("~/")) {
                    home::home_dir()
                        .map(|h| h.join(rest))
                        .unwrap_or_else(|| dir.clone())
                } else {
                    dir.clone()
                }
            }
            None => home::home_dir()
                .map(|h| h.join(".cache").join("fastembed"))
                .unwrap_or_else(|| PathBuf::from(".fastembed_cache")),
        }
    }
}

impl EmbeddingProvider for LocalFastembedProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        // Set HF_ENDPOINT before fastembed init if configured.
        // Safety: set_var is unsafe in multithreaded contexts, but this runs
        // once during model init (or early in the process lifetime), and the
        // fastembed crate reads HF_ENDPOINT at most once during download.
        if let Some(endpoint) = &self.hf_endpoint {
            unsafe { std::env::set_var("HF_ENDPOINT", endpoint) };
        }

        let cache_dir = self.effective_cache_dir();
        let init_options: TextInitOptions = match &self.model {
            Some(name) => {
                let model =
                    FastEmbedModel::from_str(name).map_err(|e| EmbeddingError::RequestFailed {
                        reason: format!("unknown fastembed model '{name}': {e}"),
                    })?;
                TextInitOptions::new(model).with_cache_dir(cache_dir)
            }
            None => {
                let opts: TextInitOptions = Default::default();
                opts.with_cache_dir(cache_dir)
            }
        };

        let mut embedder = fastembed::TextEmbedding::try_new(init_options).map_err(|e| {
            EmbeddingError::RequestFailed {
                reason: format!("fastembed init error: {e}"),
            }
        })?;

        let input = vec![text.to_string()];
        let mut vecs = embedder
            .embed(input, None)
            .map_err(|e| EmbeddingError::RequestFailed {
                reason: format!("fastembed embed error: {e}"),
            })?;

        vecs.pop().ok_or_else(|| EmbeddingError::RequestFailed {
            reason: "fastembed returned empty result".into(),
        })
    }

    fn provider_name(&self) -> &str {
        "fastembed"
    }

    fn expected_dimension(&self) -> usize {
        // BGESmallENV15 (fastembed default) outputs 384-dim.
        384
    }
}

#[derive(Debug)]
pub struct DefaultEmbeddingRouter {
    providers: Vec<Box<dyn EmbeddingProvider>>,
    /// Tracks which providers have previously failed (by index).
    /// Once dead, they are skipped on subsequent calls to avoid
    /// repeated timeouts (e.g., unreachable network, unsupported model).
    dead: Mutex<Vec<bool>>,
}

impl DefaultEmbeddingRouter {
    /// Build an embedding router from the new `[embeddings]` config section.
    ///
    /// Selects exactly one provider based on:
    ///   `provider = "local"`  → local inference (fastembed or Ollama)
    ///   `provider = "cloud"`  → remote API (referenced by name in [providers])
    pub fn from_config(config: &ZenConfig) -> Self {
        let emb = &config.embeddings;
        let mode = emb.provider.as_deref().unwrap_or("local");

        let mut providers: Vec<Box<dyn EmbeddingProvider>> = Vec::new();

        match mode {
            "local" => {
                let local_kind = emb.local_provider.as_deref().unwrap_or("fastembed");
                let model = emb.model.clone();

                match local_kind {
                    "fastembed" => {
                        let cache_dir = emb.cache_dir.as_ref().map(PathBuf::from);
                        let p =
                            LocalFastembedProvider::new(model, emb.hf_endpoint.clone(), cache_dir);
                        info!(
                            local_provider = "fastembed",
                            model = ?emb.model,
                            "DefaultEmbeddingRouter: registered fastembed"
                        );
                        providers.push(Box::new(p));
                    }
                    "ollama" => {
                        // Find the first ollama provider config.
                        if let Some((name, cfg)) = config
                            .providers
                            .iter()
                            .find(|(_, c)| c.provider_type.as_deref() == Some("ollama"))
                        {
                            let base_url = cfg
                                .base_url
                                .clone()
                                .unwrap_or_else(|| zen_core::constants::OLLAMA_BASE_URL.into());
                            let m = model
                                .clone()
                                .or_else(|| cfg.embedding_model.clone())
                                .unwrap_or_else(|| "qwen3-embedding".into());
                            info!(
                                provider = name,
                                base_url = %base_url,
                                model = %m,
                                "DefaultEmbeddingRouter: registered Ollama (local)"
                            );
                            let p: Box<dyn EmbeddingProvider> =
                                Box::new(OllamaEmbeddingProvider::new(base_url, m));
                            providers.push(p);
                        } else {
                            warn!(
                                "DefaultEmbeddingRouter: local=ollama but no ollama provider in config"
                            );
                        }
                    }
                    other => {
                        warn!("DefaultEmbeddingRouter: unknown local_provider={other}, skipping");
                    }
                }
            }
            "cloud" => {
                let api_provider = emb.api_provider.as_deref().unwrap_or("aliyun");
                let model = emb.model.clone();

                if let Some((name, cfg)) = config.providers.iter().find(|(n, _)| n == &api_provider)
                {
                    let provider_type = cfg.provider_type.as_deref().unwrap_or("openai-compatible");
                    match provider_type {
                        "ollama" => {
                            let base_url = cfg
                                .base_url
                                .clone()
                                .unwrap_or_else(|| zen_core::constants::OLLAMA_BASE_URL.into());
                            let m = model
                                .clone()
                                .or_else(|| cfg.embedding_model.clone())
                                .unwrap_or_else(|| "qwen3-embedding".into());
                            info!(
                                provider = name,
                                base_url = %base_url,
                                model = %m,
                                "DefaultEmbeddingRouter: registered Ollama (cloud)"
                            );
                            let p: Box<dyn EmbeddingProvider> =
                                Box::new(OllamaEmbeddingProvider::new(base_url, m));
                            providers.push(p);
                        }
                        "openai" | "openai-compatible" => {
                            if let Some(api_key) = resolve_api_key(cfg, name) {
                                let base_url = cfg
                                    .base_url
                                    .clone()
                                    .unwrap_or_else(|| zen_core::constants::OPENAI_BASE_URL.into());
                                let m = model
                                    .clone()
                                    .or_else(|| cfg.embedding_model.clone())
                                    .or_else(|| cfg.default_model.clone())
                                    .unwrap_or_else(|| "text-embedding-3-small".into());
                                info!(
                                    provider = name,
                                    model = %m,
                                    "DefaultEmbeddingRouter: registered OpenAI"
                                );
                                let p: Box<dyn EmbeddingProvider> = Box::new(
                                    OpenAiEmbeddingProvider::new(api_key, m, base_url)
                                        .with_name(name),
                                );
                                providers.push(p);
                            } else {
                                warn!(
                                    provider = name,
                                    "DefaultEmbeddingRouter: no API key for {api_provider}"
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
                } else {
                    warn!(
                        api_provider = api_provider,
                        "DefaultEmbeddingRouter: provider not found in config"
                    );
                }
            }
            other => {
                warn!(
                    "DefaultEmbeddingRouter: unknown provider mode={other}, no embeddings available"
                );
            }
        }

        Self {
            dead: Mutex::new(vec![false; providers.len()]),
            providers,
        }
    }

    pub fn with_providers(providers: Vec<Box<dyn EmbeddingProvider>>) -> Self {
        Self {
            dead: Mutex::new(vec![false; providers.len()]),
            providers,
        }
    }

    fn embed_with_providers(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let mut dead = self.dead.lock().unwrap();

        for (i, provider) in self.providers.iter().enumerate() {
            if i < dead.len() && dead[i] {
                continue;
            }
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
                        "DefaultEmbeddingRouter: failed, retrying once"
                    );
                    // Retry once for transient failures (e.g., Ollama runner crash).
                    // The runner is ephemeral; on retry, Ollama spawns a new one.
                    match provider.embed(text) {
                        Ok(embedding) => {
                            info!(
                                provider = provider.provider_name(),
                                dim = embedding.len(),
                                "DefaultEmbeddingRouter: computed on retry"
                            );
                            return Ok(embedding);
                        }
                        Err(e2) => {
                            warn!(
                                provider = provider.provider_name(),
                                error = %e2,
                                "DefaultEmbeddingRouter: retry also failed, marking dead"
                            );
                            if i < dead.len() {
                                dead[i] = true;
                            }
                        }
                    }
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
    fn test_ollama_provider() {
        let p = OllamaEmbeddingProvider::new(
            "http://localhost:11434".into(),
            "nomic-embed-text".into(),
        );
        assert_eq!(p.provider_name(), "ollama");
        // System embedding dimension is 4096; shorter vectors are padded.
        assert_eq!(p.expected_dimension(), 4096);
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
    fn test_empty_config_creates_default_router() {
        let config = ZenConfig::default();
        let router = DefaultEmbeddingRouter::from_config(&config);
        // Default config creates a local fastembed provider.
        assert!(!router.providers.is_empty());
        assert_eq!(router.providers.len(), 1);
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
