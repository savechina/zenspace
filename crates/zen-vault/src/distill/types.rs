//! Loop domain types (005-agentic-loop, T002).
//!
//! Schemas per `docs/specs/005-agentic-loop/data-model.md` §1-4, §9.
//! `GapKind` is additive-only: new variants are appended, never renumbered.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Outcome of one loop cycle (worker contract stage 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleOutcome {
    Completed,
    Failed,
    Aborted,
}

impl CycleOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            CycleOutcome::Completed => "completed",
            CycleOutcome::Failed => "failed",
            CycleOutcome::Aborted => "aborted",
        }
    }
}

/// Report emitted at the end of every processing cycle (data-model §2).
///
/// Supersedes [`super::DistillationReport`], which remains as a thin
/// compatibility alias for the manual `zen wiki distill` code path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoopCycleReport {
    pub cycle_id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub outcome: Option<CycleOutcome>,
    pub dry_run: bool,
    pub notes_processed: usize,
    pub entities_persisted: usize,
    pub pages_created: usize,
    pub merged_count: usize,
    pub archived_count: usize,
    pub quarantined_count: usize,
    pub pending_count: usize,
    pub gaps: Vec<GapRecord>,
    pub last_error: Option<String>,
}

/// One detected knowledge-processing gap (data-model §3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapRecord {
    pub id: String,
    pub kind: GapKind,
    /// Vault-relative path of the offending page/note, if any.
    pub subject_path: Option<String>,
    /// Entity name the gap is about, if any.
    pub subject_entity: Option<String>,
    pub detail: String,
    pub detected_at: DateTime<Utc>,
    pub cycle_id: String,
}

impl GapRecord {
    pub fn new(kind: GapKind, cycle_id: &str, detail: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            kind,
            subject_path: None,
            subject_entity: None,
            detail: detail.into(),
            detected_at: Utc::now(),
            cycle_id: cycle_id.to_string(),
        }
    }

    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.subject_path = Some(path.into().to_string_lossy().to_string());
        self
    }

    pub fn with_entity(mut self, entity: impl Into<String>) -> Self {
        self.subject_entity = Some(entity.into());
        self
    }
}

/// Knowledge-processing gap taxonomy (data-model §3, additive-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// `raw/` or inbox file untouched > 2 cycles.
    IngestNeverConsolidated,
    /// Concept page with no DB entity (verifier check 1).
    WikiPageWithoutEntities,
    /// Entity with no wiki page and no relationships (verifier check 2).
    OrphanEntity,
    /// Relationship references missing note/entity (verifier check 3).
    UnresolvedRelationship,
    /// Malformed note moved to quarantine.
    QuarantinedNote,
    /// Note failed N retries, quarantined.
    LlmFailure,
    /// Normalized alias collision per DESIGN §7.3 (FR-022).
    DuplicateEntityAlias,
    /// Decision failed 7-principles/10-anti-patterns CRIT check (FR-024).
    DecisionBlocked,
    /// Commitment `review_at` passed, daily tracker pending (FR-026).
    CommitmentOverdue,
    /// SelfModel humility<0.5 && confidence>0.8 self-cognition-block (FR-023).
    SelfCognitionBlocked,
    /// Commitment mention_to_achievement_ratio > 5 (DESIGN §9.5, FR-026).
    AntiTalkSuspect,
}

impl GapKind {
    pub const ALL: [GapKind; 11] = [
        GapKind::IngestNeverConsolidated,
        GapKind::WikiPageWithoutEntities,
        GapKind::OrphanEntity,
        GapKind::UnresolvedRelationship,
        GapKind::QuarantinedNote,
        GapKind::LlmFailure,
        GapKind::DuplicateEntityAlias,
        GapKind::DecisionBlocked,
        GapKind::CommitmentOverdue,
        GapKind::SelfCognitionBlocked,
        GapKind::AntiTalkSuspect,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            GapKind::IngestNeverConsolidated => "ingest_never_consolidated",
            GapKind::WikiPageWithoutEntities => "wiki_page_without_entities",
            GapKind::OrphanEntity => "orphan_entity",
            GapKind::UnresolvedRelationship => "unresolved_relationship",
            GapKind::QuarantinedNote => "quarantined_note",
            GapKind::LlmFailure => "llm_failure",
            GapKind::DuplicateEntityAlias => "duplicate_entity_alias",
            GapKind::DecisionBlocked => "decision_blocked",
            GapKind::CommitmentOverdue => "commitment_overdue",
            GapKind::SelfCognitionBlocked => "self_cognition_blocked",
            GapKind::AntiTalkSuspect => "anti_talk_suspect",
        }
    }
}

/// Per-note processing state machine (data-model §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Processing,
    Completed,
    Failed,
    Quarantined,
}

/// One note's journey through the current cycle (data-model §1).
///
/// The in-memory struct is a per-cycle cache; durable identity comes from
/// `notes_meta.content_hash` + `last_completed_stage` (F3, FR-011), so
/// dedup survives a crash and is never in-memory-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingJob {
    pub note_id: String,
    pub source_path: PathBuf,
    pub checksum: String,
    pub state: JobState,
    pub attempts: u8,
    pub last_error: Option<String>,
}

impl ProcessingJob {
    pub fn new(note_id: impl Into<String>, source_path: PathBuf, checksum: impl Into<String>) -> Self {
        Self {
            note_id: note_id.into(),
            source_path,
            checksum: checksum.into(),
            state: JobState::Queued,
            attempts: 0,
            last_error: None,
        }
    }
}

/// The 10 typed signals routed by MemoryCurator (FR-021, data-model §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedSignalKind {
    Fact,
    Reflection,
    Commitment,
    AntiPattern,
    MentalModel,
    VirtueLog,
    Decision,
    Correction,
    Feedback,
    Belief,
}

