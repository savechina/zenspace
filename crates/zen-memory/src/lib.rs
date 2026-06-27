pub mod context_budget;
pub mod conversation;
pub mod journal;
pub mod history;
pub mod belief;
pub mod decision;
pub mod decision_check;
pub mod dream;
pub mod identity;
pub mod memory;
pub mod memory_flush;
pub mod memory_service;
pub mod memvid;
pub mod memvid_index;
pub mod prompt;
pub mod seed;
pub mod sensitivity;
pub mod self_model;
pub mod session;

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
pub use belief::{Belief, EvidenceEntry, SourceType};
pub use decision::{
    AntiPatternReport, AntiPatternViolation, CostBreakdown, Decision, ExpectedValue, Outcome,
    OutcomeResult, Severity,
};
pub use self_model::{SelfModelItem, SelfModelLayer};
pub use zen_core::types::{SessionEntity, SessionStatus};
