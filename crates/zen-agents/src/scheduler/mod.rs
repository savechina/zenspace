//! ZenScheduler — cron-driven task scheduler for background workers.
//!
//! Uses the [`cron`](https://crates.io/crates/cron) crate for schedule
//! parsing and matching. Runs a tick loop that checks registered workers
//! against their cron expressions and fires matching ones.
//!
//! # Architecture
//!
//! ```text
//! ZenScheduler::run() → tick loop (every ~30s)
//!   ├── foreach worker: schedule.includes(now)?
//!   │   └── worker.execute(&WorkerContext{now})
//!   └── sleep(DEFAULT_TICK_INTERVAL)
//!
//! trigger(name) — immediately fire a named worker
//! list() — return registered worker summaries
//! ```

mod workers;

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use cron::Schedule;
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, warn};

pub use workers::*;
use zen_core::config::{CronConfig, default_daily_log_schedule, default_night_dream_schedule};

// ─── Core types ────────────────────────────────────────────────────────

/// Context passed to every worker execution.
#[derive(Debug, Clone)]
pub struct WorkerContext {
    /// Timestamp when the tick was triggered.
    pub now: DateTime<Utc>,
}

impl WorkerContext {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self { now }
    }
}

/// Report returned by a worker after execution.
#[derive(Debug, Clone)]
pub struct WorkerReport {
    pub worker_id: String,
    pub success: bool,
    pub fact_count: usize,
    pub duration_ms: u64,
}

// ─── ZenWorker trait ──────────────────────────────────────────────────

/// A schedulable background task.
///
/// Mirrors the naming convention of [`ZenSkill`] and [`ZenTool`]
/// in the agent system — every "thing that does work" gets a `Zen` prefix.
#[async_trait::async_trait]
pub trait ZenWorker: Send + Sync {
    /// Unique identifier for this worker (e.g. `"journal-worker"`, `"dream"`).
    fn id(&self) -> &'static str;

    /// Human-readable description of what this worker does.
    fn description(&self) -> &'static str;

    /// Cron expression defining the schedule (e.g. `"0 */5 * * * *"`).
    fn schedule(&self) -> &'static str;

    /// Execute the worker's task.
    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport>;
}

// ─── Error type ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("worker '{0}' already registered")]
    DuplicateWorker(String),

    #[error("worker '{0}' not found")]
    WorkerNotFound(String),

    #[error("invalid cron expression for worker '{worker}': {source}")]
    InvalidCron {
        worker: String,
        #[source]
        source: cron::error::Error,
    },
}

// ─── ZenScheduler ──────────────────────────────────────────────────────

/// Default tick interval for the scheduler loop (30 seconds).
pub const DEFAULT_TICK_INTERVAL_SECONDS: u64 = 30;

type RegisteredWorker = (String, Schedule, Arc<dyn ZenWorker>);

/// Cron-driven scheduler that manages and executes background workers.
///
/// # Example
///
/// ```no_run
/// use zen_agents::scheduler::{ZenScheduler, JournalWorker, DreamWorker};
///
/// # async fn example() -> anyhow::Result<()> {
/// let mut scheduler = ZenScheduler::new();
/// scheduler.register(JournalWorker::new())?;
/// scheduler.register(DreamWorker::new())?;
///
/// // Run the event loop in a background task
/// tokio::spawn(async move {
///     scheduler.run().await;
/// });
/// # Ok(())
/// # }
/// ```
pub struct ZenScheduler {
    workers: HashMap<String, RegisteredWorker>,
    tick_interval: Duration,
}

impl ZenScheduler {
    pub fn new() -> Self {
        Self {
            workers: HashMap::new(),
            tick_interval: Duration::from_secs(DEFAULT_TICK_INTERVAL_SECONDS),
        }
    }

    /// Set a custom tick interval (default 30s).
    pub fn with_tick_interval(mut self, seconds: u64) -> Self {
        self.tick_interval = Duration::from_secs(seconds);
        self
    }

