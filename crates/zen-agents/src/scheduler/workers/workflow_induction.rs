//! Daily workflow-induction worker (W13.7) — runs the Phase 1 induction pass
//! over the live plan corpus and stages reusable DAG draft templates.
//!
//! Design of record: `docs/designs/workflow-induction-2026-11-18.md` §8
//! (execution surface) + §10 AC#2. Advisory-only: the pass reads
//! `workflow_plans`/`workflow_tasks` and writes the drafts queue; no code path
//! reads an induced template to alter plan execution.
//!
//! Scope logic (Constitution XV):
//! - **Functionality**: once daily at 03:00, open `state.db`, run
//!   [`WorkflowInduction::run`] against the ZEN logs dir, and emit ONE
//!   `zen_induction` info line with the run counters.
//! - **User impact**: `zen discover report` gains an advisory
//!   `workflow_induction` section (W13.8); drafts are never auto-applied.
//! - **Default**: registered unconditionally in both scheduler profiles — the
//!   per-run flock (`workflow-induction.lock`) serializes induction against
//!   confirm/reject, and the cross-process scheduler lease keeps exactly one
//!   host ticking.
//! - **Interaction**: FAIL-SOFT by design — the output is advisory, so a
//!   missing/unreadable `state.db` or an induction error is a `warn!` plus a
//!   `success: false` report, never a panic or a scheduler error. No
//!   `audit.jsonl` line (design §7/§8: report-only).

use std::time::Instant;

use anyhow::Result;
use tracing::{info, warn};

use zen_core::paths::ZenPaths;
use zen_repo::SqliteClient;

use crate::workflow_induction::WorkflowInduction;

use super::super::{WorkerContext, WorkerReport, ZenWorker};

/// Cron: daily 03:00 (6-field, seconds first). The scheduler evaluates
/// wall-clock fields in `[cron].timezone` via its `with_timezone` wiring.
pub const WORKFLOW_INDUCTION_SCHEDULE: &str = "0 0 3 * * *";

/// Daily advisory induction of reusable workflow shapes.
pub struct WorkflowInductionWorker {
    scheduled: Option<&'static str>,
}

impl WorkflowInductionWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }

    /// Testable core: run one induction pass under an explicit [`ZenPaths`].
    ///
    /// Fault posture mirrors the sibling workers' fail-soft discipline: a DB
    /// open failure or an induction error is logged (`warn!`) and reported as
    /// `success: false`; the scheduler stays alive. Induction is advisory, so
    /// a failed pass simply leaves the previous drafts queue untouched (the
    /// Phase 1 run writes atomically only after a successful pass).
    pub async fn execute_with_paths(
        &self,
        paths: &ZenPaths,
        ctx: &WorkerContext,
    ) -> Result<WorkerReport> {
        let start = Instant::now();
        if let Ok(cfg) = zen_core::config::load_config()
            && !cfg.agentic.workflow_induction.enabled_or_default()
        {
            info!(
                "workflow-induction: disabled via [agentic.workflow_induction] enabled=false — nothing to do"
            );
            return Ok(self.finish(start, 0, true));
        }
        let state_db = paths.data().join("state.db");
        let client = match SqliteClient::open(&state_db).await {
            Ok(client) => client,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %state_db.display(),
                    "workflow-induction: failed to open state.db — skipping induction"
                );
                return Ok(self.finish(start, 0, false));
            }
        };

        let induction = WorkflowInduction::new(paths.logs());
        match induction.run(&client, ctx.now.timestamp()).await {
            Ok(run) => {
                let insufficient = run.insufficient.map(|reason| reason.missing());
                info!(
                    target: "zen_induction",
                    drafts_written = run.drafts_written,
                    skipped_corrupt = run.skipped_corrupt,
                    skipped_titles_only = run.skipped_titles_only,
                    insufficient = ?insufficient,
                    "workflow induction run complete"
                );
                Ok(self.finish(start, run.drafts_written, true))
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "workflow-induction: induction run failed — drafts queue left unchanged"
                );
                Ok(self.finish(start, 0, false))
            }
        }
    }

    fn finish(&self, start: Instant, drafts_written: usize, success: bool) -> WorkerReport {
        WorkerReport {
            worker_id: self.id().to_string(),
            success,
            fact_count: drafts_written,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        }
    }
}

impl Default for WorkflowInductionWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ZenWorker for WorkflowInductionWorker {
    fn id(&self) -> &'static str {
        "workflow-induction"
    }

    fn description(&self) -> &'static str {
        "Daily advisory workflow induction: clusters repeated plan DAG shapes into a drafts queue (never auto-applied)"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or(WORKFLOW_INDUCTION_SCHEDULE)
    }

    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport> {
        let paths = ZenPaths::detect()?;
        self.execute_with_paths(&paths, ctx).await
    }
}
