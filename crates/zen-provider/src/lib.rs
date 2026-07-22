pub mod chat;
pub mod embedding;
pub mod model_meta;
pub mod providers;
mod router;
pub mod stream;

pub use chat::{ChatMessage, ChatSession, MessageRole};
pub use embedding::{
    DefaultEmbeddingRouter, EmbeddingError, EmbeddingProvider, EmbeddingRouter,
    OpenAiEmbeddingProvider, OllamaEmbeddingProvider,
};
pub use model_meta::{
    ModelMetadata, ModelRouter, ModelStats, PromptHookTelemetry, PromptTelemetry,
};
pub use zen_core::types::ComplexityLevel;
pub use router::{
    DefaultLlmRetryClassifier, DefaultRouter, LlmConfig, LlmError, LlmRetryClassifier, LlmRouter,
    LlmRouterExt, MockProvider, Provider, TaskRequirements, is_local_llm_available,
};
pub use stream::StreamResponse;
