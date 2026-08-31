//! ZenLoopWorker — the knowledge-processing loop worker (005-agentic-loop).
//!
//! Executes the cycle from `docs/specs/005-agentic-loop/contracts/worker.md`,
//! reusing the exact service code paths behind the manual `zen wiki` subcommands:
//! pre-cycle guards → ingest sweep → distill → wisdom hooks (post-distill,
//! T024-T027) → verify → reindex → report+audit → git history (T033).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::jsonl::append_jsonl_line;
use zen_core::paths::ZenPaths;
use zen_vault::distill::{
    CycleOutcome, GapKind, GapRecord, LoopBudget, LoopCycleReport, SourceIngester,
};
use zen_vault::{DistillationPipeline, Reindexer};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

/// Return the path to the last cycle report file.
///
/// # Parameters
/// - `logs_dir` — the workspace `<logs>/` directory (from `ZenPaths::logs()`).
///
/// # Returns
/// Absolute path `<logs_dir>/loop-last-report.json`.
///
/// # Errors
/// This is a pure path-join; it never fails. The file may not exist.
pub fn last_report_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join("loop-last-report.json")
}

/// Return the path to the gap records JSONL file.
///
/// # Parameters
/// - `logs_dir` — the workspace `<logs>/` directory.
///
/// # Returns
/// Absolute path `<logs_dir>/loop-gaps.jsonl`. Each line is a `GapRecord`.
///
/// # Errors
/// Pure path-join; never fails. The file may not exist.
pub fn gaps_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join("loop-gaps.jsonl")
}

/// Return the path to the persistent retry-attempts JSON file.
///
/// Survives per-run worker instances of manual `zen wiki loop run`
/// invocations, so stale-inbox quarantine counters persist across cycles.
///
/// # Parameters
/// - `logs_dir` — the workspace `<logs>/` directory.
///
/// # Returns
/// Absolute path `<logs_dir>/loop-attempts.json`. Maps inbox filenames to
/// their consecutive-failure count.
///
/// # Errors
/// Pure path-join; never fails. The file may not exist.
pub fn attempts_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join("loop-attempts.json")
}

