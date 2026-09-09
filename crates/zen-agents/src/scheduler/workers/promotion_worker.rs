//! Promotion routing for validated hypotheses (PD-04, G3).
//!
//! The discover loop never auto-applies learnings (Hybrid C): this worker
//! stages one [`PromotionItem`] per validated [`HypothesisSlug`] into the
//! promotion queue (`logs/promotion-queue.json`, [`PROMOTION_QUEUE_FILE`]).
//! A human confirms each item (C-wave CLI); confirmation dispatches by
//! target — [`PromotionTarget::SkillDraft`] hands off to
//! [`SkillPrecipitator::stage`], the other targets append a durable
//! `promotion-confirmed.jsonl` record for their appliers. Rejection drops
//! the item with an audit line. Every transition emits a `promotion.*`
//! audit event, so a verifier can reconstruct the full promotion history
//! from `audit.jsonl` alone.

use std::fs;
use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use zen_core::paths::ZenPaths;
use zen_memory::{Belief, SourceType};
use zen_vault::distill::hypothesis::load_all;
use zen_vault::distill::types::{GapKind, HypothesisSlug, HypothesisStatus};
use zen_vault::wiki::AtomicWikiWriter;

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use crate::skill_precipitation::{SkillDraft, SkillPrecipitator, append_audit};

/// Cron: daily 4am (after dream 2am + zen-loop windows, before morning brief).
pub const PROMOTION_WORKER_SCHEDULE: &str = "0 0 4 * * *";

/// Promotion queue file in the logs dir (Hybrid C staging channel).
pub const PROMOTION_QUEUE_FILE: &str = "promotion-queue.json";

/// Durable handoff log for confirmed non-skill promotions (appliers poll it).
pub const PROMOTION_CONFIRMED_FILE: &str = "promotion-confirmed.jsonl";

/// Where a validated learning is promoted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionTarget {
    /// Canonical wiki page (graph/page-domain learnings).
    WikiPage,
    /// Belief-lifecycle evidence (judgment-domain learnings).
    BeliefEvidence,
    /// Skill candidate via [`SkillPrecipitator`] (process learnings).
    SkillDraft,
}

/// Lifecycle of a promotion item: staged → confirmed | rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionStatus {
    Staged,
    Confirmed,
    Rejected,
}

/// One staged promotion proposal awaiting human confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionItem {
    /// Deterministic id: `promo-{source slug}`.
    pub id: String,
    /// Hypothesis slug this promotion resolves.
    pub source_slug: String,
    /// Promotion target system.
    pub target: PromotionTarget,
    /// Current lifecycle status.
    pub status: PromotionStatus,
    /// One-line human summary (shown at the confirm gate).
    pub summary: String,
    /// Full hypothesis text (provenance for appliers).
    pub detail: String,
    /// Evidence refs carried from the hypothesis.
    pub evidence_refs: Vec<String>,
    /// Skill triggers (only meaningful for [`PromotionTarget::SkillDraft`]).
    pub triggers: Vec<String>,
    /// Staging wall-clock time.
    pub created_at: DateTime<Utc>,
}

/// Route a validated hypothesis to its promotion target.
///
/// Graph/page-domain gap kinds promote to wiki pages, judgment-domain
/// kinds attach as belief evidence, and stale-ingest process learnings
/// become skill candidates (which face their own Hybrid C skill gate —
/// never auto-promoted).
///
/// # Arguments
///
/// * `kind` — The hypothesis's source gap kind.
pub fn route_target(kind: GapKind) -> PromotionTarget {
    match kind {
        GapKind::WikiPageWithoutEntities
        | GapKind::OrphanEntity
        | GapKind::UnresolvedRelationship
        | GapKind::DuplicateEntityAlias => PromotionTarget::WikiPage,
        GapKind::DecisionBlocked
        | GapKind::CommitmentOverdue
        | GapKind::SelfCognitionBlocked
        | GapKind::AntiTalkSuspect
        | GapKind::QuarantinedNote
        | GapKind::LlmFailure => PromotionTarget::BeliefEvidence,
        GapKind::IngestNeverConsolidated => PromotionTarget::SkillDraft,
    }
}

