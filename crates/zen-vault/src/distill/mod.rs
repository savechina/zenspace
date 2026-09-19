pub mod adversarial;
pub mod archive;
pub mod chat_import;
pub mod checkpoint;
pub mod contradiction;
pub mod convert;
pub mod correlation;
pub mod discover_metrics;
pub mod hypothesis;
pub mod merge;
pub mod notion_extraction;
pub mod orchestration_stats;
pub mod pipeline;
pub mod recovery;
pub mod reflection;
pub mod reward_sidecar;
pub mod source_ingest;
pub mod stages;
pub mod tool_call_log;
pub mod transaction;
pub mod types;
pub mod wiki_compile;

pub use adversarial::{
    ARENA_REPORT_PREFIX, AdversarialReport, CaseResult, CliContestant, Contestant,
    ContestantVerdict, EvalCase, INCUMBENT, NaiveBaseline, PhaseScore, ZenDistill, corpus,
    judge_case, report_path, run_arena,
};
pub use chat_import::ChatImporter;
pub use checkpoint::{Checkpoint, CheckpointManager};
pub use contradiction::{Contradiction, ContradictionDetector};
pub use correlation::{Opportunity, correlate};
pub use discover_metrics::{
    DiscoverMetrics, DiscoverMetricsError, METRICS_FILE_NAME, REPORT_FILE_PREFIX, aggregate,
    load_reports, write_metrics,
};
pub use hypothesis::{
    build_exploration_prompt, build_refinement_queue, build_refinement_queue_prioritized,
    confidence_for, gap_type, generate_from_gaps, generate_from_gaps_with_history, load_all,
    reverify, reverify_with_rejections, save, slug_for,
};
pub use merge::{
    MergeStrategy, WikiMergePlan, build_merge_plans, normalize_notion_name, trigram_jaccard,
};
pub use notion_extraction::NotionExtractor;
pub use orchestration_stats::{
    AuditError, DelegateGatesStats, GatewayStats, IntentDist, LivenessStats, OrchestrationStats,
    PlanCompletedStats, TurnReviewStats, aggregate_orchestration,
};
pub use pipeline::{
    DistillationPipeline, DistillationPipelineInput, DistillationReport, ScopedRunOutcome,
    prune_context,
};
pub use recovery::RecoveryManager;
pub use reward_sidecar::{
    card_id_from_path, increment_access, increment_citations, increment_corrections, read_reward,
    write_reward,
};
pub use source_ingest::SourceIngester;
pub use stages::LlmDistillStage;
pub use tool_call_log::{
    ToolCallAggregate, aggregate_all_sessions, aggregate_tool_calls, append_tool_call,
    prune_expired_sessions,
};
pub use transaction::TransactionScope;
pub use types::{
    Belief, Commitment, CommitmentLifecycle, CycleOutcome, Decision, GapKind, GapRecord,
    GraphPlaceholder, HypothesisSlug, HypothesisStatus, JobState, LoopBudget, LoopCycleReport,
    MemoryReward, PlaceholderStatus, ProcessingJob, SelfModelItem, SelfModelLayer, ToolCall,
    TypedSignalKind, VerificationNode,
};
pub use wiki_compile::WikiCompiler;

pub struct Distill;