    /// Register a worker. Returns an error if a worker with the same ID
    /// is already registered or the cron expression is invalid.
    pub fn register(&mut self, worker: impl ZenWorker + 'static) -> Result<(), SchedulerError> {
        let id = worker.id().to_string();
        let expr = worker.schedule();

        if self.workers.contains_key(&id) {
            return Err(SchedulerError::DuplicateWorker(id));
        }

        let schedule = Schedule::from_str(expr).map_err(|source| SchedulerError::InvalidCron {
            worker: id.clone(),
            source,
        })?;

        info!(
            worker = %id,
            schedule = %expr,
            "scheduler: worker registered"
        );

        self.workers
            .insert(id, (expr.to_string(), schedule, Arc::new(worker)));
        Ok(())
    }

    /// Run the event loop. Checks all workers against their schedules
    /// at the configured tick interval. Runs indefinitely.
    pub async fn run(self) {
        info!(
            tick_interval_ms = self.tick_interval.as_millis() as u64,
            workers = self.workers.len(),
            "scheduler: starting event loop"
        );

        loop {
            let now = Utc::now();
            self.tick(now).await;
            sleep(self.tick_interval).await;
        }
    }

    /// Run a single tick: check all workers and fire matching ones.
    async fn tick(&self, now: DateTime<Utc>) {
        let ctx = WorkerContext::new(now);
        let interval = self.tick_interval;

        // Collect workers to fire first to avoid borrow issues with spawn.
        let mut to_fire: Vec<(String, Arc<dyn ZenWorker>, WorkerContext)> = Vec::new();

        for (id, (_expr, schedule, worker)) in &self.workers {
            let should_fire = schedule.upcoming(Utc).next().is_some_and(|next| {
                let diff = (next - now).num_seconds().unsigned_abs();
                // Fire if the next scheduled time is within the tick window
                diff < interval.as_secs() + 1
            });

            if should_fire {
                debug!(worker = %id, "scheduler: firing worker");
                to_fire.push((id.clone(), Arc::clone(worker), ctx.clone()));
            }
        }

        for (id, worker, ctx) in to_fire {
            tokio::spawn(async move {
                match worker.execute(&ctx).await {
                    Ok(report) => {
                        info!(
                            worker = %report.worker_id,
                            success = report.success,
                            facts = report.fact_count,
                            duration_ms = report.duration_ms,
                            "scheduler: worker completed"
                        );
                    }
                    Err(e) => {
                        error!(worker = %id, error = %e, "scheduler: worker failed");
                    }
                }
            });
        }
    }

    /// Immediately trigger a named worker outside the scheduled loop.
    pub async fn trigger(&self, name: &str) -> Result<WorkerReport, SchedulerError> {
        let (_, _, worker) = self
            .workers
            .get(name)
            .ok_or_else(|| SchedulerError::WorkerNotFound(name.to_string()))?;

        let ctx = WorkerContext::new(Utc::now());
        info!(worker = %name, "scheduler: manual trigger");
        worker.execute(&ctx).await.map_err(|e| {
            error!(worker = %name, error = %e, "scheduler: triggered worker failed");
            SchedulerError::WorkerNotFound(name.to_string())
        })
    }

    /// List all registered workers with their schedules and descriptions.
    pub fn list(&self) -> Vec<WorkerSummary> {
        self.workers
            .iter()
            .map(|(id, (expr, _schedule, worker))| WorkerSummary {
                id: id.clone(),
                schedule: expr.clone(),
                description: worker.description().to_string(),
            })
            .collect()
    }
}

impl Default for ZenScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary information for a registered worker.
#[derive(Debug, Clone)]
pub struct WorkerSummary {
    pub id: String,
    pub schedule: String,
    pub description: String,
}

// ─── Convenience constructor ──────────────────────────────────────────