/// Stages validated hypotheses into the promotion queue (Hybrid C worker).
pub struct PromotionWorker {
    logs_dir: Option<PathBuf>,
    skills_dir: Option<PathBuf>,
    scheduled: Option<&'static str>,
}

impl PromotionWorker {
    /// Build with explicit dirs (test seam; production uses [`ZenPaths`]).
    pub fn new(logs_dir: PathBuf, skills_dir: PathBuf) -> Self {
        Self {
            logs_dir: Some(logs_dir),
            skills_dir: Some(skills_dir),
            scheduled: None,
        }
    }

    /// Build resolving dirs from [`ZenPaths`] at execution time.
    pub fn with_paths() -> Self {
        Self {
            logs_dir: None,
            skills_dir: None,
            scheduled: None,
        }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }

    fn resolve_dirs(&self, paths: &ZenPaths) -> (PathBuf, PathBuf) {
        let logs = self.logs_dir.clone().unwrap_or_else(|| paths.logs());
        let skills = self.skills_dir.clone().unwrap_or_else(|| paths.skills());
        (logs, skills)
    }

    fn queue_path(logs_dir: &Path) -> PathBuf {
        logs_dir.join(PROMOTION_QUEUE_FILE)
    }

    /// Stage one item per validated slug; skips already-queued slugs.
    ///
    /// Returns the count of newly staged items. Emits
    /// `promotion.proposal.staged` per item.
    pub fn stage_from_validated(&self, logs_dir: &Path, slugs: &[HypothesisSlug]) -> Result<usize> {
        Self::with_queue_lock(logs_dir, || {
            self.stage_from_validated_locked(logs_dir, slugs)
        })
    }

    fn stage_from_validated_locked(
        &self,
        logs_dir: &Path,
        slugs: &[HypothesisSlug],
    ) -> Result<usize> {
        let mut queued = Self::read_queue(logs_dir)?;
        let mut staged = 0;
        for slug in slugs
            .iter()
            .filter(|s| s.status == HypothesisStatus::Validated)
        {
            if queued
                .iter()
                .any(|item: &PromotionItem| item.source_slug == slug.slug)
            {
                continue;
            }
            let target = route_target(slug.gap_kind);
            let triggers = if target == PromotionTarget::SkillDraft {
                vec![slug.slug.clone()]
            } else {
                Vec::new()
            };
            queued.push(PromotionItem {
                id: format!("promo-{}", slug.slug),
                source_slug: slug.slug.clone(),
                target,
                status: PromotionStatus::Staged,
                summary: format!("{:?}: {}", target, slug.hypothesis),
                detail: slug.hypothesis.clone(),
                evidence_refs: slug.evidence_refs.clone(),
                triggers,
                created_at: Utc::now(),
            });
            append_audit(
                logs_dir,
                serde_json::json!({
                    "kind": "promotion.proposal.staged",
                    "slug": slug.slug,
                    "target": format!("{target:?}"),
                }),
            )?;
            staged += 1;
        }
        if staged > 0 {
            Self::write_queue(logs_dir, &queued)?;
        }
        info!(staged, "promotion proposals staged, awaiting confirmation");
        Ok(staged)
    }

    /// Pending (staged, unconfirmed) items.
    pub fn pending(&self, logs_dir: &Path) -> Result<Vec<PromotionItem>> {
        Ok(Self::read_queue(logs_dir)?
            .into_iter()
            .filter(|item| item.status == PromotionStatus::Staged)
            .collect())
    }

