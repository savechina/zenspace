//! ZenLoopWorker — the knowledge-processing loop worker (005-agentic-loop).
//!
//! Executes the 6-stage cycle from `docs/specs/005-agentic-loop/contracts/worker.md`,
//! reusing the exact service code paths behind the manual `zen wiki` subcommands:
//! pre-cycle guards → ingest sweep → distill → verify → reindex → report+audit.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{info, warn};

use zen_core::config::load_config;
use zen_core::jsonl::append_jsonl_line;
use zen_core::paths::ZenPaths;
use zen_vault::distill::{
    CycleOutcome, GapKind, GapRecord, LoopCycleReport, SourceIngester,
};
use zen_vault::{DistillationPipeline, Reindexer};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

/// Where the last cycle report is persisted (`<logs>/loop-last-report.json`).
pub fn last_report_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join("loop-last-report.json")
}

/// Where gap records accumulate (`<logs>/loop-gaps.jsonl`).
pub fn gaps_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join("loop-gaps.jsonl")
}

/// The cron-driven knowledge-processing loop (worker contract, Phase 1).
pub struct ZenLoopWorker {
    scheduled: Option<&'static str>,
    /// Inbox file names seen in the previous cycle — stale = seen ≥2 cycles
    /// and still present (IngestNeverConsolidated).
    prev_inbox: Mutex<Option<HashSet<String>>>,
    cycles: AtomicU32,
    /// `zen wiki loop run --dry-run`: read paths only, no mutations.
    dry_run: bool,
    /// `[agentic.loop] enabled = false` — execute() becomes a no-op.
    cron_enabled: bool,
}

impl ZenLoopWorker {
    pub fn new() -> Self {
        Self {
            scheduled: None,
            prev_inbox: Mutex::new(None),
            cycles: AtomicU32::new(0),
            dry_run: false,
            cron_enabled: true,
        }
    }

    /// Override the cron schedule (from `[agentic.loop] interval`).
    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }

    /// Dry-run mode: skip distill/reindex mutations, report only.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Cron-disabled state (registered but never fires; manual `run` unaffected).
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
}

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
        let distill = pipeline
            .run_scoped(
                &paths.inbox(),
                &paths.wiki(),
                &paths.archive(),
                &logs_dir,
                db.as_ref(),
            )
            .await;

        match distill {
            Ok(dr) => {
                report.notes_processed = dr.notes_processed;
                report.entities_persisted = dr.entities_persisted;
                report.pages_created = dr.wiki_pages_created;
                report.archived_count = dr.migrated_files.len();
            }
            Err(e) => {
                report.outcome = Some(CycleOutcome::Failed);
                report.last_error = Some(format!("distill stage: {e:#}"));
                gaps.extend(report.gaps.drain(..));
                report.gaps = gaps;
                persist_report_and_audit(&paths, &logs_dir, &report).await?;
                return Ok(worker_report(&report, started));
            }
        }

        // ── Stage 4: verify (stub — T016 wires GraphIntegrityVerifier) ────
        // Structural gap detection lands with US2; page lint stays in
        // `zen wiki lint` until then.

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

        // ── Stage 6: report + audit ───────────────────────────────────────
        let _ = self.cycles.fetch_add(1, Ordering::Relaxed);
        report.outcome = Some(CycleOutcome::Completed);
        report.gaps = gaps;
        persist_report_and_audit(&paths, &logs_dir, &report).await?;

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
}
