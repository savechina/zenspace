pub mod chat_import;
pub mod checkpoint;
pub mod contradiction;
pub mod merge;
pub mod notion_extraction;
pub mod pipeline;
pub mod recovery;
pub mod source_ingest;
pub mod transaction;
pub mod types;
pub mod wiki_compile;

pub use chat_import::ChatImporter;
pub use checkpoint::{Checkpoint, CheckpointManager};
pub use contradiction::{Contradiction, ContradictionDetector};
pub use notion_extraction::NotionExtractor;
pub use merge::{MergeStrategy, WikiMergePlan, build_merge_plans, trigram_jaccard};
pub use pipeline::{DistillationPipeline, DistillationPipelineInput, DistillationReport};
pub use recovery::RecoveryManager;
pub use source_ingest::SourceIngester;
pub use transaction::TransactionScope;
pub use types::{
    Belief, Commitment, CommitmentLifecycle, CycleOutcome, Decision, GapKind, GapRecord,
    GraphPlaceholder, HypothesisSlug, HypothesisStatus, JobState, LoopBudget, LoopCycleReport,
    MemoryReward, PlaceholderStatus, ProcessingJob, SelfModelItem, SelfModelLayer, ToolCall,
    TypedSignalKind, VerificationNode,
};
pub use wiki_compile::WikiCompiler;

pub struct Distill;