    /// Confirm an item (Hybrid C gate passed) and dispatch by target.
    ///
    /// Ordering contract: the queue mutation (item removed, status
    /// `Confirmed`) is persisted BEFORE dispatch, so a crash mid-confirm can
    /// never double-confirm (at-most-once). If dispatch fails after the
    /// persist, the item stays confirmed-out of the queue, a
    /// `promotion.proposal.dispatch_failed` audit line is written, and the
    /// error propagates.
    ///
    /// [`PromotionTarget::SkillDraft`] hands off to
    /// [`SkillPrecipitator::stage`]; other targets append the full item to
    /// `promotion-confirmed.jsonl` for their appliers. Emits
    /// `promotion.proposal.confirmed`.
    ///
    /// # Errors
    ///
    /// Returns an error when no staged item with `id` exists, when the
    /// queue file is corrupt, or when dispatch fails after the persist.
    pub fn confirm(
        &self,
        logs_dir: &Path,
        skills_dir: &Path,
        wiki_dir: &Path,
        id: &str,
    ) -> Result<PromotionTarget> {
        Self::with_queue_lock(logs_dir, || {
            self.confirm_locked(logs_dir, skills_dir, wiki_dir, id)
        })
    }

    fn confirm_locked(
        &self,
        logs_dir: &Path,
        skills_dir: &Path,
        wiki_dir: &Path,
        id: &str,
    ) -> Result<PromotionTarget> {
        let mut queued = Self::read_queue(logs_dir)?;
        let pos = queued
            .iter()
            .position(|item| item.id == id && item.status == PromotionStatus::Staged)
            .ok_or_else(|| anyhow::anyhow!("no staged promotion item with id '{id}'"))?;
        let mut item = queued.remove(pos);
        item.status = PromotionStatus::Confirmed;
        Self::write_queue(logs_dir, &queued)?;
        let dispatch = match item.target {
            PromotionTarget::SkillDraft => {
                let precipitator =
                    SkillPrecipitator::new(skills_dir.to_path_buf(), logs_dir.to_path_buf());
                precipitator
                    .stage(&[SkillDraft {
                        name: item.source_slug.clone(),
                        description: item.summary.clone(),
                        triggers: item.triggers.clone(),
                        context_files: Vec::new(),
                        prompt: item.detail.clone(),
                        observations: item.evidence_refs.clone(),
                    }])
                    .map(|_| ())
            }
            PromotionTarget::BeliefEvidence => {
                let path = logs_dir.join(PROMOTION_CONFIRMED_FILE);
                let mut line = serde_json::to_string(&item)?;
                line.push('\n');
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .with_context(|| format!("open promotion handoff log: {}", path.display()))?
                    .write_all(line.as_bytes())
                    .with_context(|| format!("append promotion handoff log: {}", path.display()))
                    .map(|_| ())?;
                Self::apply_belief_evidence(&item);
                Ok(())
            }
            PromotionTarget::WikiPage => {
                let path = logs_dir.join(PROMOTION_CONFIRMED_FILE);
                let mut line = serde_json::to_string(&item)?;
                line.push('\n');
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .with_context(|| format!("open promotion handoff log: {}", path.display()))?
                    .write_all(line.as_bytes())
                    .with_context(|| format!("append promotion handoff log: {}", path.display()))
                    .map(|_| ())?;
                Self::apply_wiki_page(wiki_dir, &item)?;
                Ok(())
            }
        };
        if let Err(e) = dispatch {
            append_audit(
                logs_dir,
                serde_json::json!({
                    "kind": "promotion.proposal.dispatch_failed",
                    "id": item.id,
                    "error": e.to_string(),
                }),
            )?;
            return Err(e);
        }
        append_audit(
            logs_dir,
            serde_json::json!({
                "kind": "promotion.proposal.confirmed",
                "id": item.id,
                "target": format!("{:?}", item.target),
            }),
        )?;
        info!(id, target = ?item.target, "promotion confirmed and dispatched");
        Ok(item.target)
    }

