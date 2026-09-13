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

use zen_core::config::{HostSourceContext, LoopConfig, load_config};
use zen_core::jsonl::append_jsonl_line;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmRouter, Provider, TaskRequirements};
use zen_vault::distill::{
    CycleOutcome, GapKind, GapRecord, LoopBudget, LoopCycleReport, SourceIngester,
};
use zen_vault::graph_router::{DocExtractor, is_routable_extension};
use zen_vault::{DistillationPipeline, Reindexer};

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::promotion_worker::{PromotionTarget, PromotionWorker};

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

    /// Stage 2 (FR-033, T043-T045): host-source governance sweep.
    ///
    /// Scope logic (Constitution XV):
    /// - Functionality: stages new `md`/`txt` files from each configured
    ///   `host_sources` entry into `vault/inbox/_incoming/{host_hash}/`,
    ///   preserves doc-track originals under `vault/raw/{host_hash}/`
    ///   (stamped frontmatter), promotes staging into the inbox, and emits
    ///   the `loop.host.ingested` audit event with sensitivity-routed
    ///   model tier.
    /// - User impact: configured host dirs feed the knowledge pipeline each
    ///   cycle; code-track repos are indexed in place (never copied);
    ///   Private sources route to local models unless `allow_cloud = true`.
    /// - Default: `host_sources` empty → feature off, no FS or audit writes.
    /// - Interaction: runs BEFORE `ingest_sweep` so promoted files enter the
    ///   same inbox stale-detection (prev_inbox) flow as raw copies; routing
    ///   (Stage 3a) additionally honors the `raw_graph_routing` master switch.
    async fn sweep_host_sources(
        &self,
        paths: &ZenPaths,
        loop_cfg: &LoopConfig,
        host_contexts: &[HostSourceContext],
        config: &zen_core::config::ZenConfig,
        cycle_id: &str,
    ) {
        if host_contexts.is_empty() {
            return;
        }
        let inbox = paths.inbox();
        let doc_extractor = DocExtractor::new();
        let mut staged_by_hash: HashMap<String, Vec<String>> = HashMap::new();

        for ctx in host_contexts {
            if !ctx.host_path.is_dir() {
                warn!(
                    source = %ctx.host_path.display(),
                    "loop: host_source path missing — skipped"
                );
                continue;
            }
            let staged = stage_host_dir(ctx, loop_cfg, &inbox);
            if !staged.is_empty() {
                info!(
                    source = %ctx.host_path.display(),
                    staged = staged.len(),
                    "loop: host source swept into _incoming staging"
                );
            }
            // T044 doc track: preserve originals read-only under
            // raw/{host_hash}/ with provenance frontmatter stamps.
            if ctx.preserves_raw() {
                for name in &staged {
                    let host_file = ctx.host_path.join(name);
                    match std::fs::read_to_string(&host_file) {
                        Ok(content) => {
                            if let Err(e) = doc_extractor.ensure_raw_copy(
                                &paths.raw(),
                                ctx,
                                &host_file,
                                &content,
                            ) {
                                warn!(
                                    error = %e,
                                    file = %host_file.display(),
                                    "loop: host raw preservation failed (continues)"
                                );
                            }
                        }
                        Err(e) => warn!(
                            error = %e,
                            file = %host_file.display(),
                            "loop: host file unreadable — raw copy skipped"
                        ),
                    }
                }
            }
            staged_by_hash.insert(ctx.host_hash.clone(), staged);
        }

        let promoted_by_hash = SourceIngester::new()
            .promote_incoming(&inbox)
            .unwrap_or_else(|e| {
                warn!(error = %e, "loop: host staging promotion failed");
                HashMap::new()
            });

        // T045 (FR-033/F6): per-source audit with sensitivity-routed tier.
        // Cloud extraction requires allow_cloud=true; otherwise routing runs
        // through the router's existing enforce_sensitivity local-only gate.
        let mut router: Option<DefaultRouter> = None;
        for ctx in host_contexts {
            let staged = staged_by_hash
                .get(&ctx.host_hash)
                .map(Vec::len)
                .unwrap_or(0);
            let promoted = promoted_by_hash.get(&ctx.host_hash).copied().unwrap_or(0);
            if staged == 0 && promoted == 0 {
                continue;
            }
            let model_tier = {
                let router = router.get_or_insert_with(|| DefaultRouter::from_agentic(config));
                resolve_model_tier(router, ctx)
            };
            info!(
                source = %ctx.host_path.display(),
                sensitivity = %ctx.sensitivity,
                model_tier,
                staged,
                promoted,
                "loop: host source ingested"
            );
            let audit = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "kind": "loop.host.ingested",
                "cycle_id": cycle_id,
                "source": ctx.host_path.to_string_lossy(),
                "sensitivity": ctx.sensitivity.to_string(),
                "model_tier": model_tier,
                "worker_type": ctx.worker_type.map(|k| k.as_str()),
                "raw_policy": ctx.raw_policy.as_str(),
                "allow_cloud": ctx.allow_cloud,
                "staged": staged,
                "promoted": promoted,
            });
            let audit_path = paths.logs().join("audit.jsonl");
            if let Err(e) = append_jsonl_line(&audit_path, &audit) {
                warn!(error = %e, "loop: host audit append failed");
            }
        }
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

