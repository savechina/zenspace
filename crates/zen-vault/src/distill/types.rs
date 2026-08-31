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
    /// Unique cycle identifier (uuid v7, `cycle-{id}`).
    pub cycle_id: String,
    /// Wall-clock start (Utc::now at `zen_loop` tick); None before first tick.
    pub started_at: Option<DateTime<Utc>>,
    /// Wall-clock end; None while Running.
    pub finished_at: Option<DateTime<Utc>>,
    /// Terminal outcome — Completed / Failed / Aborted; None while in-flight.
    pub outcome: Option<CycleOutcome>,
    /// When true, pipeline runs read-only (FR-018); no DB/FS mutations.
    pub dry_run: bool,
    /// Inbox notes loaded before filtering (FR-001).
    pub notes_processed: usize,
    /// Entities durably upserted via NotionService (FR-003/T003).
    pub entities_persisted: usize,
    /// New wiki pages compiled via WikiCompiler (FR-004).
    pub pages_created: usize,
    /// Wiki pages merged via `execute_merge_plans` (FR-016/T015).
    pub merged_count: usize,
    /// Raw notes archived to `vault/archive/<yyyy-mm>/` (FR-006/T005).
    pub archived_count: usize,
    /// Notes moved to `vault/quarantine/` after max retries (FR-010).
    pub quarantined_count: usize,
    /// Notes deferred due to LoopBudget over-limit (FR-032).
    pub pending_count: usize,
    /// Page-lint results run in-cycle (T016, FR-005/SC-003).
    pub lint_orphan_pages: usize,
    /// Broken `[[wikilink]]` targets counted by Linter; 0 = clean (FR-005).
    pub lint_broken_wikilinks: usize,
    /// Detected gaps for next discovery cycle (FR-014, data-model §3).
    pub gaps: Vec<GapRecord>,
    /// Last cycle-level error message, if any; None on success.
    pub last_error: Option<String>,
}

/// One detected knowledge-processing gap (data-model §3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapRecord {
    /// Gap UUID v7 — stable id for HypothesisSlug trace (FR-028).
    pub id: String,
    /// Gap taxonomy variant (FR-014, additive-only).
    pub kind: GapKind,
    /// Vault-relative path of the offending page/note, if any.
    pub subject_path: Option<String>,
    /// Entity name the gap is about, if any.
    pub subject_entity: Option<String>,
    /// Human-readable gap description (e.g., "orphan: Foo").
    pub detail: String,
    /// Detection wall-clock time (Utc::now).
    pub detected_at: DateTime<Utc>,
    /// Originating cycle_id for audit lineage.
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
    /// Note id (notes_meta.note_id) — durable identity for dedup (FR-011 F3).
    pub note_id: String,
    /// Vault-relative inbox path of the note file.
    pub source_path: PathBuf,
    /// SHA-256 content_hash for crash-safe dedup; never in-memory-only.
    pub checksum: String,
    /// Current FSM state (Queued→Processing→Completed/Failed→Quarantined).
    pub state: JobState,
    /// Retry attempts consumed; max via LoopConfig.max_attempts (default 3).
    pub attempts: u8,
    /// Last failure message, if any.
    pub last_error: Option<String>,
}

impl ProcessingJob {
    pub fn new(
        note_id: impl Into<String>,
        source_path: PathBuf,
        checksum: impl Into<String>,
    ) -> Self {
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
    /// Proposition text (e.g., "Rust is fast").
    pub proposition: String,
    /// Prior probability 0.0–1.0 before evidence (default 0.5 if missing).
    pub prior: f64,
    /// Posterior after Bayesian update 0.0–1.0.
    pub posterior: f64,
    /// Number of supporting evidence items.
    pub evidence_count: u32,
    /// Last Bayesian update timestamp.
    pub last_updated: DateTime<Utc>,
}

/// Prospective commitment (FR-026, M5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    /// Commitment description — what is promised.
    pub what: String,
    /// Target deadline; None = no hard deadline (open-ended).
    pub by_when: Option<DateTime<Utc>>,
    /// Daily review trigger time; past due → CommitmentOverdue gap (FR-026).
    pub review_at: Option<DateTime<Utc>>,
    /// Current lifecycle state (Drafted→Validated→Executing…).
    pub lifecycle: CommitmentLifecycle,
    /// Consecutive on-track days; AntiTalk detection when streak+mention ratio fails.
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

/// Decision 5-layer record (FR-024) — goal→facts→logic→execution→feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// Layer-1: decision objective (what to achieve).
    pub goal: String,
    /// Layer-2: supporting facts (evidence).
    pub facts: Vec<String>,
    /// Layer-3: reasoning chain (why this choice).
    pub logic: String,
    /// Layer-4: action items to execute.
    pub execution: Vec<String>,
    /// Layer-5: retrospective; None until closed.
    pub feedback: Option<String>,
}