    /// PD-06 fusion: route a confirmed BeliefEvidence promotion into the real
    /// belief surface. Matches a belief whose `id` equals the promotion's
    /// source slug in either belief directory and applies a supporting
    /// Bayesian update, so the evidence flows to wisdom-synth decay, express
    /// reviews, morning briefs, and prompt injection. Best-effort: no
    /// matching belief (or path-detection failure) leaves the JSONL handoff
    /// as the sole record rather than failing the confirm.
    fn apply_belief_evidence(item: &PromotionItem) {
        let Ok(paths) = ZenPaths::detect() else {
            info!(id = %item.id, "promotion: no workspace detected; belief evidence stays audit-only");
            return;
        };
        let mut dirs = vec![paths.wiki().join("wisdom").join("beliefs")];
        if let Some(root) = paths.workspace_root() {
            dirs.push(root.join("memories").join("beliefs"));
        }
        for dir in dirs {
            let Ok(beliefs) = Belief::load_all(&dir) else {
                continue;
            };
            if let Some(mut belief) = beliefs.into_iter().find(|b| b.id == item.source_slug) {
                belief.update(
                    true,
                    SourceType::SelfObservation,
                    Some(item.summary.clone()),
                );
                match belief.save(&dir) {
                    Ok(()) => {
                        info!(belief = %belief.id, "promotion: belief evidence applied");
                        return;
                    }
                    Err(e) => warn!(error = %e, "promotion: belief save failed"),
                }
            }
        }
        info!(slug = %item.source_slug, "promotion: no matching belief; evidence stays audit-only");
    }

    /// PD-06: materialize a confirmed WikiPage promotion as a concept stub
    /// at `wiki/concepts/<subject>.md`, where subject is the source slug's
    /// last segment — the same stem heuristic reverify uses to decide a
    /// page exists, so a confirmation resolves its own hypothesis. Existing
    /// pages are never overwritten. Write failures propagate as dispatch
    /// errors (JSONL audit is already written by the caller).
    fn apply_wiki_page(wiki_dir: &Path, item: &PromotionItem) -> Result<()> {
        let subject = item
            .source_slug
            .rsplit('-')
            .next()
            .unwrap_or(&item.source_slug)
            .to_string();
        let rel = Path::new("concepts").join(format!("{subject}.md"));
        let target = wiki_dir.join(&rel);
        if target.exists() {
            info!(path = %target.display(), "promotion: wiki page already exists; skipping write");
            return Ok(());
        }
        let mut content = format!(
            "---\ntitle: {subject}\ntype: concept\nsource: {}\n---\n\n# {subject}\n\n",
            item.id
        );
        content.push_str(item.detail.trim());
        content.push('\n');
        if !item.evidence_refs.is_empty() {
            content.push_str("\n## Evidence\n");
            for r in &item.evidence_refs {
                content.push_str(&format!("- {r}\n"));
            }
        }
        AtomicWikiWriter::new(wiki_dir)
            .write(&rel, &content)
            .with_context(|| format!("write promoted wiki page: {}", target.display()))?;
        info!(path = %target.display(), "promotion: wiki page created");
        Ok(())
    }