/// Stage new host files into `_incoming/{host_hash}/` (T043).
///
/// A file is staged only when neither the pending slot nor the promoted
/// ledger (`promoted/{name}`) holds it — the staging tree is the sweep's
/// persistent seen-set, so each host file enters the pipeline exactly once.
/// Only top-level `md`/`txt` files are swept; config `skip_extensions`
/// applies.
fn stage_host_dir(ctx: &HostSourceContext, loop_cfg: &LoopConfig, inbox: &Path) -> Vec<String> {
    let staging = inbox.join("_incoming").join(&ctx.host_hash);
    let promoted_dir = staging.join("promoted");
    let mut staged = Vec::new();
    let Ok(entries) = std::fs::read_dir(&ctx.host_path) else {
        return staged;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !(ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("txt")) {
            continue;
        }
        if loop_cfg.is_extension_skipped(ext) {
            continue;
        }
        if staging.join(name).exists() || promoted_dir.join(name).exists() {
            continue;
        }
        std::fs::create_dir_all(&staging).ok();
        match std::fs::copy(&path, staging.join(name)) {
            Ok(_) => staged.push(name.to_string()),
            Err(e) => warn!(
                error = %e,
                file = %path.display(),
                "loop: host staging copy failed (continues)"
            ),
        }
    }
    staged
}

/// T045 (FR-033/F6): resolve the effective model tier for a host source via
/// the existing router sensitivity enforcement.
///
/// - `allow_cloud = false` (default): routing requests `Sensitivity::Private`
///   so `enforce_sensitivity` forces a local provider — cloud is unreachable
///   regardless of the source's own classification.
/// - `allow_cloud = true`: explicit per-source opt-in overrides the local-only
///   guard; the routed provider decides the tier.
///
/// # Returns
/// `"local"`, `"cloud"`, or `"local-unavailable"` (enforcement could not
/// find a reachable local provider — recorded honestly in the audit event;
/// extraction continues heuristically offline).
fn resolve_model_tier(router: &DefaultRouter, host: &HostSourceContext) -> &'static str {
    let route_sensitivity = if host.allow_cloud {
        Sensitivity::Public
    } else {
        Sensitivity::Private
    };
    let requirements = TaskRequirements {
        max_tokens: None,
        sensitivity: route_sensitivity,
        preferred_model: None,
        budget_limit: None,
    };
    match router.route(&requirements) {
        Ok(Provider::Ollama) => "local",
        Ok(_) if host.allow_cloud => "cloud",
        Ok(_) => "local",
        Err(_) => {
            warn!(
                source = %host.host_path.display(),
                "loop: local LLM unavailable for Private host extraction — tier recorded as local-unavailable"
            );
            "local-unavailable"
        }
    }
}

