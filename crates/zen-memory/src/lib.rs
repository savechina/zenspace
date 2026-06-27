pub mod anti_pattern;
pub mod belief;
pub mod commitment;
pub mod okr;
pub mod context_budget;
pub mod conversation;
pub mod correction;
pub mod fact;
pub mod decision;
pub mod decision_check;
pub mod dream;
pub mod feedback_signal;
pub mod identity;
pub mod journal;
pub mod history;
pub mod memory;
pub mod memory_flush;
pub mod memory_service;
pub mod memvid;
pub mod memvid_index;
pub mod mental_model;
pub mod prompt;
pub mod priority;
pub mod quality_gate;
pub mod reflection_signal;
pub mod seed;
pub mod self_model;
pub mod sensitivity;
pub mod session;
pub mod virtue_log;

#[deprecated(
    since = "0.0.1",
    note = "Will be refactored into CompactionStrategyTrait for multi-strategy extensibility. \
            Use rig_compose::ContextPack + rig_memvid::MemoryContextPack for built-in path."
)]
#[allow(deprecated)]
pub use context_budget::ContextBudget;
pub use conversation::ConversationStore;
pub use dream::ZenDream;
pub use history::HistoryStore;
#[allow(deprecated)]
pub use memory::{MemoryEntry, MemoryStats, MemoryStore};
pub use memory_flush::MemoryFlush;
pub use memory_service::IdentityContext;
#[allow(deprecated)]
pub use memvid::{
    CompactionResult, CompactionStrategy, ContextProjector, ZenMemvidStore, create_persist_hook,
    default_memory_config,
};
pub use memvid::TRIPLET_MIN_CONFIDENCE;
pub use memvid_index::{MemvidIndexer, MemvidIndexReport};
pub use prompt::PromptAssembly;
pub use seed::{copy_seeds_to, seed_file_paths, SEED_FILE_COUNT};
pub use sensitivity::{compute_max_sensitivity, validate_provider_for_sensitivity};
pub use session::SessionManager;
pub use session::{ConversationTurn, RetrievedNote, SessionContext};
pub use belief::{Belief, EvidenceEntry, ResearchMethod, SourceType};
pub use decision::{
    AntiPatternReport, AntiPatternViolation, CostBreakdown, Decision, ExpectedValue, Outcome,
    OutcomeResult, Severity,
};
pub use self_model::{SelfModelItem, SelfModelLayer};
pub use commitment::{Commitment, CommitmentState, ExecutionChecklist, Milestone, StopLossLine};
pub use okr::{CommitmentOkr, compute_commitment_completion_rate};
pub use correction::Correction;
pub use fact::Fact;
pub use feedback_signal::{Feedback, FeedbackDisposition, FeedbackProperties};
pub use quality_gate::{
    Bias, DecisionPrincipleReport, DecisionPromotionReport, InformationQualityGate,
    check_decision_principles, DECISION_PRINCIPLES, EXTRACTION_GUARDRAILS,
};
pub use anti_pattern::AntiPatternSignal;
pub use mental_model::MentalModelSignal;
pub use reflection_signal::ReflectionSignal;
pub use virtue_log::{VirtueDomain, VirtueLog, VirtueStatus};
pub use zen_core::types::{SessionEntity, SessionStatus};
