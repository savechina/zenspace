pub mod anthropic;
pub mod cohere;
pub mod gemini;
pub mod mistral;
pub mod ollama;
pub mod openai;

pub use anthropic::AnthropicProvider;
pub use cohere::CohereProvider;
pub use gemini::GeminiProvider;
pub use mistral::MistralProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAIProvider;

/// D2b per-call model resolver shared by every provider.
///
/// # Purpose
/// `options.model` (the per-call override) wins over the provider's
/// configured default model name; an absent override returns the configured
/// name unchanged (byte-identical legacy path).
/// # Usage
/// Provider wire builders call this at their completion-model construction
/// site: `client.chat(effective_model_name(&self.model, options))`.
/// # Returns
/// The override when present, otherwise `configured`.
pub fn effective_model_name<'a>(
    configured: &'a str,
    options: &'a zen_core::config::ModelOptions,
) -> &'a str {
    options.model.as_deref().unwrap_or(configured)
}