    /// Cross-process advisory lock serializing promotion queue
    /// read-modify-write cycles (stage/confirm/reject). create-new is the
    /// acquisition; a lock file older than 60s is presumed crashed and
    /// broken. Waits up to 2s in 100ms steps, then fails closed. Blocking
    /// sleep is bounded and acceptable here: callers are the 4am worker and
    /// short-lived CLI invocations, not latency-sensitive turns.
    fn with_queue_lock<T>(logs_dir: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
        const WAIT_MS: u32 = 2000;
        const STALE: Duration = Duration::from_secs(60);
        fs::create_dir_all(logs_dir)?;
        let lock_path = logs_dir.join("promotion-queue.lock");
        let mut waited = 0u32;
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&lock_path)
                        .ok()
                        .and_then(|meta| meta.modified().ok())
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > STALE);
                    if stale {
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }
                    if waited >= WAIT_MS {
                        anyhow::bail!(
                            "promotion queue busy: lock held at {} (remove it if no other zen process is running)",
                            lock_path.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    waited += 100;
                }
                Err(e) => return Err(e.into()),
            }
        }
        let result = f();
        let _ = fs::remove_file(&lock_path);
        result
    }

    /// Reject a staged item (drops it with an audit line).
    ///
    /// # Errors
    ///
    /// Returns an error when no staged item with `id` exists.
    pub fn reject(&self, logs_dir: &Path, id: &str) -> Result<bool> {
        Self::with_queue_lock(logs_dir, || self.reject_locked(logs_dir, id))
    }

    fn reject_locked(&self, logs_dir: &Path, id: &str) -> Result<bool> {
        let mut queued = Self::read_queue(logs_dir)?;
        let Some(pos) = queued
            .iter()
            .position(|item| item.id == id && item.status == PromotionStatus::Staged)
        else {
            return Ok(false);
        };
        let item = queued.remove(pos);
        Self::write_queue(logs_dir, &queued)?;
        append_audit(
            logs_dir,
            serde_json::json!({
                "kind": "promotion.proposal.rejected",
                "id": item.id,
            }),
        )?;
        info!(id, "promotion rejected");
        Ok(true)
    }

    /// Read the staged queue. Missing file = empty queue (fresh state).
    ///
    /// Corrupt JSON fails closed (loud error, not silent-empty): this queue
    /// holds human-gate state, and presenting an empty queue would make
    /// staged items vanish without a trace. Stricter than the FR-047
    /// state.json fail-open convention by design.
    fn read_queue(logs_dir: &Path) -> Result<Vec<PromotionItem>> {
        let path = Self::queue_path(logs_dir);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read promotion queue: {}", path.display()))?;
        serde_json::from_str(&content).with_context(|| {
            format!(
                "promotion queue corrupt at {} — fix or remove the file before staging",
                path.display()
            )
        })
    }

    /// Persist the queue atomically: write a sibling `.tmp` then rename, so
    /// a crash mid-write can never leave a truncated queue behind.
    fn write_queue(logs_dir: &Path, items: &[PromotionItem]) -> Result<()> {
        fs::create_dir_all(logs_dir)
            .with_context(|| format!("create logs dir: {}", logs_dir.display()))?;
        let path = Self::queue_path(logs_dir);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(items)?)
            .with_context(|| format!("write promotion queue tmp: {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("swap promotion queue: {}", path.display()))?;
        Ok(())
    }
}

impl Default for PromotionWorker {
    fn default() -> Self {
        Self::with_paths()
    }
}

#[async_trait::async_trait]
impl ZenWorker for PromotionWorker {
    fn id(&self) -> &'static str {
        "promotion"
    }

    fn description(&self) -> &'static str {
        "Stage validated hypotheses into the Hybrid C promotion queue"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or(PROMOTION_WORKER_SCHEDULE)
    }

    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport> {
        let paths = ZenPaths::detect()?;
        self.execute_with_paths(&paths, ctx).await
    }
}

impl PromotionWorker {
    async fn execute_with_paths(
        &self,
        paths: &ZenPaths,
        _ctx: &WorkerContext,
    ) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let (logs_dir, _skills_dir) = self.resolve_dirs(paths);
        let hypotheses_dir = paths.wiki().join("wisdom/hypotheses");
        let slugs = match load_all(&hypotheses_dir) {
            Ok(slugs) => slugs,
            Err(e) => {
                warn!(error = %e, "promotion: hypotheses unreadable; staging nothing");
                Vec::new()
            }
        };
        let staged = self.stage_from_validated(&logs_dir, &slugs)?;
        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: staged,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zen_vault::distill::types::GapKind;