/// Persist reverify-rejected hypotheses as negative-space records under
/// `wiki/wisdom/rejected/` (FR-040) so the loop never re-proposes falsified
/// claims. Best-effort: a record failure is logged, never fails the cycle.
fn record_rejected_hypotheses(paths: &ZenPaths, rejected: &[zen_memory::RejectedHypothesis]) {
    for r in rejected {
        match r.record(paths) {
            Ok(path) => info!(
                path = %path.display(),
                "loop: hypothesis rejected — negative-space record persisted"
            ),
            Err(e) => warn!(error = %e, "loop: rejected hypothesis record failed (non-fatal)"),
        }
    }
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

        // FR-033 (T043): resolve governed host sources once per cycle.
        // Invalid entries are warn-and-skipped (fail-closed validation in
        // HostSourceConfig::resolve), never silently reinterpreted.
        let workspace_id = paths
            .workspace_root()
            .map(|p| p.to_string_lossy().into_owned());
        let host_contexts: Vec<HostSourceContext> = loop_cfg
            .host_sources
            .iter()
            .filter_map(|source| match source.resolve(workspace_id.as_deref()) {
                Ok(ctx) => Some(ctx),
                Err(reason) => {
                    warn!(
                        source = %source.host_path,
                        %reason,
                        "loop: invalid host_source — skipped"
                    );
                    None
                }
            })
            .collect();

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

        // ── Stage 2: host-source governance sweep, then ingest sweep ─────
        // Host promotion runs FIRST so promoted files enter the exact same
        // inbox listing + stale-detection (prev_inbox) flow as raw copies.
        self.sweep_host_sources(&paths, loop_cfg, &host_contexts, config, &cycle_id)
            .await;
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
        let pipeline =
            DistillationPipeline::new().with_cas_commit(loop_cfg.cas_commit_or_default());
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
                // T032/T033 (FR-031/FR-032): pipeline outcome bookkeeping —
                // placeholder downgrades + CAS drift detection counters.
                report.placeholder_downgrades = outcome.placeholder_downgrades;
                report.cas_rolled_back = outcome.cas_rolled_back;
                report.cas_drifted = outcome.cas_drifted;
                gaps.extend(outcome.gaps);
            }
            Err(e) => {
                report.outcome = Some(CycleOutcome::Failed);
                report.last_error = Some(format!("distill stage: {e:#}"));
                report.gaps = gaps;
                persist_report_and_audit(&paths, &logs_dir, &report).await?;
                return Ok(worker_report(&report, started));
            }
        }

        // ── Stage 3a: Raw source graph routing (T031, FR-030) ─────────────
        // Code/paper sources under vault/raw/ bypass the md-note distill
        // pipeline and route straight into the entity graph. DailyNote and
        // Unknown tracks are skipped — md/txt already flow through distill,
        // so routing them here would double-process. Warn-and-continue per
        // file; a missing DB skips the whole stage.
        if loop_cfg.raw_graph_routing_or_default() {
            match db.as_ref() {
                Some(db) => {
                    let raw_dir = paths.vault().join("raw");
                    let router =
                        zen_vault::graph_router::GraphRouter::new(zen_vault::NotionService::new());
                    let mut routed = 0usize;
                    let mut joined = 0usize;
                    if let Ok(entries) = std::fs::read_dir(&raw_dir) {
                        for entry in entries.filter_map(|e| e.ok()) {
                            let path = entry.path();
                            if !path.is_file() {
                                continue;
                            }
                            let content = match std::fs::read(&path) {
                                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                                Err(e) => {
                                    warn!(error = %e, path = %path.display(), "loop: raw source read failed");
                                    continue;
                                }
                            };
                            let track = zen_vault::graph_router::classify_source(&path, &content);
                            if !matches!(
                                track,
                                zen_vault::graph_router::SourceTrack::Code
                                    | zen_vault::graph_router::SourceTrack::Paper
                            ) {
                                continue;
                            }
                            match router
                                .route_and_join(db, &path, &content, None, None, &report.cycle_id)
                                .await
                            {
                                Ok(outcome) => {
                                    routed += 1;
                                    joined += outcome.notions_extracted;
                                }
                                Err(e) => {
                                    warn!(error = %e, path = %path.display(), "loop: raw source graph routing failed (continues)");
                                }
                            }
                        }
                    }
                    if routed > 0 {
                        info!(routed, joined, "loop: raw sources routed into graph");
                    }

                    // FR-033 host-source routing (T044/T045): governed sources
                    // route WITH their HostSourceContext — code track reads the
                    // host dir in place (index-only + provenance pages), doc
                    // track routes the preserved raw/{host_hash}/ copies (or
                    // the host dir itself for index-only docs).
                    for ctx in &host_contexts {
                        let routing_root = if ctx.preserves_raw() {
                            raw_dir.join(&ctx.host_hash)
                        } else {
                            ctx.host_path.clone()
                        };
                        if !routing_root.is_dir() {
                            continue;
                        }
                        let Ok(entries) = std::fs::read_dir(&routing_root) else {
                            continue;
                        };
                        for entry in entries.filter_map(|e| e.ok()) {
                            let path = entry.path();
                            if !path.is_file() || !is_routable_extension(&path) {
                                continue;
                            }
                            let content = match std::fs::read(&path) {
                                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                                Err(e) => {
                                    warn!(error = %e, path = %path.display(), "loop: host source read failed");
                                    continue;
                                }
                            };
                            match router
                                .route_and_join(
                                    db,
                                    &path,
                                    &content,
                                    Some(ctx),
                                    Some(&paths.vault()),
                                    &report.cycle_id,
                                )
                                .await
                            {
                                Ok(outcome) => {
                                    routed += 1;
                                    joined += outcome.notions_extracted;
                                }
                                Err(e) => {
                                    warn!(error = %e, path = %path.display(), "loop: host source graph routing failed (continues)");
                                }
                            }
                        }
                    }

                    if routed > 0 {
                        info!(routed, joined, "loop: sources routed into graph");
                    }
                    report.raw_sources_routed = routed;
                    report.raw_notions_joined = joined;
                }
                None => {
                    warn!("loop: DB unavailable — skipping raw-source graph routing");
                }
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
            let slugs = zen_vault::distill::generate_from_gaps(&gaps);
            for slug in &slugs {
                if let Err(e) = zen_vault::distill::save(slug, &hypotheses_dir) {
                    warn!(error = %e, slug = %slug.slug, "loop: hypothesis save failed");
                }
            }
            report.hypotheses_generated = slugs.len();

            // FR-031a: declare this cycle's slugs as placeholder page slots
            // (placeholders.json) so concurrent writers downgrade creates to
            // updates. Persistence is best-effort; a corrupt registry starts
            // fresh rather than blocking the cycle.
            if !slugs.is_empty() {
                let registry_path = paths.logs().join("placeholders.json");
                let mut registry = match zen_vault::graph_verify::PlaceholderRegistry::load(
                    &registry_path,
                ) {
                    Ok(registry) => registry,
                    Err(e) => {
                        warn!(error = %e, "loop: placeholder registry unreadable — starting fresh");
                        zen_vault::graph_verify::PlaceholderRegistry::new()
                    }
                };
                for slug in &slugs {
                    registry.declare(&slug.slug, "zen-loop");
                }
                if let Err(e) = registry.save(&registry_path) {
                    warn!(error = %e, path = %registry_path.display(), "loop: placeholder registry save failed (non-fatal)");
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

        // ── Stage 5c: Hypothesis refinement & reverify (T029, FR-028) ─────
        // Refinement queue: every hypothesis slug on disk becomes external
        // fetch prompts + precise user questions, persisted to logs/ so the
        // operator (or a future host surface) can answer them between
        // cycles. Then stale hypotheses (older than the configured window)
        // transition via reverify. Both are best-effort.
        if loop_cfg.hypothesis_refinement_or_default() {
            let hypotheses_dir = paths.vault().join("wiki/wisdom/hypotheses");
            let slugs = match zen_vault::distill::load_all(&hypotheses_dir) {
                Ok(slugs) => slugs,
                Err(e) => {
                    warn!(error = %e, "loop: hypothesis load for refinement failed");
                    Vec::new()
                }
            };
            let (fetch_prompts, user_questions) =
                zen_vault::distill::build_refinement_queue(&slugs);
            report.refinement_fetch_prompts = fetch_prompts.len();
            report.refinement_user_questions = user_questions.len();

            let queue = serde_json::json!({
                "cycle_id": report.cycle_id,
                "generated_at": chrono::Utc::now().to_rfc3339(),
                "fetch_prompts": fetch_prompts,
                "user_questions": user_questions,
            });
            let queue_path = paths.logs().join("refinement-queue.json");
            match serde_json::to_string_pretty(&queue)
                .context("serializing refinement queue")
                .and_then(|json| {
                    std::fs::write(&queue_path, json)
                        .with_context(|| format!("write {}", queue_path.display()))
                }) {
                Ok(()) => {}
                Err(e) => {
                    warn!(error = %e, "loop: refinement queue persist failed (non-fatal)")
                }
            }

            let wiki_dir = paths.vault().join("wiki");
            match zen_vault::distill::reverify_with_rejections(
                &hypotheses_dir,
                &wiki_dir,
                chrono::Utc::now(),
                chrono::Duration::days(loop_cfg.reverify_older_than_days_or_default() as i64),
            ) {
                Ok((validated, reverified, rejected)) => {
                    report.hypotheses_validated = validated;
                    report.hypotheses_reverified = reverified;
                    report.hypotheses_rejected = rejected.len();
                    record_rejected_hypotheses(&paths, &rejected);
                }
                Err(e) => warn!(error = %e, "loop: hypothesis reverify failed (non-fatal)"),
            }

            // ── Stage 5d: Promotion staging (PD-06 fusion) ─────────────────
            // Validated hypotheses become Hybrid C promotion proposals in the
            // same cycle that validates them. Absorbs the former nightly
            // discover-loop worker; stage_from_validated is idempotent per
            // source slug. Gated with 5c: no refinement ⇒ no new validations
            // ⇒ nothing to stage.
            match zen_vault::distill::load_all(&hypotheses_dir) {
                Ok(slugs) => {
                    let promoter = PromotionWorker::new(paths.logs(), paths.skills());
                    match promoter.stage_from_validated(&paths.logs(), &slugs) {
                        Ok(staged) => {
                            report.promotions_applied = staged;
                            report.skills_precipitated = promoter
                                .pending(&paths.logs())
                                .map(|items| {
                                    items
                                        .iter()
                                        .filter(|item| item.target == PromotionTarget::SkillDraft)
                                        .count()
                                })
                                .unwrap_or(0);
                            if staged > 0 {
                                info!(staged, "loop: promotions staged for Hybrid C gates");
                            }
                        }
                        Err(e) => warn!(error = %e, "loop: promotion staging failed (non-fatal)"),
                    }
                }
                Err(e) => warn!(error = %e, "loop: hypothesis load for staging failed"),
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
    // Path Spec v2 (T18-C2): git target is the vault repo, not the
    // workspace root. vault/ is always global (personal knowledge follows
    // the user, not the project).
    let vault_path = paths.vault();

    let inside = std::process::Command::new("git")
        .arg("-C")
        .arg(&vault_path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    let inside_work_tree = matches!(
        inside,
        Ok(out) if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true"
    );
    if !inside_work_tree {
        debug!(vault = %vault_path.display(), "loop: vault not a git work tree — skipping git commit");
        return;
    }

    // Containment: only commit files inside the vault work tree.
    let managed: Vec<PathBuf> = [paths.vault(), paths.logs(), paths.memory()]
        .into_iter()
        .filter(|p| p.starts_with(&vault_path))
        .collect();
    if managed.is_empty() {
        debug!(
            vault = %vault_path.display(),
            "loop: managed paths outside vault work tree — skipping git commit"
        );
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

    let mut add_cmd = std::process::Command::new("git");
    add_cmd.arg("-C").arg(&vault_path).arg("add").arg("--");
    for managed_path in &managed {
        add_cmd.arg(managed_path);
    }
    let add = add_cmd.output();
    match add {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            warn!(
                vault = %vault_path.display(),
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
        .arg(&vault_path)
        .args(["commit", "-m", &message])
        .output();
    match commit {
        Ok(out) if out.status.success() => {
            info!(vault = %vault_path.display(), "loop: git history committed");
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if stderr.contains("nothing to commit") || stderr.contains("no changes added to commit")
            {
                debug!("loop: git commit skipped — nothing to commit");
            } else {
                warn!(
                    vault = %vault_path.display(),
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

    #[test]
    fn reverify_rejection_persists_negative_space_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = ZenPaths::for_testing(dir.path().to_path_buf());
        let hypo_dir = paths.vault().join("wiki/wisdom/hypotheses");
        let wiki_dir = paths.vault().join("wiki");
        std::fs::create_dir_all(&hypo_dir).unwrap();

        let h = zen_vault::distill::HypothesisSlug {
            slug: "loop-reject-entity".into(),
            hypothesis: "stale claim should be recorded".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: zen_vault::distill::HypothesisStatus::Exploring,
            exploration_prompt: None,
            evidence_refs: vec![],
            created_from: "g1".into(),
        };
        zen_vault::distill::save(&h, &hypo_dir).unwrap();
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86400);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(hypo_dir.join("loop-reject-entity.md"))
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();

        let (_, _, rejected) = zen_vault::distill::reverify_with_rejections(
            &hypo_dir,
            &wiki_dir,
            chrono::Utc::now(),
            chrono::Duration::days(7),
        )
        .unwrap();
        assert_eq!(rejected.len(), 1);

        record_rejected_hypotheses(&paths, &rejected);

        let rejected_dir = paths.wiki().join("wisdom").join("rejected");
        let entries: Vec<_> = std::fs::read_dir(&rejected_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "exactly one rejection record written");
        let content =
            std::fs::read_to_string(rejected_dir.join("stale-claim-should-be-recorded.md"))
                .unwrap();
        assert!(content.contains("stale claim should be recorded"));
        assert!(content.contains("falsifier"));
    }

    #[test]
    fn commit_cycle_to_git_commits_to_vault_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();

        let git_ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&vault)
            .args(["init"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !git_ok {
            eprintln!("skip: git binary unavailable");
            return;
        }

        std::fs::write(vault.join("log.md"), "# cycle log\n").unwrap();

        let paths = ZenPaths::for_testing(tmp.path().to_path_buf());
        let report = LoopCycleReport {
            cycle_id: "cycle-contract-test-001".into(),
            notes_processed: 3,
            pages_created: 1,
            merged_count: 2,
            quarantined_count: 0,
            ..LoopCycleReport::default()
        };

        commit_cycle_to_git(&paths, &report);

        let log_out = std::process::Command::new("git")
            .arg("-C")
            .arg(&vault)
            .args(["log", "--oneline"])
            .output()
            .unwrap();
        assert!(log_out.status.success(), "git log should succeed");
        let log = String::from_utf8_lossy(&log_out.stdout);
        assert!(
            log.contains("loop: cycle-contract-test-001"),
            "commit message must appear in git log: {log}"
        );
    }

    #[test]
    fn commit_cycle_to_git_skips_non_git_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();

        let paths = ZenPaths::for_testing(tmp.path().to_path_buf());
        let report = LoopCycleReport {
            cycle_id: "cycle-no-git-001".into(),
            ..LoopCycleReport::default()
        };

        commit_cycle_to_git(&paths, &report);
    }
}
