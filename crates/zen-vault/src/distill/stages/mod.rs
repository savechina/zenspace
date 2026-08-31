/// Pipeline stages for distillation (FR-003).
///
/// Each stage encapsulates one phase of the distill pipeline with its own
/// budget accounting and fallback logic.
pub mod llm_distill;

pub use llm_distill::LlmDistillStage;
