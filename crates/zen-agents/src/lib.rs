pub mod agent_profile;
pub mod completion_model;
pub mod context;
pub mod decision;
pub mod delegate_task;
pub mod delegate_tools;
pub mod execution;
pub mod executor;
pub mod intent;
pub mod observability;
pub mod orchestrator;
pub mod output_schema;
pub mod plan_task;
pub mod prompt;
pub mod telemetry;

#[deprecated(
    since = "0.1.0",
    note = "PromptBuilder has been merged into zen_memory::PromptAssembly. Use PromptAssembly instead."
)]
pub use prompt::{PromptBuilder, PromptTemplate};
pub mod registry;
pub mod review;
pub mod safety_hook;
pub mod scheduler;
pub mod skill_embedding;
pub mod skill_history;
pub mod skill_hit_router;
pub mod skill_loader;
pub mod skill_precipitation;
pub mod wiring;
pub mod zen_agent;
pub mod zen_skill;

/// Turn-affinity scope for approval routing (SC-007, T103), re-exported
/// so the gateway can set it around hosted turns without depending on
/// zen-plugin directly.
pub use zen_plugin::tools::approval_hook::APPROVAL_TURN;

pub use agent_profile::{
    AgentClearance, AgentProfile, AgentProfileBuilder, Capability, CostPerToken, LlmPreference,
    Role,
};
pub use context::AgentContext;
pub use decision::{BinaryDecision, classify_binary, resolve_binary_threshold};
pub use execution::{AgentExecution, ExecutionMetadata, ToolCall};
pub use executor::{AgentExecutor, ErrorCategory, RetryPolicy};
pub use observability::{emit_prompt_completed, emit_prompt_failed, emit_prompt_started};
pub use orchestrator::AgentOrchestrator;
pub use registry::{AgentRegistry, DefaultAgentRegistry, RegistryError};
pub use review::{HermesValidator, MetisReviewer, MomusReviewer, QualityPipeline, ZeusEscalation};
pub use skill_hit_router::{SKILL_HIT_MAX_HITS, SKILL_HIT_THRESHOLD, SkillHit, SkillHitRouter};
pub use telemetry::{EVENT_TARGET, EventKind, TelemetryEvent};
pub use wiring::ZenWiring;
pub use zen_agent::{IdentityContext, ZenAgent, ZenAgentBuilder, load_identity_files};
pub use zen_skill::{ZenSkill, ZenTool};