    fn setup() -> (TempDir, PathBuf, PathBuf) {
        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        let skills = dir.path().join("skills");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&skills).unwrap();
        (dir, logs, skills)
    }

    fn validated_slug(slug: &str, kind: GapKind) -> HypothesisSlug {
        HypothesisSlug {
            slug: slug.into(),
            hypothesis: format!("hypothesis about {slug}"),
            gap_kind: kind,
            confidence: 0.7,
            status: HypothesisStatus::Validated,
            exploration_prompt: None,
            evidence_refs: vec!["wiki/a.md".into()],
            created_from: "gap-1".into(),
        }
    }

    #[test]
    fn route_target_maps_gap_domains() {
        assert_eq!(
            route_target(GapKind::OrphanEntity),
            PromotionTarget::WikiPage
        );
        assert_eq!(
            route_target(GapKind::DecisionBlocked),
            PromotionTarget::BeliefEvidence
        );
        assert_eq!(
            route_target(GapKind::IngestNeverConsolidated),
            PromotionTarget::SkillDraft
        );
    }

    #[test]
    fn route_target_covers_all_gap_kinds() {
        let expected: &[(GapKind, PromotionTarget)] = &[
            (GapKind::WikiPageWithoutEntities, PromotionTarget::WikiPage),
            (GapKind::OrphanEntity, PromotionTarget::WikiPage),
            (GapKind::UnresolvedRelationship, PromotionTarget::WikiPage),
            (GapKind::DuplicateEntityAlias, PromotionTarget::WikiPage),
            (GapKind::DecisionBlocked, PromotionTarget::BeliefEvidence),
            (GapKind::CommitmentOverdue, PromotionTarget::BeliefEvidence),
            (
                GapKind::SelfCognitionBlocked,
                PromotionTarget::BeliefEvidence,
            ),
            (GapKind::AntiTalkSuspect, PromotionTarget::BeliefEvidence),
            (GapKind::QuarantinedNote, PromotionTarget::BeliefEvidence),
            (GapKind::LlmFailure, PromotionTarget::BeliefEvidence),
            (
                GapKind::IngestNeverConsolidated,
                PromotionTarget::SkillDraft,
            ),
        ];
        for (kind, target) in expected {
            assert_eq!(
                route_target(*kind),
                *target,
                "unexpected route for {kind:?}"
            );
        }
    }

    #[test]
    fn confirm_belief_evidence_appends_handoff_record() {
        let (_dir, logs, skills) = setup();
        let worker = PromotionWorker::new(logs.clone(), skills.clone());
        let slugs = vec![validated_slug("b1", GapKind::CommitmentOverdue)];
        worker.stage_from_validated(&logs, &slugs).unwrap();
        let target = worker.confirm(&logs, &skills, &logs, "promo-b1").unwrap();
        assert_eq!(target, PromotionTarget::BeliefEvidence);
        let handoff = fs::read_to_string(logs.join(PROMOTION_CONFIRMED_FILE)).unwrap();
        assert!(handoff.contains("promo-b1"));
        assert!(worker.pending(&logs).unwrap().is_empty());
    }

    #[test]
    fn confirm_wiki_page_creates_concept_stub() {
        let (_dir, logs, skills) = setup();
        let worker = PromotionWorker::new(logs.clone(), skills.clone());
        let slugs = vec![validated_slug("w2", GapKind::WikiPageWithoutEntities)];
        worker.stage_from_validated(&logs, &slugs).unwrap();
        let target = worker.confirm(&logs, &skills, &logs, "promo-w2").unwrap();
        assert_eq!(target, PromotionTarget::WikiPage);
        let page = logs.join("concepts").join("w2.md");
        let content = fs::read_to_string(page).unwrap();
        assert!(content.contains("type: concept"));
        assert!(content.contains("promo-w2"));
        assert!(content.contains("## Evidence"));
    }

    #[test]
    fn read_queue_corrupt_json_errors_not_empty() {
        let (_dir, logs, _skills) = setup();
        fs::write(logs.join(PROMOTION_QUEUE_FILE), "not-json{").unwrap();
        let worker = PromotionWorker::new(logs.clone(), PathBuf::from("/tmp/skills"));
        assert!(worker.pending(&logs).is_err());
        assert!(worker.confirm(&logs, &logs, &logs, "promo-x").is_err());
    }

    #[test]
    fn stage_skips_non_validated_and_duplicates() {
        let (_dir, logs, _skills) = setup();
        let worker = PromotionWorker::new(logs.clone(), PathBuf::from("/tmp/skills"));
        let mut exploring = validated_slug("a", GapKind::OrphanEntity);
        exploring.status = HypothesisStatus::Exploring;
        let slugs = vec![
            validated_slug("v1", GapKind::OrphanEntity),
            exploring,
            validated_slug("v2", GapKind::IngestNeverConsolidated),
        ];
        assert_eq!(worker.stage_from_validated(&logs, &slugs).unwrap(), 2);
        // Second run stages nothing (dedup by source_slug).
        assert_eq!(worker.stage_from_validated(&logs, &slugs).unwrap(), 0);
        let pending = worker.pending(&logs).unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().any(|i| i.id == "promo-v1"));
    }

    #[test]
    fn confirm_skill_draft_hands_off_to_precipitator() {
        let (_dir, logs, skills) = setup();
        let worker = PromotionWorker::new(logs.clone(), skills.clone());
        let slugs = vec![validated_slug("s1", GapKind::IngestNeverConsolidated)];
        worker.stage_from_validated(&logs, &slugs).unwrap();
        let target = worker.confirm(&logs, &skills, &logs, "promo-s1").unwrap();
        assert_eq!(target, PromotionTarget::SkillDraft);
        assert!(worker.pending(&logs).unwrap().is_empty());
        // SkillDraft reached the skill queue (Hybrid C downstream gate).
        let queue = fs::read_to_string(logs.join("skill-confirmations.json")).unwrap();
        assert!(queue.contains("s1"));
    }

    #[test]
    fn confirm_wiki_page_appends_handoff_record() {
        let (_dir, logs, skills) = setup();
        let worker = PromotionWorker::new(logs.clone(), skills.clone());
        let slugs = vec![validated_slug("w1", GapKind::OrphanEntity)];
        worker.stage_from_validated(&logs, &slugs).unwrap();
        let target = worker.confirm(&logs, &skills, &logs, "promo-w1").unwrap();
        assert_eq!(target, PromotionTarget::WikiPage);
        let handoff = fs::read_to_string(logs.join(PROMOTION_CONFIRMED_FILE)).unwrap();
        assert!(handoff.contains("promo-w1"));
    }

    #[test]
    fn reject_drops_item_with_audit() {
        let (_dir, logs, _skills) = setup();
        let worker = PromotionWorker::new(logs.clone(), PathBuf::from("/tmp/skills"));
        let slugs = vec![validated_slug("r1", GapKind::OrphanEntity)];
        worker.stage_from_validated(&logs, &slugs).unwrap();
        assert!(worker.reject(&logs, "promo-r1").unwrap());
        assert!(!worker.reject(&logs, "promo-r1").unwrap());
        assert!(worker.pending(&logs).unwrap().is_empty());
        let audit = fs::read_to_string(logs.join("audit.jsonl")).unwrap();
        assert!(audit.contains("promotion.proposal.rejected"));
    }

    #[test]
    fn confirm_unknown_id_errors() {
        let (_dir, logs, skills) = setup();
        let worker = PromotionWorker::new(logs.clone(), skills);
        assert!(worker.confirm(&logs, &logs, &logs, "promo-nope").is_err());
    }

    #[test]
    fn worker_identity_and_schedule() {
        let worker = PromotionWorker::with_paths();
        assert_eq!(worker.id(), "promotion");
        assert_eq!(worker.schedule(), PROMOTION_WORKER_SCHEDULE);
    }

    #[tokio::test]
    async fn execute_stages_from_hypotheses_dir() {
        use zen_vault::distill::hypothesis::save;

        let dir = TempDir::new().unwrap();
        let paths = ZenPaths::for_testing(dir.path().to_path_buf());
        let hyp_dir = paths.wiki().join("wisdom/hypotheses");
        fs::create_dir_all(&hyp_dir).unwrap();
        save(&validated_slug("e1", GapKind::OrphanEntity), &hyp_dir).unwrap();

        let worker = PromotionWorker::with_paths();
        let report = worker
            .execute_with_paths(&paths, &WorkerContext::new(chrono::Utc::now()))
            .await
            .unwrap();
        assert!(report.success);
        assert_eq!(report.fact_count, 1);
        assert_eq!(worker.pending(&paths.logs()).unwrap().len(), 1);
    }
}
