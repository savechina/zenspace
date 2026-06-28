pub mod agent_profile;
pub mod blackboard;
pub mod completion_model;
pub mod context;
pub mod coordinator;
pub mod delegate_tools;
pub mod execution;
pub mod executor;
pub mod observability;
pub mod orchestrator;
pub mod prompt;

#[deprecated(
    since = "0.1.0",
    note = "PromptBuilder has been merged into zen_memory::PromptAssembly. Use PromptAssembly instead."
)]
pub use prompt::{PromptBuilder, PromptTemplate};
pub mod registry;
pub mod review;
pub mod scheduler;
pub mod safety_hook;
pub mod sandbox;
pub mod wiring;
pub mod zen_agent;
pub mod zen_skill;

pub use agent_profile::{
    AgentClearance, AgentProfile, AgentProfileBuilder, Capability, CostPerToken, LlmPreference,
    Role,
};
pub use blackboard::{
    Blackboard, BlackboardHandle, BlackboardTask, Deliverable, DeliverableMetadata, Feedback,
    FeedbackErrorType, SystemEvent, ValidationStatus,
};
pub use context::AgentContext;
pub use coordinator::{EntropyConfig, ZenCoordinator};
pub use execution::{AgentExecution, ExecutionMetadata, ToolCall};
pub use executor::{AgentExecutor, ErrorCategory, RetryPolicy};
pub use observability::{emit_prompt_completed, emit_prompt_failed, emit_prompt_started};
pub use orchestrator::AgentOrchestrator;
pub use registry::{AgentRegistry, DefaultAgentRegistry, RegistryError};
pub use review::{HermesValidator, MetisReviewer, MomusReviewer, QualityPipeline, ZeusEscalation};
pub use rig_tap::{EVENT_TARGET, EventKind, ObservabilityEvent, extract_event};
pub use sandbox::{ExecutionOutput, ResourceLimits, WasmSandbox};
pub use wiring::ZenWiring;
pub use zen_agent::{IdentityContext, ZenAgent, ZenAgentBuilder, load_identity_files};
pub use zen_skill::{ZenSkill, ZenTool};
