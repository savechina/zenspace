pub mod context_budget;
pub mod conversation;
pub mod journal;
pub mod history;
pub mod dream;
pub mod identity;
pub mod memory;
pub mod memory_flush;
pub mod memory_service;
pub mod memvid;
pub mod prompt;
pub mod sensitivity;
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
pub use prompt::PromptAssembly;
pub use sensitivity::{compute_max_sensitivity, validate_provider_for_sensitivity};
pub use session::SessionManager;
pub use session::{ConversationTurn, RetrievedNote, SessionContext};
pub use zen_core::types::{SessionEntity, SessionStatus};