/// Bayesian belief record (FR-025, M2→M4 lifecycle).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Belief {
    pub proposition: String,
    pub prior: f64,
    pub posterior: f64,
    pub evidence_count: u32,
    pub last_updated: DateTime<Utc>,
}

/// Prospective commitment (FR-026, M5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    pub what: String,
    pub by_when: Option<DateTime<Utc>>,
    pub review_at: Option<DateTime<Utc>>,
    pub lifecycle: CommitmentLifecycle,
    pub discipline_streak: u32,
}

/// Commitment lifecycle states (FR-026).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentLifecycle {
    Drafted,
    Validated,
    Executing,
    Reviewing,
    Completed,
    Abandoned,
    Pivoted,
}

/// Decision 5-layer record (FR-024).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub goal: String,
    pub facts: Vec<String>,
    pub logic: String,
    pub execution: Vec<String>,
    pub feedback: Option<String>,
}

/// 8-layer SelfModel introspective node (FR-023).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfModelItem {
    pub layer: SelfModelLayer,
    pub label: String,
    pub humility_score: Option<f64>,
    pub optionality_count: Option<u32>,
}

/// SelfModel layer taxonomy (FR-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfModelLayer {
    Knowledge,
    Skill,
    SocialRole,
    SelfConcept,
    Trait,
    Motivation,
    Value,
    Limit,
}

/// Discovery Loop hypothesis (FR-028, filesystem JSON persistence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HypothesisSlug {
    pub slug: String,
    pub hypothesis: String,
    pub gap_kind: GapKind,
    pub confidence: f64,
    pub status: HypothesisStatus,
    pub exploration_prompt: Option<String>,
    pub evidence_refs: Vec<String>,
    pub created_from: String,
}

/// Hypothesis lifecycle (FR-028): hypothesis→exploring→validated|rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    Hypothesis,
    Exploring,
    Validated,
    Rejected,
}

/// Pre-declared target page reserved during Agent planning (FR-031).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphPlaceholder {
    pub slug: String,
    pub status: PlaceholderStatus,
    pub claimed_by: Option<String>,
}

/// Placeholder lifecycle (FR-031): placeholder|claimed|merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceholderStatus {
    Placeholder,
    Claimed,
    Merged,
}

/// Per-ingest token & step budget (FR-032, OCC/CAS enforcement).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopBudget {
    pub max_steps: u32,
    pub max_tokens: u32,
    pub consumed_steps: u32,
    pub consumed_tokens: u32,
}

impl Default for LoopBudget {
    fn default() -> Self {
        Self {
            max_steps: 5,
            max_tokens: 8_000,
            consumed_steps: 0,
            consumed_tokens: 0,
        }
    }
}

impl LoopBudget {
    /// Consume one step; returns false when over budget → pending pool.
    pub fn consume_step(&mut self) -> bool {
        if self.consumed_steps >= self.max_steps {
            return false;
        }
        self.consumed_steps += 1;
        true
    }

    /// Consume tokens; returns false when over budget → pending pool.
    pub fn consume_tokens(&mut self, tokens: u32) -> bool {
        let next = self.consumed_tokens.saturating_add(tokens);
        if next > self.max_tokens {
            return false;
        }
        self.consumed_tokens = next;
        true
    }

    pub fn over_budget(&self) -> bool {
        self.consumed_steps >= self.max_steps || self.consumed_tokens >= self.max_tokens
    }
}

/// ORAV per-source verification node (FR-029, transient).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationNode {
    pub slug_legality: bool,
    pub fact_consistency: bool,
    pub contradiction_detected: bool,
    pub will_retry: bool,
    pub gap_ref: Option<String>,
}

/// RLVR Tier-1 MemoryCard reward sidecar (FR-034) —
/// persisted at `memories/.reward/{card_id}.json`, additive & non-destructive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryReward {
    pub access_count: u64,
    pub downstream_citations: u64,
    pub correction_count: u64,
    pub last_reward_at: Option<DateTime<Utc>>,
}

/// RLVR Tier-1 tool-call outcome record (FR-036) —
/// appended to `sessions/{session_id}/tool_calls.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    pub success: bool,
    pub latency_ms: u64,
    pub error_category: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_budget_defaults_match_spec() {
        let b = LoopBudget::default();
        assert_eq!(b.max_steps, 5);
        assert_eq!(b.max_tokens, 8_000);
        assert!(!b.over_budget());
    }

    #[test]
    fn loop_budget_exhaustion() {
        let mut b = LoopBudget::default();
        for _ in 0..5 {
            assert!(b.consume_step());
        }
        assert!(!b.consume_step());
        assert!(b.over_budget());
    }

    #[test]
    fn gap_kind_roundtrips_snake_case() {
        for kind in GapKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            let back: GapKind = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, &kind);
        }
    }

    #[test]
    fn gap_record_new_assigns_id_and_time() {
        let gap = GapRecord::new(GapKind::OrphanEntity, "cycle-1", "orphan: Foo");
        assert!(!gap.id.is_empty());
        assert_eq!(gap.detail, "orphan: Foo");
        assert!(gap.subject_path.is_none());
    }

    #[test]
    fn cycle_report_serializes_snake_case_outcome() {
        let mut report = LoopCycleReport {
            cycle_id: "c1".into(),
            ..Default::default()
        };
        report.outcome = Some(CycleOutcome::Completed);
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"completed\""));
    }

    #[test]
    fn processing_job_starts_queued() {
        let job = ProcessingJob::new("n1", PathBuf::from("inbox/a.md"), "hash");
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.attempts, 0);
    }
}