/// 8-layer SelfModel introspective node (FR-023).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfModelItem {
    /// Introspection layer taxonomy (Knowledge→Limit, 8 variants).
    pub layer: SelfModelLayer,
    /// Human-readable self-assessment label.
    pub label: String,
    /// 0.0–1.0 humility metric; <0.5 with high confidence triggers SelfCognitionBlocked gap.
    pub humility_score: Option<f64>,
    /// Alternative options considered; None = not counted.
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
    /// Filesystem-safe slug (kebab-case, unique).
    pub slug: String,
    /// Hypothesis statement text.
    pub hypothesis: String,
    /// Source gap taxonomy that spawned this hypothesis.
    pub gap_kind: GapKind,
    /// 0.0–1.0 confidence; <0.6 filtered (SC-012).
    pub confidence: f64,
    /// Lifecycle: Hypothesis→Exploring→Validated/Rejected.
    pub status: HypothesisStatus,
    /// LLM exploration prompt, if generated.
    pub exploration_prompt: Option<String>,
    /// Vault-relative evidence file paths.
    pub evidence_refs: Vec<String>,
    /// Originating gap/cycle reference (gap_id or cycle_id).
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
    /// Target page slug (must match wiki page naming convention).
    pub slug: String,
    /// Lifecycle: Placeholder→Claimed→Merged.
    pub status: PlaceholderStatus,
    /// Agent id claiming this placeholder; None if unclaimed.
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
    /// Step ceiling per cycle; default 5 (T032).
    pub max_steps: u32,
    /// Token ceiling per cycle; default 8000 (T032).
    pub max_tokens: u32,
    /// Steps consumed this cycle; >max_steps → pending pool.
    pub consumed_steps: u32,
    /// Tokens consumed this cycle; >max_tokens → pending pool.
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
    /// Slug naming convention passed (no illegal chars).
    pub slug_legality: bool,
    /// Facts cross-referenced against sources OK.
    pub fact_consistency: bool,
    /// Contradiction detected in this verification round.
    pub contradiction_detected: bool,
    /// Verification will be retried next cycle.
    pub will_retry: bool,
    /// Associated gap id, if any.
    pub gap_ref: Option<String>,
}

/// RLVR Tier-1 MemoryCard reward sidecar (FR-034) —
/// persisted at `memories/.reward/{card_id}.json`, additive & non-destructive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryReward {
    /// Total retrieval count for this memory card.
    pub access_count: u64,
    /// References by downstream memories (citation graph).
    pub downstream_citations: u64,
    /// Corrections applied to this memory.
    pub correction_count: u64,
    /// Last RLVR reward update timestamp.
    pub last_reward_at: Option<DateTime<Utc>>,
}

/// RLVR Tier-1 tool-call outcome record (FR-036) —
/// appended to `sessions/{session_id}/tool_calls.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool name identifier (e.g., "fs.read").
    pub tool: String,
    /// Call succeeded without error.
    pub success: bool,
    /// Round-trip latency in milliseconds.
    pub latency_ms: u64,
    /// ErrorCategory classification if failed (AgenticError category).
    pub error_category: Option<String>,
    /// Call timestamp (Utc::now at completion).
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

    #[test]
    fn loop_budget_token_exhaustion_pending_pool() {
        let mut b = LoopBudget::default();
        assert!(b.consume_tokens(4_000));
        assert!(!b.over_budget());
        assert!(b.consume_tokens(3_999));
        assert!(!b.over_budget());
        assert!(b.consume_tokens(1));
        assert!(b.over_budget());
        assert_eq!(b.consumed_tokens, 8_000);
        assert!(!b.consume_tokens(1));
    }

    #[test]
    fn loop_budget_skip_extensions_config_is_default() {
        let mut b = LoopBudget {
            max_steps: 1,
            max_tokens: 100,
            consumed_steps: 0,
            consumed_tokens: 0,
        };
        assert!(b.consume_step());
        assert!(b.over_budget());
        assert!(!b.consume_step());
    }

    #[test]
    fn content_hash_sha256_is_64_hex() {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(b"hello world");
        assert_eq!(hash.len(), 32);
        let hex = hash.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hex, "");
    }
}