/// Create a fully-configured [`ZenScheduler`] with all built-in workers
/// registered. This is the primary way to bootstrap scheduling in the
/// TUI.
///
/// Registers:
/// - `journal-worker` (JournalWorker): runs every 5 minutes, checks daily log, updates MEMORY.md
/// - `subconscious`: runs every 5 minutes, evaluates workspace state
/// - `dream`: runs 2-4AM, executes the nightly consolidation cycle
/// - `session-journaler`: runs every 5 minutes, extracts journal entries from session conversations
/// - `entity-extractor`: runs every 10 minutes, extracts entities from journal entries into graph.db
/// - `wiki-compiler`: runs every 30 minutes, compiles wiki pages from graph.db entities
/// - `commitment-tracker` (CommitmentTracker): runs daily 8AM, tracks commitments from journal entries
/// - `reflection-worker` (ReflectionWorker): runs daily 6AM, aggregates reflections into wiki/wisdom/
/// - `wisdom-synth` (WisdomSynthesizer): runs weekly Sun 2AM (cron: `0 0 2 * * 7`), synthesizes reflections + beliefs into wisdom candidates
/// - `express` (ExpressWorker): runs weekly Sat 3PM (cron: `0 0 15 * * 6`), LLM expression of insights into publishable review and blog drafts
/// - `memvid-indexer` (MemvidIndexerWorker): runs nightly 1AM (cron: `0 0 1 * * *`), ingests journal, wiki, wisdom into memvid store
/// - `evidence-gatherer` (EvidenceGatherer): runs weekly Mon 6AM (cron: `0 0 6 * * 1`), scans beliefs with low evidence count, generates research suggestions
pub fn create_default_scheduler() -> ZenScheduler {
    let mut scheduler = ZenScheduler::new();

    if let Err(e) = scheduler.register(JournalWorker::new()) {
        warn!("scheduler: failed to register journal-worker: {e}");
    }
    if let Err(e) = scheduler.register(SubconsciousWorker::new()) {
        warn!("scheduler: failed to register subconscious worker: {e}");
    }
    if let Err(e) = scheduler.register(DreamWorker::new()) {
        warn!("scheduler: failed to register dream worker: {e}");
    }
    if let Err(e) = scheduler.register(SessionJournaler::new()) {
        warn!("scheduler: failed to register session-journaler worker: {e}");
    }
    if let Err(e) = scheduler.register(EntityExtractorWorker::new()) {
        warn!("scheduler: failed to register entity-extractor worker: {e}");
    }
    if let Err(e) = scheduler.register(WikiCompilerWorker::new()) {
        warn!("scheduler: failed to register wiki-compiler worker: {e}");
    }
    if let Err(e) = scheduler.register(CommitmentTracker::new()) {
        warn!("scheduler: failed to register commitment-tracker worker: {e}");
    }
    if let Err(e) = scheduler.register(ReflectionWorker::new()) {
        warn!("scheduler: failed to register reflection-worker worker: {e}");
    }

    if let Err(e) = scheduler.register(WisdomSynthesizer::new()) {
        warn!("scheduler: failed to register wisdom-synth worker: {e}");
    }

    if let Err(e) = scheduler.register(DecisionTracker::new()) {
        warn!("scheduler: failed to register decision-tracker worker: {e}");
    }

    if let Err(e) = scheduler.register(ExpressWorker::new()) {
        warn!("scheduler: failed to register express worker: {e}");
    }

    if let Err(e) = scheduler.register(MemvidIndexerWorker::new()) {
        warn!("scheduler: failed to register memvid-indexer worker: {e}");
    }

    if let Err(e) = scheduler.register(EvidenceGatherer::new()) {
        warn!("scheduler: failed to register evidence-gatherer worker: {e}");
    }

    scheduler
}

