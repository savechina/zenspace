//! Daily bounded-retention sweep — applies the zen-vault retention policy
//! engine to every filesystem home and audits the outcome (review D2).
//!
//! Scope logic (Constitution XV):
//! - Functionality: fires [`apply_policies`] once daily at 03:30 (after the
//!   02:00 dream consolidation) and appends ONE `loop.retention.applied`
//!   audit line with per-home counts to `logs/audit.jsonl`.
//! - User impact: append-only homes (audit.jsonl, outbox, journals, …) stay
//!   bounded; `zen logs` / `zen discover report` keep reading the fresh
//!   current audit.jsonl (rotated siblings are cold but present).
//! - Default: `[agentic.retention] enabled=true, dry_run=false`.
//! - Interaction: `enabled=false` → the worker returns immediately (no
//!   sweep, no audit line); `dry_run=true` → the full report is computed and
//!   audited with `dry_run:true`, nothing mutated. Registered in BOTH
//!   scheduler profiles (Full + InApp) — the cross-process scheduler lease
//!   guarantees exactly one host runs the sweep.

use anyhow::Result;
use tracing::{info, warn};

use zen_core::config::{RetentionConfig, load_config};
use zen_core::jsonl::append_jsonl_line;
use zen_core::paths::ZenPaths;
use zen_vault::distill::retention::apply_policies;

use super::super::{WorkerContext, WorkerReport, ZenWorker};

/// Cron: daily 03:30 (6-field, seconds first) — after the 02:00 dream worker.
pub const RETENTION_SCHEDULE: &str = "0 30 3 * * *";

pub struct RetentionWorker {
    scheduled: Option<&'static str>,
}

impl RetentionWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }

    /// Testable core: runs one sweep against an explicit tree + config and
    /// appends the `loop.retention.applied` audit line. Sync by design — the
    /// sweep is plain filesystem work with no awaits.
    pub fn execute_with_paths(
        &self,
        paths: &ZenPaths,
        cfg: &RetentionConfig,
        ctx: &WorkerContext,
    ) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        if !cfg.enabled_or_default() {
            info!("retention: disabled via [agentic.retention] enabled=false — nothing to do");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                llm_cost_usd: 0.0,
            });
        }
        let dry_run = cfg.dry_run_or_default();
        let report = apply_policies(paths, cfg, ctx.now);
        let removed = report.removed();
        let rotated = report.rotated();
        let bytes_freed = report.bytes_freed();

        let audit = serde_json::json!({
            "kind": "loop.retention.applied",
            "ts": ctx.now.to_rfc3339(),
            "removed": removed,
            "rotated": rotated,
            "bytes_freed": bytes_freed,
            "dry_run": dry_run,
            "per_home": report.homes,
        });
        let audit_path = paths.logs().join("audit.jsonl");
        if let Err(e) = append_jsonl_line(&audit_path, &audit) {
            warn!(
                error = %e, path = %audit_path.display(),
                "retention: audit append failed (sweep result kept in worker report)"
            );
        }
        info!(
            removed,
            rotated, bytes_freed, dry_run, "retention sweep applied"
        );

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: (removed + rotated) as usize,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

impl Default for RetentionWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ZenWorker for RetentionWorker {
    fn id(&self) -> &'static str {
        "retention"
    }

    fn description(&self) -> &'static str {
        "Daily bounded-retention sweep: rotates JSONL logs, deletes aged files, caps keep-newest homes (quarantine report-only)"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or(RETENTION_SCHEDULE)
    }

    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport> {
        let cfg = match load_config() {
            Ok(config) => config.agentic.retention.clone(),
            Err(e) => {
                warn!(
                    error = %e,
                    "retention: config load failed — using defaults (enabled, no dry-run)"
                );
                RetentionConfig::default()
            }
        };
        let paths = ZenPaths::detect()?;
        self.execute_with_paths(&paths, &cfg, ctx)
    }
}