fn load_attempts(logs_dir: &Path) -> HashMap<String, u8> {
    std::fs::read_to_string(attempts_path(logs_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_attempts(logs_dir: &Path, attempts: &HashMap<String, u8>) {
    if let Ok(json) = serde_json::to_string_pretty(attempts) {
        std::fs::write(attempts_path(logs_dir), json).ok();
    }
}

/// The cron-driven knowledge-processing loop worker.
///
/// Executes the full cycle: pre-cycle guards, ingest sweep, distill,
/// wisdom hooks (T024-T027), graph verify, reindex, report + audit,
/// and optional git commit (T033). Registered in `ZenScheduler` and
/// fires on a configurable cron interval (default `0 */5 * * * *`).
pub struct ZenLoopWorker {
    scheduled: Option<&'static str>,
    /// Inbox file names seen in the previous cycle — stale = seen ≥2 cycles
    /// and still present (IngestNeverConsolidated).
    prev_inbox: Mutex<Option<HashSet<String>>>,
    /// Inbox files still present after a cycle → attempt count (T017).
    attempts: Mutex<HashMap<String, u8>>,
    /// T018: pre-cycle checksums; a file whose checksum changes mid-cycle
    /// (user edit during processing) is skipped and re-queued next cycle.
    pre_checksums: Mutex<HashMap<String, String>>,
    cycles: AtomicU32,
    /// `zen wiki loop run --dry-run`: read paths only, no mutations.
    dry_run: bool,
    /// `[agentic.loop] enabled = false` — execute() becomes a no-op.
    cron_enabled: bool,
}

impl ZenLoopWorker {
    /// Create a new worker with default settings.
    ///
    /// # Returns
    /// A `ZenLoopWorker` with default cron schedule (`0 */5 * * * *`),
    /// dry-run disabled, and cron enabled.
    pub fn new() -> Self {
        Self {
            scheduled: None,
            prev_inbox: Mutex::new(None),
            attempts: Mutex::new(HashMap::new()),
            pre_checksums: Mutex::new(HashMap::new()),
            cycles: AtomicU32::new(0),
            dry_run: false,
            cron_enabled: true,
        }
    }

    /// Override the cron schedule expression.
    ///
    /// # Parameters
    /// - `expr` — a cron expression (e.g. `"0 */10 * * * *"`).
    ///
    /// # Returns
    /// The modified worker (builder pattern).
    ///
    /// # Errors
    /// None at construction time; invalid cron expressions are caught at
    /// scheduler registration.
    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }

    /// Enable or disable dry-run mode.
    ///
    /// When dry-run is active, the worker computes the cycle but skips all
    /// distill and reindex mutations — report-only.
    ///
    /// # Parameters
    /// - `dry_run` — `true` to skip mutations, `false` for normal execution.
    ///
    /// # Returns
    /// The modified worker (builder pattern).
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Disable cron registration (worker is registered but never fires).
    ///
    /// Manual invocations via `zen wiki loop run` are unaffected — `run`
    /// constructs its own worker and calls `trigger()` directly.
    ///
    /// # Returns
    /// The modified worker (builder pattern).
    pub fn disabled(mut self) -> Self {
        self.cron_enabled = false;
        self
    }

    /// Stage 2: ingest sweep — `raw/` files into inbox + stale detection.
    async fn ingest_sweep(&self, paths: &ZenPaths, gaps: &mut Vec<GapRecord>, cycle_id: &str) {
        let ingested = SourceIngester::new()
            .ingest(&paths.raw())
            .unwrap_or_else(|e| {
                warn!(error = %e, "ingest sweep failed, continuing cycle");
                0
            });
        if ingested > 0 {
            info!(ingested, "loop: ingest sweep copied raw files into inbox");
        }

        let current: HashSet<String> = inbox_listing(&paths.inbox());
        let mut prev = self.prev_inbox.lock().await;
        if let Some(prev_set) = prev.as_ref() {
            for name in current.difference(prev_set) {
                let mut gap = GapRecord::new(
                    GapKind::IngestNeverConsolidated,
                    cycle_id,
                    format!("inbox file untouched across cycles: {name}"),
                )
                .with_path(name.clone());
                gap.subject_path = Some(name.clone());
                gaps.push(gap);
            }
        }
        *prev = Some(current);
    }

    /// Stage 3b (T024-T027): wisdom composition, post-distill —
    /// typed-signal routing, SelfModel humility gate, Decision CRIT check +
    /// quarantine, belief decay + demotion scan.
    /// Each hook is best-effort: a failure warns and the cycle continues.
    async fn run_wisdom_hooks(
        &self,
        paths: &ZenPaths,
        ctx: &WorkerContext,
        gaps: &mut Vec<GapRecord>,
        report: &mut LoopCycleReport,
    ) {
        // T024: typed-signal routing via the existing MemoryCurator worker
        // (journal → wiki/wisdom + memories/commitments). Runs first so the
        // T026 decision gate sees freshly routed decision pages.
        match super::MemoryCurator::new().execute(ctx).await {
            Ok(curator_report) => {
                info!(
                    signals = curator_report.fact_count,
                    "loop: curator routing complete"
                )
            }
            Err(e) => warn!(error = %e, "loop: curator routing failed (cycle continues)"),
        }

        // T025: SelfModel self-cognition gate (FR-023) — fires only when
        // humility < 0.5 AND confidence > 0.8. Items live under
        // memories/self-model/ (not identity/).
        let self_model_dir = paths.memory().join("self-model");
        match zen_memory::self_model::SelfModelItem::load_all(&self_model_dir) {
            Ok(items) => {
                for item in items {
                    if let Some(humility) = item.humility_score
                        && humility < 0.5
                        && item.confidence > 0.8
                    {
                        gaps.push(
                            GapRecord::new(
                                GapKind::SelfCognitionBlocked,
                                &report.cycle_id,
                                format!(
                                    "self-model `{}` humility {humility:.2} < 0.5 with confidence {:.2} > 0.8 — self-cognition block",
                                    item.name, item.confidence
                                ),
                            )
                            .with_entity(item.name.clone()),
                        );
                    }
                }
            }
            Err(e) => debug!(error = %e, "loop: no self-model items to evaluate"),
        }

        // T026: Decision quality gate (FR-024) — real markdown-parsed
        // decisions checked against 7 principles + 10 anti-patterns; CRIT
        // quarantines the page.
        self.run_decision_quality_gate(paths, gaps, report);

        // T027: belief lifecycle (FR-025) — 90-day decay first, then the
        // posterior < 0.2 demotion scan (preserve, never delete).
        let beliefs_dir = paths.vault().join("wiki").join("wisdom").join("beliefs");
        if beliefs_dir.is_dir() {
            match zen_memory::belief::Belief::load_all(&beliefs_dir) {
                Ok(mut beliefs) => {
                    let decayed = zen_memory::belief::apply_decay_all(&mut beliefs, ctx.now);
                    if decayed > 0 {
                        info!(decayed, "loop: applied belief decay");
                        for belief in &beliefs {
                            if let Err(e) = belief.save(&beliefs_dir) {
                                warn!(belief = %belief.id, error = %e, "loop: failed to save decayed belief");
                            }
                        }
                    }
                    let demote_dir = paths.memory().join("demoted-beliefs");
                    for belief in &beliefs {
                        if belief.posterior < 0.2 {
                            let slug = format!("{}.md", belief.id);
                            let from = beliefs_dir.join(&slug);
                            if from.exists() {
                                std::fs::create_dir_all(&demote_dir).ok();
                                if std::fs::rename(&from, demote_dir.join(&slug)).is_ok() {
                                    info!(belief = %belief.id, "loop: belief demoted to M2");
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "loop: belief lifecycle scan failed (cycle continues)")
                }
            }
        }

        // T028/T033 (FR-026/FR-032): commitment gap scan — discovers overdue
        // commitments and feeds them into the cycle's gap vec for persistence
        // to loop-gaps.jsonl and surfacing via `zen wiki loop gaps`.
        let commitment_gaps = super::commitment_tracker::scan_commitment_gaps(paths, ctx.now);
        if !commitment_gaps.is_empty() {
            info!(
                count = commitment_gaps.len(),
                "loop: commitment gap scan found overdue commitments"
            );
            gaps.extend(commitment_gaps);
        }
    }

    /// T026 (FR-024): scan `wiki/wisdom/decisions/*.md`, parse each via the
    /// real zen-memory Decision markdown parser, and run BOTH the 10
    /// anti-pattern checks (`check_all`) and the 7-principles check
    /// (`check_decision_principles`). CRIT violations block promotion and
    /// move the page to `vault/archive/quarantine/` (mirroring the T017
    /// note-quarantine mechanism) with a `DecisionBlocked` gap; non-CRIT
    /// principle violations are trace warnings only. Falls back to journal
    /// `kind: decision` blocks when the decisions directory is empty — those
    /// emit gaps without quarantine (no page exists yet). Idempotent: pages
    /// already under quarantine are not re-checked.
    fn run_decision_quality_gate(
        &self,
        paths: &ZenPaths,
        gaps: &mut Vec<GapRecord>,
        report: &mut LoopCycleReport,
    ) {
        use zen_memory::decision_check::check_all;
        use zen_memory::quality_gate::check_decision_principles;

        let decisions_dir = paths.vault().join("wiki").join("wisdom").join("decisions");
        let quarantine_dir = paths.archive().join("quarantine");

        if decisions_dir.is_dir()
            && let Ok(decisions) = zen_memory::decision::Decision::load_all(&decisions_dir)
        {
            for decision in decisions {
                let anti_report = check_all(&decision);
                let principles = check_decision_principles(&decision);
                if !principles.all_passed && !anti_report.has_crit {
                    warn!(
                        decision = %decision.id,
                        "loop: decision fails {}/7 principles (non-CRIT, promotion review advised)",
                        principles.failed_count
                    );
                }
                if !anti_report.has_crit {
                    continue;
                }
                let from = decisions_dir.join(format!("{}.md", decision.id));
                let Some(file_name) = from.file_name().map(|n| n.to_os_string()) else {
                    continue;
                };
                if !from.exists() || quarantine_dir.join(&file_name).exists() {
                    continue;
                }
                std::fs::create_dir_all(&quarantine_dir).ok();
                let to = quarantine_dir.join(&file_name);
                if std::fs::rename(&from, &to).is_ok() {
                    let crit_patterns: Vec<&str> = anti_report
                        .violations
                        .iter()
                        .filter(|v| v.severity == zen_memory::decision::Severity::Crit)
                        .map(|v| v.pattern_id.as_str())
                        .collect();
                    warn!(
                        decision = %decision.id,
                        crit = ?crit_patterns,
                        "loop: decision quarantined (CRIT anti-pattern)"
                    );
                    gaps.push(
                        GapRecord::new(
                            GapKind::DecisionBlocked,
                            &report.cycle_id,
                            format!(
                                "decision `{}` CRIT anti-patterns [{}] — quarantined",
                                decision.id,
                                crit_patterns.join(", ")
                            ),
                        )
                        .with_path(&to),
                    );
                }
            }
            return;
        }

        // Fallback: journal `kind: decision` blocks when no decision pages
        // exist (payload format matches the MemoryCurator routing contract).
        let journal_dir = paths.memory().join("journal");
        let Ok(entries) = std::fs::read_dir(&journal_dir) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !content.to_lowercase().contains("kind: decision") {
                continue;
            }
            for raw in journal_decision_payloads(&content) {
                let text = raw.split("|||").next().unwrap_or_default().trim();
                if text.is_empty() {
                    continue;
                }
                let id = zen_memory::decision::Decision::slugify_title(text);
                let decision = zen_memory::decision::Decision::new(
                    id.clone(),
                    text.to_string(),
                    "journal".to_string(),
                );
                if check_all(&decision).has_crit {
                    gaps.push(
                        GapRecord::new(
                            GapKind::DecisionBlocked,
                            &report.cycle_id,
                            format!("journal decision `{id}` has CRIT anti-pattern violations"),
                        )
                        .with_path(&path),
                    );
                }
            }
        }
    }
}

/// Convenience: `ZenLoopWorker::default()` returns `ZenLoopWorker::new()`.
impl Default for ZenLoopWorker {
    fn default() -> Self {
        Self::new()
    }
}

fn inbox_listing(inbox: &Path) -> HashSet<String> {
    std::fs::read_dir(inbox)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// `ZenWorker` implementation — the main cycle entry point.
///
/// `execute()` runs the full 6-stage pipeline: pre-cycle guards, ingest
/// sweep, distill, wisdom hooks, graph verify + reindex, report + audit.
/// Returns a `WorkerReport` with cycle metadata.
#[async_trait::async_trait]
impl ZenWorker for ZenLoopWorker {
    fn id(&self) -> &'static str {
        "zen-loop"
    }

    fn description(&self) -> &'static str {
        "Knowledge-processing loop: ingest sweep → distill → verify → reindex → report"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */5 * * * *")
    }

    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport> {
        if !self.cron_enabled {
            return Ok(WorkerReport {
                worker_id: "zen-loop".to_string(),
                success: true,
                fact_count: 0,
                duration_ms: 0,
                llm_cost_usd: 0.0,
            });
        }
        let started = Instant::now();
        let cycle_id = uuid::Uuid::now_v7().to_string();
        let paths = ZenPaths::detect().context("loop: cannot detect ZEN paths")?;
        let config = load_config()?;
        let loop_cfg = &config.agentic.loop_cfg;
        let logs_dir = paths.logs().to_path_buf();
        std::fs::create_dir_all(&logs_dir).ok();

        let mut report = LoopCycleReport {
            cycle_id: cycle_id.clone(),
            started_at: Some(ctx.now),
            dry_run: false,
            ..LoopCycleReport::default()
        };

        // ── Stage 1: pre-cycle guards ─────────────────────────────────────
        let free = fs2::available_space(paths.vault()).unwrap_or(u64::MAX);
        if free < loop_cfg.min_free_bytes_or_default() {
            report.outcome = Some(CycleOutcome::Aborted);
            report.last_error = Some(format!(
                "free space {free} < min_free_bytes {}",
                loop_cfg.min_free_bytes_or_default()
            ));
            persist_report_and_audit(&paths, &logs_dir, &report).await?;
            return Ok(worker_report(&report, started));
        }

        // ── Stage 2: ingest sweep (+ stale detection) ─────────────────────
        let mut gaps: Vec<GapRecord> = Vec::new();
        self.ingest_sweep(&paths, &mut gaps, &cycle_id).await;

        // T018: snapshot pre-cycle checksums for the concurrent-modification
        // gate — a file edited mid-cycle is left for the next cycle.
        let inbox_before = inbox_listing(&paths.inbox());
        {
            let mut pre = self.pre_checksums.lock().await;
            pre.clear();
            for name in &inbox_before {
                let path = paths.inbox().join(name);
                if let Ok(bytes) = std::fs::read(&path) {
                    let checksum = zen_vault::ChangeDetector::compute_checksum(
                        &String::from_utf8_lossy(&bytes),
                    );
                    pre.insert(name.clone(), checksum);
                }
            }
        }

        // ── Stage 3: distill (same code path as `zen wiki distill`) ───────
        if self.dry_run {
            info!("loop: dry-run — skipping distill/reindex mutations");
            report.outcome = Some(CycleOutcome::Completed);
            report.dry_run = true;
            report.gaps = gaps;
            persist_report_and_audit(&paths, &logs_dir, &report).await?;
            return Ok(worker_report(&report, started));
        }
        let db_path = paths.data().join("state.db");
        let db = match zen_repo::SqliteClient::open_lazy(&db_path).await {
            Ok(db) => Some(db),
            Err(e) => {
                warn!(error = %e, "loop: DB unavailable — distill runs without graph persist");
                None
            }
        };
        let pipeline = DistillationPipeline::new();
        // T033 (FR-032): per-cycle LoopBudget — constructs from config defaults
        // since LoopConfig doesn't expose max_steps/max_tokens fields yet.
        let mut budget = LoopBudget::default();
        let distill = pipeline
            .run_scoped(
                &paths.inbox(),
                &paths.wiki(),
                &paths.archive(),
                &logs_dir,
                db.as_ref(),
                Some(&mut budget),
            )
            .await;

        match distill {
            Ok(outcome) => {
                report.notes_processed = outcome.report.notes_processed;
                report.entities_persisted = outcome.report.entities_persisted;
                report.pages_created = outcome.report.wiki_pages_created;
                report.merged_count = outcome.report.merged_count;
                report.archived_count = outcome.report.migrated_files.len();
                report.pending_count = outcome.pending_count;
                gaps.extend(outcome.gaps);
            }
            Err(e) => {
                report.outcome = Some(CycleOutcome::Failed);
                report.last_error = Some(format!("distill stage: {e:#}"));
                gaps.append(&mut report.gaps);
                report.gaps = gaps;
                persist_report_and_audit(&paths, &logs_dir, &report).await?;
                return Ok(worker_report(&report, started));
            }
        }

        // ── Stage 3b: wisdom hooks (T024-T027, post-distill) ──────────────
        // Typed-signal routing runs immediately after distill (FR-021) so
        // routed wisdom surfaces are verified by the Stage 4 graph checks
        // and reindexed in Stage 5 within the same cycle.
        self.run_wisdom_hooks(&paths, ctx, &mut gaps, &mut report)
            .await;

        // ── Stage 4: verify (T016) — GraphIntegrityVerifier + page lint ───
        if let Some(db) = db.as_ref() {
            let verifier = zen_vault::GraphIntegrityVerifier::new(db, &cycle_id);
            let inventory = zen_vault::wiki_page_inventory(&paths.wiki());
            match verifier.verify(&inventory).await {
                Ok(mut graph_gaps) => gaps.append(&mut graph_gaps),
                Err(e) => warn!(error = %e, "loop: graph verify failed (cycle continues)"),
            }
        }
        match zen_vault::Linter::new().run(&paths.wiki()) {
            Ok(lint) => {
                report.lint_orphan_pages = lint.orphan_pages.len();
                report.lint_broken_wikilinks = lint.broken_wikilinks.len();
            }
            Err(e) => warn!(error = %e, "loop: page lint failed (cycle continues)"),
        }

        // T017: inbox-empty guarantee — leftover notes retry next cycles,
        // quarantined after max_attempts (FR-006/FR-010).
        {
            let max_attempts = loop_cfg.max_attempts_or_default();
            let mut attempts = self.attempts.lock().await;
            *attempts = load_attempts(&logs_dir);
            let leftover = inbox_listing(&paths.inbox());
            let mut quarantined = 0usize;
            for name in leftover {
                let pre = self.pre_checksums.lock().await;
                let edited_mid_cycle = pre
                    .get(&name)
                    .map(|before| {
                        std::fs::read(paths.inbox().join(&name))
                            .map(|b| {
                                zen_vault::ChangeDetector::compute_checksum(
                                    &String::from_utf8_lossy(&b),
                                ) != *before
                            })
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                drop(pre);
                if edited_mid_cycle {
                    info!(file = %name, "loop: file changed mid-cycle — re-queued next cycle (T018)");
                    attempts.remove(&name);
                    continue;
                }
                let count = attempts.entry(name.clone()).or_insert(0);
                *count += 1;
                if u32::from(*count) >= max_attempts {
                    let quarantine_dir = paths.archive().join("quarantine");
                    std::fs::create_dir_all(&quarantine_dir).ok();
                    let from = paths.inbox().join(&name);
                    let to = quarantine_dir.join(&name);
                    if std::fs::rename(&from, &to).is_ok() {
                        quarantined += 1;
                        gaps.push(
                            GapRecord::new(
                                GapKind::QuarantinedNote,
                                &cycle_id,
                                format!("note failed {max_attempts} cycles — quarantined"),
                            )
                            .with_path(&to),
                        );
                        attempts.remove(&name);
                    }
                }
            }
            save_attempts(&logs_dir, &attempts);
            report.quarantined_count = quarantined;
        }

        // ── Stage 5: reindex (checksum-gated, same path as `zen wiki reindex`) ──
        // without_embeddings: the 5-min hot chain stays offline-safe; vec0
        // embeddings belong to the batch rhythms (wiki-compiler 30min /
        // memvid-indexer nightly) per FR-027 rhythm separation.
        if let Some(db) = db {
            let reindexer = Reindexer::with_client(db).without_embeddings();
            match reindexer.reindex(&paths.vault()).await {
                Ok(rr) => info!(files_updated = rr.files_updated, "loop: reindex complete"),
                Err(e) => warn!(error = %e, "loop: reindex failed (cycle continues)"),
            }
        }

        // ── Stage 5b: Discovery Loop hypotheses (T029, FR-028) ─────────────
        // Jeff Dean Discovery Loop: this cycle's accumulated gaps (distill,
        // wisdom hooks, commitment tracker, graph verify) incubate into
        // HypothesisSlug records at wiki/wisdom/hypotheses/. Idempotent
        // per slug (save merges evidence and keeps the higher status), so
        // repeated cycles converge instead of duplicating.
        {
            let hypotheses_dir = paths.vault().join("wiki/wisdom/hypotheses");
            let slugs = zen_vault::distill::generate_from_gaps(&gaps, ctx.now);
            for slug in &slugs {
                if let Err(e) = zen_vault::distill::save(slug, &hypotheses_dir) {
                    warn!(error = %e, slug = %slug.slug, "loop: hypothesis save failed");
                }
            }
            if !slugs.is_empty() {
                info!(
                    generated = slugs.len(),
                    exploring = slugs
                        .iter()
                        .filter(|s| matches!(
                            s.status,
                            zen_vault::distill::HypothesisStatus::Exploring
                        ))
                        .count(),
                    "loop: hypotheses generated from gaps"
                );
            }
        }

        // ── Stage 6: report + audit ───────────────────────────────────────
        let _ = self.cycles.fetch_add(1, Ordering::Relaxed);
        report.outcome = Some(CycleOutcome::Completed);
        report.gaps = gaps;
        persist_report_and_audit(&paths, &logs_dir, &report).await?;

        // T033 (FR-032): local-first git history for the cycle's mutations.
        commit_cycle_to_git(&paths, &report);

        Ok(worker_report(&report, started))
    }
}

fn worker_report(report: &LoopCycleReport, started: Instant) -> WorkerReport {
    WorkerReport {
        worker_id: "zen-loop".to_string(),
        success: report.outcome == Some(CycleOutcome::Completed),
        fact_count: report.notes_processed,
        duration_ms: started.elapsed().as_millis() as u64,
        llm_cost_usd: 0.0,
    }
}

/// Extract decision payload lines (`- text|||context|||ev`) that follow a
/// `kind: decision` tag in a journal entry (T026 journal fallback).
fn journal_decision_payloads(content: &str) -> Vec<String> {
    let mut payloads = Vec::new();
    let mut in_decision_block = false;
    for line in content.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        if lower.starts_with("kind:") {
            in_decision_block = lower.strip_prefix("kind:").is_some_and(|v| {
                v.trim().trim_matches('"').eq_ignore_ascii_case("decision")
                    || v.trim().trim_matches('"').eq_ignore_ascii_case("decisions")
            });
            continue;
        }
        if trimmed.starts_with("## ") {
            in_decision_block = false;
            continue;
        }
        if in_decision_block
            && let Some(item) = trimmed.strip_prefix("- ")
            && !item.trim().is_empty()
            && !item.trim().starts_with("_(no ")
        {
            payloads.push(item.trim().to_string());
        }
    }
    payloads
}

/// T033 (FR-032): commit the cycle's mutations to the workspace git repo.
///
/// Scope logic:
/// - Functionality: appends a local-first history commit per cycle
/// - User impact: `git log` in the workspace shows loop mutations
/// - Default: skipped when no workspace root is detected or the workspace
///   is not inside a git work tree (never initializes a repo)
/// - Interaction: dry-run cycles return before this point; failures are
///   logged (`nothing to commit` at debug, real errors at warn) and never
///   fail the cycle
fn commit_cycle_to_git(paths: &ZenPaths, report: &LoopCycleReport) {
    let Some(workspace_root) = paths.workspace_root() else {
        debug!("loop: no workspace root — skipping git commit");
        return;
    };

    let inside = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    let inside_work_tree = matches!(
        inside,
        Ok(out) if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true"
    );
    if !inside_work_tree {
        debug!(workspace = %workspace_root.display(), "loop: workspace not a git work tree — skipping git commit");
        return;
    }

    let message = format!(
        "loop: {} notes={} pages={} merged={} quarantined={}",
        report.cycle_id,
        report.notes_processed,
        report.pages_created,
        report.merged_count,
        report.quarantined_count
    );

    let add = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["add", "-A"])
        .output();
    match add {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            warn!(
                workspace = %workspace_root.display(),
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "loop: git add failed (cycle continues)"
            );
            return;
        }
        Err(e) => {
            warn!(error = %e, "loop: git add failed to spawn (cycle continues)");
            return;
        }
    }

    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["commit", "-m", &message])
        .output();
    match commit {
        Ok(out) if out.status.success() => {
            info!(workspace = %workspace_root.display(), "loop: git history committed");
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if stderr.contains("nothing to commit") || stderr.contains("no changes added to commit")
            {
                debug!("loop: git commit skipped — nothing to commit");
            } else {
                warn!(
                    workspace = %workspace_root.display(),
                    stderr = %stderr.trim(),
                    "loop: git commit failed (cycle continues)"
                );
            }
        }
        Err(e) => warn!(error = %e, "loop: git commit failed to spawn (cycle continues)"),
    }
}

/// Persist the cycle report (`loop-last-report.json`), append gaps
/// (`loop-gaps.jsonl`), and emit the audit event (`audit.jsonl`).
async fn persist_report_and_audit(
    paths: &ZenPaths,
    logs_dir: &Path,
    report: &LoopCycleReport,
) -> Result<()> {
    let report_path = last_report_path(logs_dir);
    let json = serde_json::to_string_pretty(report)?;
    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&report_path, json)
        .with_context(|| format!("write cycle report: {}", report_path.display()))?;

    let gaps_file = gaps_path(logs_dir);
    for gap in &report.gaps {
        append_jsonl_line(&gaps_file, gap)
            .with_context(|| format!("append gap: {}", gaps_file.display()))?;
    }

    let audit = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "kind": "loop.cycle.completed",
        "cycle_id": report.cycle_id,
        "outcome": report.outcome.map(|o| o.as_str()).unwrap_or("unknown"),
        "notes_processed": report.notes_processed,
        "entities_persisted": report.entities_persisted,
        "pages_created": report.pages_created,
        "archived_count": report.archived_count,
        "gaps": report.gaps.len(),
        "vault": paths.vault().to_string_lossy(),
    });
    let audit_path = logs_dir.join("audit.jsonl");
    append_jsonl_line(&audit_path, &audit)
        .with_context(|| format!("append audit: {}", audit_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_metadata_matches_contract() {
        let worker = ZenLoopWorker::new();
        assert_eq!(worker.id(), "zen-loop");
        assert_eq!(worker.schedule(), "0 */5 * * * *");
        assert!(!worker.description().is_empty());
    }

    #[test]
    fn worker_with_schedule_override() {
        let worker = ZenLoopWorker::new().with_schedule("0 */10 * * * *");
        assert_eq!(worker.schedule(), "0 */10 * * * *");
    }

    #[test]
    fn inbox_listing_empty_on_missing_dir() {
        let set = inbox_listing(Path::new("/nonexistent-zen-inbox"));
        assert!(set.is_empty());
    }

    #[test]
    fn journal_decision_payloads_extracts_kind_blocks() {
        let content = "---\nsession_id: s1\nkind: decision\n---\n\n- Use SQLite|||offline|||cheap\n\n## Facts\n\n- a fact\n\nkind: decisions\n\n- Ship dark mode|||demand\n- _(no decisions extracted)_\n";
        let payloads = journal_decision_payloads(content);
        assert_eq!(payloads.len(), 2);
        assert!(payloads[0].starts_with("Use SQLite"));
        assert!(payloads[1].starts_with("Ship dark mode"));
    }

    #[test]
    fn journal_decision_payloads_empty_without_tag() {
        let content = "---\nsession_id: s1\n---\n\n## Facts\n\n- plain fact\n";
        assert!(journal_decision_payloads(content).is_empty());
    }
}
