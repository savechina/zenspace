pub mod chat;
pub mod model_meta;
pub mod providers;
pub mod rig_ollama;
pub mod rig_openai;
mod router;
pub mod stream;

pub use chat::{ChatMessage, ChatSession, MessageRole};
pub use model_meta::{
    ComplexityLevel, ModelMetadata, ModelRouter, ModelStats,
    PromptHookTelemetry, PromptTelemetry,
};
pub use router::{
    DefaultRouter, LlmConfig, LlmError, LlmRouter, LlmRouterExt, MockProvider, Provider,
    TaskRequirements, is_local_llm_available,
};
pub use stream::StreamResponse;