/// Create a [`ZenScheduler`] wired with `CronConfig` values for worker schedules.
///
/// Uses `default_daily_log_schedule()` / `default_night_dream_schedule()` as
/// fallbacks when config fields are `None`.
pub fn create_configured_scheduler(config: &CronConfig) -> ZenScheduler {
    let mut scheduler = ZenScheduler::new();

    let dl_schedule = config
        .daily_log_schedule()
        .unwrap_or_else(|| default_daily_log_schedule().to_string());
    if let Err(e) = scheduler.register(JournalWorker::new().with_schedule(&dl_schedule)) {
        warn!("scheduler: failed to register journal-worker: {e}");
    }

    let sc_schedule = config
        .subconscious_interval_minutes
        .map(|mins| format!("0 */{mins} * * * *"))
        .unwrap_or_else(|| default_daily_log_schedule().to_string());
    if let Err(e) = scheduler.register(SubconsciousWorker::new().with_schedule(&sc_schedule)) {
        warn!("scheduler: failed to register subconscious worker: {e}");
    }

    let dream_schedule = config
        .night_dream_schedule()
        .unwrap_or_else(|| default_night_dream_schedule().to_string());
    if let Err(e) = scheduler.register(DreamWorker::new().with_schedule(&dream_schedule)) {
        warn!("scheduler: failed to register dream worker: {e}");
    }

    if let Err(e) = scheduler.register(SessionJournaler::new()) {
        warn!("scheduler: failed to register session-journaler worker: {e}");
    }

    if let Err(e) = scheduler.register(EntityExtractorWorker::new()) {
        warn!("scheduler: failed to register entity-extractor worker: {e}");
    }

    if let Err(e) = scheduler.register(WikiCompilerWorker::new()) {
        warn!("scheduler: failed to register wiki-compiler worker: {e}");
    }

    if let Err(e) = scheduler.register(CommitmentTracker::new()) {
        warn!("scheduler: failed to register commitment-tracker worker: {e}");
    }

    if let Err(e) = scheduler.register(ReflectionWorker::new()) {
        warn!("scheduler: failed to register reflection-worker worker: {e}");
    }

    if let Err(e) = scheduler.register(WisdomSynthesizer::new()) {
        warn!("scheduler: failed to register wisdom-synth worker: {e}");
    }

    if let Err(e) = scheduler.register(DecisionTracker::new()) {
        warn!("scheduler: failed to register decision-tracker worker: {e}");
    }

    if let Err(e) = scheduler.register(ExpressWorker::new()) {
        warn!("scheduler: failed to register express worker: {e}");
    }

    if let Err(e) = scheduler.register(MemvidIndexerWorker::new()) {
        warn!("scheduler: failed to register memvid-indexer worker: {e}");
    }

    if let Err(e) = scheduler.register(EvidenceGatherer::new()) {
        warn!("scheduler: failed to register evidence-gatherer worker: {e}");
    }

    scheduler
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestWorker;

    #[async_trait::async_trait]
    impl ZenWorker for TestWorker {
        fn id(&self) -> &'static str {
            "test"
        }
        fn description(&self) -> &'static str {
            "test worker"
        }
        fn schedule(&self) -> &'static str {
            "0 0 2-4 * * *"
        }
        async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
            Ok(WorkerReport {
                worker_id: "test".to_string(),
                success: true,
                fact_count: 0,
                duration_ms: 0,
            })
        }
    }

    #[tokio::test]
    async fn test_register_and_list() {
        let scheduler = {
            let mut s = ZenScheduler::new();
            s.register(TestWorker).unwrap();
            s
        };

        let summary = scheduler.list();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].id, "test");
    }

    #[tokio::test]
    async fn test_duplicate_registration() {
        let mut scheduler = ZenScheduler::new();
        scheduler.register(TestWorker).unwrap();
        let result = scheduler.register(TestWorker);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_trigger_nonexistent() {
        let scheduler = ZenScheduler::new();
        let result = scheduler.trigger("nonexistent").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_cron_expression() {
        let mut scheduler = ZenScheduler::new();
        struct BadWorker;

        #[async_trait::async_trait]
        impl ZenWorker for BadWorker {
            fn id(&self) -> &'static str {
                "bad"
            }
            fn description(&self) -> &'static str {
                ""
            }
            fn schedule(&self) -> &'static str {
                "not-a-cron"
            }
            async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
                unimplemented!()
            }
        }

        let result = scheduler.register(BadWorker);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_default_scheduler() {
        let scheduler = create_default_scheduler();
        let items = scheduler.list();
        assert_eq!(items.len(), 13);
        assert!(items.iter().any(|w| w.id == "journal-worker"));
        assert!(items.iter().any(|w| w.id == "dream"));
        assert!(items.iter().any(|w| w.id == "subconscious"));
        assert!(items.iter().any(|w| w.id == "session-journaler"));
        assert!(items.iter().any(|w| w.id == "entity-extractor"));
        assert!(items.iter().any(|w| w.id == "wiki-compiler"));
        assert!(items.iter().any(|w| w.id == "commitment-tracker"));
        assert!(items.iter().any(|w| w.id == "reflection-worker"));
        assert!(items.iter().any(|w| w.id == "wisdom-synth"));
        assert!(items.iter().any(|w| w.id == "decision-tracker"));
        assert!(items.iter().any(|w| w.id == "express"));
        assert!(items.iter().any(|w| w.id == "memvid-indexer"));
        assert!(items.iter().any(|w| w.id == "evidence-gatherer"));
    }
}
