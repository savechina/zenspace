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
use std::sync::{Arc, RwLock};

use anyhow::Result;
use chrono::{DateTime, Utc};
use cron::Schedule;
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, warn};

pub mod lease;
pub use lease::{LeaseError, SchedulerLease};
pub use workers::*;
use zen_core::config::{
    CronConfig, default_daily_log_schedule, default_night_dream_schedule,
    default_wisdom_synthesis_schedule,
};

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
    /// LLM cost incurred during this execution (USD, 0.0 if no LLM calls).
    pub llm_cost_usd: f64,
}

// ─── ZenWorker trait ──────────────────────────────────────────────────

/// A schedulable background task.
///
/// Mirrors the naming convention of [`ZenSkill`] and [`ZenTool`]
/// in the agent system — every "thing that does work" gets a `Zen` prefix.
#[async_trait::async_trait]
pub trait ZenWorker: Send + Sync {
    /// Unique identifier for this worker (e.g. `"memory-curator"`, `"dream"`).
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

type RegisteredWorker = (
    String,
    Schedule,
    Arc<dyn ZenWorker>,
    bool,
    // F5: per-worker in-flight flag — a slow worker never overlaps itself.
    Arc<std::sync::atomic::AtomicBool>,
);

/// Clears a worker's in-flight flag on drop (F5) — panic-safe.
struct InFlightGuard<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Cron-driven scheduler that manages and executes background workers.
///
/// # Example
///
/// ```no_run
/// use zen_agents::scheduler::{ZenScheduler, MemoryCurator, DreamWorker};
///
/// # async fn example() -> anyhow::Result<()> {
/// let mut scheduler = ZenScheduler::new();
/// scheduler.register(MemoryCurator::new())?;
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
    /// Cumulative LLM cost per worker (USD) for the current month, optionally
    /// persisted to a sidecar file (see [`ZenScheduler::with_cost_ledger`]).
    worker_costs: Arc<RwLock<WorkerCostLedger>>,
    /// Monthly cost cap per worker (USD). Workers exceeding this are skipped.
    cost_cap_usd: f64,
    /// Timezone cron wall-clock fields are evaluated against (E11).
    /// Utc unless the caller wires `CronConfig::timezone_or_default()`.
    tz: chrono_tz::Tz,
}

/// `"YYYY-MM"` (UTC) month key the cost cap window is scoped to.
fn month_key(now: DateTime<Utc>) -> String {
    now.format("%Y-%m").to_string()
}

/// Per-worker monthly LLM cost totals backing the `[cron] llm_cost_cap_usd`
/// cap check.
///
/// Scope logic (Constitution XV):
/// - Functionality: holds the cumulative USD cost per worker for one calendar
///   month and (when `path` is set) mirrors it to a small JSON sidecar so the
///   cap survives daemon restarts within the month.
/// - User impact: a worker whose monthly total reaches the cap is skipped on
///   every subsequent tick (with the existing warn line) until the month rolls
///   over; without persistence a restart used to reset every total to 0, so
///   the cap could never trip on a frequently-restarted daemon.
/// - Default: in-memory only (`path: None`), month = construction month.
/// - Interaction: rollover is lazy — a month mismatch at check or accumulate
///   time resets the totals; a missing/corrupt sidecar fails open to empty
///   (loud warn on corrupt), matching the plugin `state.json` precedent.
#[derive(Debug)]
struct WorkerCostLedger {
    month: String,
    costs: HashMap<String, f64>,
    path: Option<std::path::PathBuf>,
}

impl WorkerCostLedger {
    fn in_memory(month: String) -> Self {
        Self {
            month,
            costs: HashMap::new(),
            path: None,
        }
    }
}

/// On-disk shape of the cost ledger sidecar (`path` itself is not persisted).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CostLedgerFile {
    month: String,
    costs: HashMap<String, f64>,
}

/// Load persisted monthly totals for `month`. Missing file, unreadable file,
/// corrupt JSON, or a month mismatch all yield an empty map (fail-open; the
/// cap simply starts accruing from zero). Corrupt content warns loudly.
fn load_cost_ledger(path: &std::path::Path, month: &str) -> HashMap<String, f64> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return HashMap::new(),
    };
    match serde_json::from_str::<CostLedgerFile>(&raw) {
        Ok(file) if file.month == month => file.costs,
        Ok(_) => HashMap::new(),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "scheduler: cost ledger corrupt — starting from zero (cap accrues fresh)");
            HashMap::new()
        }
    }
}

/// Persist monthly totals atomically (tmp → fsync → rename). A write failure
/// is returned to the caller, which warns — in-memory totals stay correct for
/// the process lifetime either way.
fn save_cost_ledger(
    path: &std::path::Path,
    month: &str,
    costs: &HashMap<String, f64>,
) -> std::io::Result<()> {
    let file = CostLedgerFile {
        month: month.to_string(),
        costs: costs.clone(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    zen_core::atomic_file::write_atomic(path, json.as_bytes())
}

impl ZenScheduler {
    pub fn new() -> Self {
        Self {
            workers: HashMap::new(),
            tick_interval: Duration::from_secs(DEFAULT_TICK_INTERVAL_SECONDS),
            worker_costs: Arc::new(RwLock::new(WorkerCostLedger::in_memory(month_key(
                Utc::now(),
            )))),
            cost_cap_usd: 10.0,
            tz: chrono_tz::UTC,
        }
    }

    /// Persist per-worker monthly cost totals to `path` (and reseed from it),
    /// so the `[cron] llm_cost_cap_usd` cap survives restarts within a month.
    ///
    /// A file whose `month` differs from the current UTC month (or that is
    /// missing/corrupt) seeds nothing — the monthly window resets by design.
    pub fn with_cost_ledger(self, path: std::path::PathBuf) -> Self {
        let month = month_key(Utc::now());
        let costs = load_cost_ledger(&path, &month);
        *self.worker_costs.write().unwrap() = WorkerCostLedger {
            month,
            costs,
            path: Some(path),
        };
        self
    }

    /// The cost-cap check the tick loop runs before firing a worker: returns
    /// the worker's cumulative cost for `now`'s month when it is at or above
    /// the cap, `None` otherwise (including a month rollover — a new month
    /// starts every worker at zero).
    fn cost_cap_exceeded(&self, worker_id: &str, now: DateTime<Utc>) -> Option<f64> {
        let ledger = self.worker_costs.read().unwrap();
        if ledger.month != month_key(now) {
            return None;
        }
        ledger
            .costs
            .get(worker_id)
            .copied()
            .filter(|&cost| cost >= self.cost_cap_usd)
    }

    /// Evaluate cron schedules in `tz` (E11): worker wall-clock fields
    /// ("0 0 9 * * *") then mean that zone's local time. Pairs with
    /// `CronConfig::timezone_or_default()`; defaults to Utc.
    pub fn with_timezone(mut self, tz: chrono_tz::Tz) -> Self {
        self.tz = tz;
        self
    }

    /// The timezone cron schedules are evaluated in.
    pub fn timezone(&self) -> chrono_tz::Tz {
        self.tz
    }

    /// Set the per-worker monthly LLM cost cap (USD).
    pub fn with_cost_cap(mut self, cap_usd: f64) -> Self {
        self.cost_cap_usd = cap_usd;
        self
    }

    /// Get the current per-worker monthly cost cap (USD).
    pub fn cost_cap(&self) -> f64 {
        self.cost_cap_usd
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

        self.workers.insert(
            id,
            (
                expr.to_string(),
                schedule,
                Arc::new(worker),
                true,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ),
        );
        Ok(())
    }

    /// Run the event loop. Checks all workers against their schedules
    /// at the configured tick interval. Runs indefinitely.
    pub async fn run(self) {
        // Thin wrapper: a watch whose sender stays alive and never fires,
        // so `run_with_shutdown`'s `changed()` never resolves with a
        // sender-dropped error and the loop runs forever.
        let (tx, rx) = tokio::sync::watch::channel(false);
        let _never_fires = tx;
        self.run_with_shutdown(rx).await
    }

    /// Run the event loop until the shutdown watch flips to `true`.
    ///
    /// The loop `select!`s between one tick+sleep and the shutdown
    /// signal, so a signal lands within one tick interval. Handles both
    /// the `changed()` transition and the already-true case (the signal
    /// may have fired before the loop started).
    pub async fn run_with_shutdown(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        info!(
            tick_interval_ms = self.tick_interval.as_millis() as u64,
            workers = self.workers.len(),
            "scheduler: starting event loop"
        );
        if *shutdown.borrow() {
            info!("scheduler: shutdown already signalled, not starting event loop");
            return;
        }
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow_and_update() {
                        info!("scheduler: shutdown signal received, stopping event loop");
                        break;
                    }
                }
                _ = self.tick_and_sleep() => {}
            }
        }
    }

    /// One tick followed by the tick-interval sleep (the select arm of
    /// [`Self::run_with_shutdown`]).
    async fn tick_and_sleep(&self) {
        let now = Utc::now();
        self.tick(now).await;
        sleep(self.tick_interval).await;
    }

    /// Run a single tick: check all workers and fire matching ones.
    async fn tick(&self, now: DateTime<Utc>) {
        let ctx = WorkerContext::new(now);
        let interval = self.tick_interval;

        // Collect workers to fire first to avoid borrow issues with spawn.
        #[allow(clippy::type_complexity)]
        let mut to_fire: Vec<(
            String,
            Arc<dyn ZenWorker>,
            WorkerContext,
            Arc<std::sync::atomic::AtomicBool>,
        )> = Vec::new();

        for (id, (_expr, schedule, worker, enabled, in_flight)) in &self.workers {
            if !enabled {
                continue;
            }
            // Anchor on the passed `now` (via `after`), NOT `upcoming` —
            // `upcoming` re-anchors on the real system clock, which makes
            // the fire decision depend on wall time instead of the tick
            // instant and breaks deterministic/triggered evaluation.
            let should_fire = schedule
                .after(&now.with_timezone(&self.tz))
                .next()
                .is_some_and(|next| {
                    let diff = (next.with_timezone(&Utc) - now)
                        .num_seconds()
                        .unsigned_abs();
                    // Fire if the next scheduled time is within the tick window
                    diff < interval.as_secs() + 1
                });
            if !should_fire {
                continue;
            }
            // F5: skip a worker that is still running from a previous FIRE.
            // The flag is claimed ONLY once the fire decision is made:
            // InFlightGuard lives inside the spawned fire task, so claiming
            // the flag before the decision latches it forever on the first
            // out-of-window tick and the worker never fires again
            // (regression: production daemons ran 16 workers, zero cron
            // fires — pinned by tick_out_of_window_does_not_latch_in_flight).
            if in_flight.swap(true, std::sync::atomic::Ordering::AcqRel) {
                debug!(worker = %id, "scheduler: skipping worker (still in flight)");
                continue;
            }
            debug!(worker = %id, "scheduler: firing worker");
            to_fire.push((
                id.clone(),
                Arc::clone(worker),
                ctx.clone(),
                Arc::clone(in_flight),
            ));
        }

        for (id, worker, ctx, in_flight) in to_fire {
            if let Some(cost) = self.cost_cap_exceeded(&id, now) {
                warn!(
                    worker = %id,
                    cost = cost,
                    cap = self.cost_cap_usd,
                    "scheduler: skipping worker (monthly LLM cost cap exceeded)"
                );
                continue;
            }
            let cost_cap = self.cost_cap_usd;
            let costs = Arc::clone(&self.worker_costs);
            let month = month_key(now);
            tokio::spawn(async move {
                // F5: always clear the in-flight flag, success or failure.
                let _clear = InFlightGuard(&in_flight);
                match worker.execute(&ctx).await {
                    Ok(report) => {
                        info!(
                            worker = %report.worker_id,
                            success = report.success,
                            facts = report.fact_count,
                            duration_ms = report.duration_ms,
                            llm_cost_usd = report.llm_cost_usd,
                            "scheduler: worker completed"
                        );
                        if report.llm_cost_usd > 0.0 {
                            let mut ledger = costs.write().unwrap();
                            if ledger.month != month {
                                ledger.month = month;
                                ledger.costs.clear();
                            }
                            let entry = ledger.costs.entry(report.worker_id.clone()).or_insert(0.0);
                            *entry += report.llm_cost_usd;
                            if *entry >= cost_cap {
                                warn!(
                                    worker = %report.worker_id,
                                    cumulative_cost = *entry,
                                    cap = cost_cap,
                                    "scheduler: worker hit monthly cost cap, will skip next runs"
                                );
                            }
                            if let Some(path) = ledger.path.clone()
                                && let Err(e) =
                                    save_cost_ledger(&path, &ledger.month, &ledger.costs)
                            {
                                warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "scheduler: cost ledger persist failed (in-memory totals kept)"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!(worker = %id, error = %e, "scheduler: worker failed");
                    }
                }
            });
        }
    }

    /// Immediately trigger a named worker outside the scheduled loop.
    ///
    /// F5: returns a busy error (not WorkerNotFound) when the worker is
    /// still executing a previous run.
    pub async fn trigger(&self, name: &str) -> Result<WorkerReport, SchedulerError> {
        let (_, _, worker, _, in_flight) = self
            .workers
            .get(name)
            .ok_or_else(|| SchedulerError::WorkerNotFound(name.to_string()))?;

        // F5: manual runs must not overlap cron runs of the same worker.
        if in_flight.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Err(SchedulerError::WorkerNotFound(format!(
                "{name} (busy: previous run still in flight)"
            )));
        }
        let _clear = InFlightGuard(in_flight);

        let ctx = WorkerContext::new(Utc::now());
        info!(worker = %name, "scheduler: manual trigger");
        worker.execute(&ctx).await.map_err(|e| {
            error!(worker = %name, error = %e, "scheduler: triggered worker failed");
            SchedulerError::WorkerNotFound(name.to_string())
        })
    }

    /// Return the IDs of all registered workers (deterministic insertion order).
    ///
    /// Used as the test seam for profile-based worker-set assertions.
    // Unit C will consume this accessor; scoped allow until then.
    #[allow(dead_code)]
    pub fn worker_ids(&self) -> Vec<String> {
        self.workers.keys().cloned().collect()
    }

    /// List all registered workers with their schedules and descriptions.
    pub fn list(&self) -> Vec<WorkerSummary> {
        self.workers
            .iter()
            .map(
                |(id, (expr, _schedule, worker, enabled, _in_flight))| WorkerSummary {
                    id: id.clone(),
                    schedule: expr.clone(),
                    description: worker.description().to_string(),
                    enabled: *enabled,
                },
            )
            .collect()
    }

    /// Enable a worker by name. Returns `Err` if no worker with that ID is registered.
    pub fn enable(&mut self, name: &str) -> Result<(), SchedulerError> {
        let entry = self
            .workers
            .get_mut(name)
            .ok_or_else(|| SchedulerError::WorkerNotFound(name.to_string()))?;
        entry.3 = true;
        info!(worker = %name, "scheduler: worker enabled");
        Ok(())
    }

    /// Disable a worker by name. Returns `Err` if no worker with that ID is registered.
    pub fn disable(&mut self, name: &str) -> Result<(), SchedulerError> {
        let entry = self
            .workers
            .get_mut(name)
            .ok_or_else(|| SchedulerError::WorkerNotFound(name.to_string()))?;
        entry.3 = false;
        info!(worker = %name, "scheduler: worker disabled");
        Ok(())
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
    pub enabled: bool,
}

// ─── Scheduler profiles ────────────────────────────────────────────────

/// Worker-set profile that controls which background workers a
/// [`ZenScheduler`] registers.
///
/// Profiles let the same scheduler infrastructure serve two distinct
/// hosting contexts without duplicating registration logic:
///
/// | Profile | Host | Workers | Rationale |
/// |---------|------|---------|-----------|
/// | `Full` | `zen serve start` daemon | All 16 | Daemon is the sole owner of the memvid store and the morning-brief outbox — needs every worker. |
/// | `InApp` | TUI session (in-process) | 14 (excludes `memvid-indexer`, `morning-brief`) | The memvid indexer requires exclusive file-system flock held by the daemon; morning-brief stages to an outbox consumed by the daemon's `OutboxDrainer`. Running either in the TUI would race the daemon. |
///
/// Both profiles share identical cron schedules (from `CronConfig`), timezone
/// wiring (`with_timezone`), and per-worker config gates (e.g.
/// `[agentic.loop].enabled` for `zen-loop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchedulerProfile {
    /// Full daemon profile — all 16 background workers.
    ///
    /// Used by `zen serve start` where the process is the sole long-lived
    /// owner of shared resources (memvid store, outbox drainer, git
    /// work-tree).
    Full,

    /// In-app learning-core profile — 14 workers, excluding
    /// `memvid-indexer` and `morning-brief`.
    ///
    /// Used by the TUI when hosting a lightweight in-process scheduler for
    /// the learning-core pipeline. The two excluded workers are
    /// daemon-exclusive:
    /// - **`memvid-indexer`**: acquires an exclusive flock on the memvid
    ///   store; the daemon already holds it.
    /// - **`morning-brief`**: stages outbox files consumed by the daemon's
    ///   `OutboxDrainer`; a TUI duplicate would create races.
    InApp,
}

// ─── Convenience constructor ──────────────────────────────────────────

/// Create a fully-configured [`ZenScheduler`] with all built-in workers
/// registered. This is the primary way to bootstrap scheduling in the
/// TUI.
///
/// Registers:
/// - `memory-curator` (MemoryCurator): runs every 5 minutes, checks daily log, updates MEMORY.md
/// - `subconscious`: runs every 5 minutes, evaluates workspace state
/// - `dream`: runs 2AM, executes the nightly consolidation cycle
/// - `session-journaler`: runs every 5 minutes, extracts journal entries from session conversations
/// - `notion-extractor`: runs every 10 minutes, extracts notions from journal entries into state.db
/// - `wiki-compiler`: runs every 30 minutes, compiles wiki pages from state.db notions
/// - `commitment-tracker` (CommitmentTracker): runs daily 8AM, tracks commitments from journal entries
/// - `reflection-worker` (ReflectionWorker): runs daily 6AM, aggregates reflections into wiki/wisdom/
/// - `wisdom-synth` (WisdomSynthesizer): runs weekly Sun 2AM (cron: `0 0 2 * * 7`), synthesizes reflections + beliefs into wisdom candidates
/// - `express` (ExpressWorker): runs weekly Sat 3PM (cron: `0 0 15 * * 6`), LLM expression of insights into publishable review and blog drafts
/// - `memvid-indexer` (MemvidIndexerWorker): runs nightly 1AM (cron: `0 0 1 * * *`), ingests journal, wiki, wisdom into memvid store
/// - `evidence-gatherer` (EvidenceGatherer): runs weekly Mon 6AM (cron: `0 0 6 * * 1`), scans beliefs with low evidence count, generates research suggestions
pub fn create_default_scheduler() -> ZenScheduler {
    // Utc on purpose: this constructor never loads config (dormant-safe
    // set). The production serve/TUI path uses create_configured_scheduler,
    // which honors [cron].timezone / ZEN_CRON_TIMEZONE.
    let mut scheduler = ZenScheduler::new();

    // ── Critical workers: failure panics (memory pipeline broken without them) ──
    scheduler
        .register(SessionJournaler::new())
        .unwrap_or_else(|e| {
            panic!("FATAL: critical worker 'session-journaler' failed to register: {e}")
        });
    scheduler.register(DreamWorker::new()).unwrap_or_else(|e| {
        panic!("FATAL: critical worker 'dream-worker' failed to register: {e}")
    });
    scheduler
        .register(MemoryCurator::new())
        .unwrap_or_else(|e| {
            panic!("FATAL: critical worker 'memory-curator' failed to register: {e}")
        });
    scheduler
        .register(MemvidIndexerWorker::new())
        .unwrap_or_else(|e| {
            panic!("FATAL: critical worker 'memvid-indexer' failed to register: {e}")
        });

    // ── Non-critical workers: failure warns (system continues with reduced capability) ──
    if let Err(e) = scheduler.register(SubconsciousWorker::new()) {
        warn!("scheduler: failed to register subconscious worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(NotionExtractorWorker::new()) {
        warn!("scheduler: failed to register notion-extractor worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(WikiCompilerWorker::new()) {
        warn!("scheduler: failed to register wiki-compiler worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(CommitmentTracker::new()) {
        warn!("scheduler: failed to register commitment-tracker worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(ReflectionWorker::new()) {
        warn!("scheduler: failed to register reflection-worker worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(WisdomSynthesizer::new()) {
        warn!("scheduler: failed to register wisdom-synth worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(DecisionTracker::new()) {
        warn!("scheduler: failed to register decision-tracker worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(ExpressWorker::new()) {
        warn!("scheduler: failed to register express worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(EvidenceGatherer::new()) {
        warn!("scheduler: failed to register evidence-gatherer worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(MorningBriefWorker::new()) {
        warn!("scheduler: failed to register morning-brief worker (non-critical): {e}");
    }
    if let Err(e) = scheduler.register(PromotionWorker::with_paths()) {
        warn!("scheduler: failed to register promotion worker (non-critical): {e}");
    }

    // Knowledge-processing loop: the default scheduler enables zen-loop with
    // its built-in interval; the configured scheduler gates it on
    // [agentic.loop] instead. Kept set-equal with create_configured_scheduler.
    if let Err(e) = scheduler.register(ZenLoopWorker::new()) {
        warn!("scheduler: failed to register zen-loop worker (non-critical): {e}");
    }

    scheduler
}

/// Create a [`ZenScheduler`] wired with `CronConfig` values for worker
/// schedules, selecting the worker set via [`SchedulerProfile`].
///
/// Uses `default_daily_log_schedule()` / `default_night_dream_schedule()` as
/// fallbacks when config fields are `None`.
///
/// The two profiles share identical cron schedules, timezone wiring, and
/// per-worker config gates — the only difference is which workers are
/// registered (see [`SchedulerProfile`] docs for the exclusion rationale).
pub fn create_configured_scheduler_with(
    config: &CronConfig,
    profile: SchedulerProfile,
) -> ZenScheduler {
    let mut scheduler = ZenScheduler::new().with_timezone(config.timezone_or_default());

    // The monthly cost cap only means something across restarts: persist the
    // per-worker totals beside the other loop sidecars. Fail-open — an
    // unresolvable ZEN_HOME degrades to the previous in-memory-only accrual.
    if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
        scheduler = scheduler.with_cost_ledger(paths.logs().join("worker-costs.json"));
    }

    let dl_schedule = config
        .daily_log_schedule()
        .unwrap_or_else(|| default_daily_log_schedule().to_string());
    if let Err(e) = scheduler.register(MemoryCurator::new().with_schedule(&dl_schedule)) {
        warn!("scheduler: failed to register memory-curator: {e}");
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

    if let Err(e) = scheduler.register(NotionExtractorWorker::new()) {
        warn!("scheduler: failed to register notion-extractor worker: {e}");
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

    let wisdom_schedule = config
        .wisdom_synthesis_schedule
        .clone()
        .unwrap_or_else(|| default_wisdom_synthesis_schedule().to_string());
    if let Err(e) = scheduler.register(WisdomSynthesizer::new().with_schedule(&wisdom_schedule)) {
        warn!("scheduler: failed to register wisdom-synth worker: {e}");
    }

    if let Err(e) = scheduler.register(DecisionTracker::new()) {
        warn!("scheduler: failed to register decision-tracker worker: {e}");
    }

    if let Err(e) = scheduler.register(ExpressWorker::new()) {
        warn!("scheduler: failed to register express worker: {e}");
    }

    // Daemon-only: memvid-indexer holds an exclusive flock on the memvid
    // store — the TUI must not race the daemon's instance.
    if profile == SchedulerProfile::Full
        && let Err(e) = scheduler.register(MemvidIndexerWorker::new())
    {
        warn!("scheduler: failed to register memvid-indexer worker: {e}");
    }

    if let Err(e) = scheduler.register(EvidenceGatherer::new()) {
        warn!("scheduler: failed to register evidence-gatherer worker: {e}");
    }

    // Daemon-only: morning-brief stages outbox files consumed by the
    // daemon's OutboxDrainer — a TUI duplicate would create races.
    if profile == SchedulerProfile::Full
        && let Err(e) = scheduler.register(MorningBriefWorker::new())
    {
        warn!("scheduler: failed to register morning-brief worker: {e}");
    }

    if let Err(e) = scheduler.register(PromotionWorker::with_paths()) {
        warn!("scheduler: failed to register promotion worker: {e}");
    }

    // Knowledge-processing loop: interval + enabled come from
    // [agentic.loop]; disabled → manual-only (`zen wiki loop run`).
    if let Ok(config) = zen_core::config::load_config() {
        let loop_cfg = &config.agentic.loop_cfg;
        let mut loop_worker = ZenLoopWorker::new().with_schedule(loop_cfg.interval_or_default());
        if !loop_cfg.enabled_or_default() {
            loop_worker = loop_worker.disabled();
        }
        if let Err(e) = scheduler.register(loop_worker) {
            warn!("scheduler: failed to register zen-loop worker (non-critical): {e}");
        }
    }

    scheduler
}

/// Create a [`ZenScheduler`] wired with `CronConfig` values for worker schedules.
///
/// Convenience wrapper around [`create_configured_scheduler_with`] with
/// [`SchedulerProfile::Full`] — all 16 workers registered. Existing call
/// sites (TUI, serve command) use this entry point.
pub fn create_configured_scheduler(config: &CronConfig) -> ZenScheduler {
    create_configured_scheduler_with(config, SchedulerProfile::Full)
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
                llm_cost_usd: 0.0,
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

    struct CountingWorker(Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait::async_trait]
    impl ZenWorker for CountingWorker {
        fn id(&self) -> &'static str {
            "count"
        }
        fn description(&self) -> &'static str {
            "counts executions"
        }
        fn schedule(&self) -> &'static str {
            "0 0 9 * * *"
        }
        async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(WorkerReport {
                worker_id: "count".to_string(),
                success: true,
                fact_count: 0,
                duration_ms: 0,
                llm_cost_usd: 0.0,
            })
        }
    }

    #[tokio::test]
    async fn test_tick_evaluates_cron_in_configured_timezone() {
        use chrono::TimeZone;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // 2026-09-06 00:59:50 Utc == 08:59:50 Shanghai: the 9am cron is due
        // within the tick window in Shanghai, 8h away in Utc.
        let now = Utc.with_ymd_and_hms(2026, 9, 6, 0, 59, 50).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let mut shanghai = ZenScheduler::new().with_timezone(chrono_tz::Asia::Shanghai);
        shanghai
            .register(CountingWorker(Arc::clone(&counter)))
            .unwrap();
        shanghai.tick(now).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1, "9am fires in Shanghai");

        let counter = Arc::new(AtomicUsize::new(0));
        let mut utc = ZenScheduler::new();
        utc.register(CountingWorker(Arc::clone(&counter))).unwrap();
        utc.tick(now).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 0, "not due in Utc");
    }

    #[tokio::test]
    async fn tick_out_of_window_does_not_latch_in_flight() {
        use chrono::TimeZone;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Regression (B5 pilot, 2026-09-14): in_flight.swap(true) ran BEFORE
        // the fire decision, so the first out-of-window tick latched the flag
        // with no InFlightGuard to clear it — every later tick skipped the
        // worker as "still in flight" and production daemons never fired any
        // cron worker (16 registered, zero fires over 60min).
        let counter = Arc::new(AtomicUsize::new(0));
        let mut sched = ZenScheduler::new();
        sched
            .register(CountingWorker(Arc::clone(&counter)))
            .unwrap();

        sched
            .tick(Utc.with_ymd_and_hms(2026, 9, 14, 8, 0, 0).unwrap())
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "out-of-window tick (08:00 vs 09:00 cron) must not fire"
        );

        sched
            .tick(Utc.with_ymd_and_hms(2026, 9, 14, 8, 59, 50).unwrap())
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "in-window tick (08:59:50, 10s to cron) must fire after an out-of-window tick"
        );
    }

    /// Worker whose reported cost comes from the real metering path:
    /// metered `ModelMetadata` pricing ($1/M in, $2/M out) × usage
    /// (5M in + 3M out) = $11 ≥ the $10 default cap.
    struct CostWorker(Arc<std::sync::atomic::AtomicUsize>);

    impl CostWorker {
        fn metered_cost_usd() -> f64 {
            let meta = zen_provider::ModelMetadata {
                name: "metered".to_string(),
                provider: "openai".to_string(),
                context_window: 0,
                input_cost_per_million: 1.0,
                output_cost_per_million: 2.0,
                capabilities: Vec::new(),
                is_local: false,
            };
            zen_provider::usage_to_cost_usd(&meta, 5_000_000, 3_000_000)
        }
    }

    #[async_trait::async_trait]
    impl ZenWorker for CostWorker {
        fn id(&self) -> &'static str {
            "cost"
        }
        fn description(&self) -> &'static str {
            "reports metered LLM cost"
        }
        fn schedule(&self) -> &'static str {
            "*/1 * * * * *"
        }
        async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(WorkerReport {
                worker_id: "cost".to_string(),
                success: true,
                fact_count: 0,
                duration_ms: 0,
                llm_cost_usd: Self::metered_cost_usd(),
            })
        }
    }

    #[tokio::test]
    async fn metered_llm_cost_trips_cap_and_skips_subsequent_fires() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        assert!(
            (CostWorker::metered_cost_usd() - 11.0).abs() < 1e-9,
            "metering must derive $11 from usage × pricing"
        );

        let counter = Arc::new(AtomicUsize::new(0));
        let mut sched = ZenScheduler::new();
        sched.register(CostWorker(Arc::clone(&counter))).unwrap();

        let now = Utc::now();
        sched.tick(now).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "first fire runs under an empty ledger"
        );
        assert_eq!(
            sched.cost_cap_exceeded("cost", now),
            Some(11.0),
            "accumulated metered cost must be at/above the $10 cap"
        );

        sched.tick(now).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "the real tick-time cap check must skip the worker"
        );
    }

    #[tokio::test]
    async fn cost_ledger_persists_monthly_totals_and_reseeds_the_cap_check() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join("worker-costs.json");

        let counter = Arc::new(AtomicUsize::new(0));
        let mut first = ZenScheduler::new().with_cost_ledger(ledger.clone());
        first.register(CostWorker(Arc::clone(&counter))).unwrap();
        let now = Utc::now();
        first.tick(now).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        let raw = std::fs::read_to_string(&ledger).expect("sidecar persisted");
        let file: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(file["month"], month_key(now));
        assert!(file["costs"]["cost"].as_f64().unwrap() >= 10.0);

        let counter2 = Arc::new(AtomicUsize::new(0));
        let mut restarted = ZenScheduler::new().with_cost_ledger(ledger);
        restarted
            .register(CostWorker(Arc::clone(&counter2)))
            .unwrap();
        restarted.tick(Utc::now()).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            counter2.load(Ordering::SeqCst),
            0,
            "a restart within the month must reseed the cap from the sidecar"
        );
    }

    #[tokio::test]
    async fn stale_month_ledger_does_not_trip_the_cap() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join("worker-costs.json");
        std::fs::write(&ledger, r#"{"month":"2020-01","costs":{"cost":999.0}}"#).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let mut sched = ZenScheduler::new().with_cost_ledger(ledger);
        sched.register(CostWorker(Arc::clone(&counter))).unwrap();
        sched.tick(Utc::now()).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "a previous month's spend must not block the current month"
        );
    }

    #[tokio::test]
    async fn corrupt_cost_ledger_fails_open() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join("worker-costs.json");
        std::fs::write(&ledger, "not json {{{").unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let mut sched = ZenScheduler::new().with_cost_ledger(ledger);
        assert!(sched.cost_cap_exceeded("cost", Utc::now()).is_none());
        sched.register(CostWorker(Arc::clone(&counter))).unwrap();
        sched.tick(Utc::now()).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "a corrupt sidecar degrades to fresh in-memory accrual"
        );
    }

    #[tokio::test]
    async fn run_with_shutdown_exits_on_signal() {
        let scheduler = ZenScheduler::new();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(scheduler.run_with_shutdown(rx));
        // Let the loop start, then signal.
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send_replace(true);
        let result = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(
            result.is_ok(),
            "run_with_shutdown must exit on signal, got {result:?}"
        );
    }

    #[tokio::test]
    async fn run_with_shutdown_exits_when_already_signalled() {
        let scheduler = ZenScheduler::new();
        let (_tx, rx) = tokio::sync::watch::channel(true);
        let task = tokio::spawn(scheduler.run_with_shutdown(rx));
        let result = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(
            result.is_ok(),
            "run_with_shutdown must exit when already signalled, got {result:?}"
        );
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
        assert_eq!(items.len(), 16);
        assert!(items.iter().any(|w| w.id == "memory-curator"));
        assert!(items.iter().any(|w| w.id == "dream"));
        assert!(items.iter().any(|w| w.id == "subconscious"));
        assert!(items.iter().any(|w| w.id == "session-journaler"));
        assert!(items.iter().any(|w| w.id == "notion-extractor"));
        assert!(items.iter().any(|w| w.id == "wiki-compiler"));
        assert!(items.iter().any(|w| w.id == "commitment-tracker"));
        assert!(items.iter().any(|w| w.id == "reflection-worker"));
        assert!(items.iter().any(|w| w.id == "wisdom-synth"));
        assert!(items.iter().any(|w| w.id == "decision-tracker"));
        assert!(items.iter().any(|w| w.id == "express"));
        assert!(items.iter().any(|w| w.id == "memvid-indexer"));
        assert!(items.iter().any(|w| w.id == "evidence-gatherer"));
        assert!(items.iter().any(|w| w.id == "morning-brief"));
        assert!(items.iter().any(|w| w.id == "promotion"));
        assert!(items.iter().any(|w| w.id == "zen-loop"));
    }

    #[test]
    fn test_create_configured_scheduler_core_workers() {
        // zen-loop is config-gated (enabled flag via load_config), so only
        // the 15 core workers are asserted unconditionally.
        let scheduler = create_configured_scheduler(&CronConfig::default());
        let items = scheduler.list();
        assert!(items.len() >= 15);
        for id in [
            "memory-curator",
            "dream",
            "subconscious",
            "session-journaler",
            "notion-extractor",
            "wiki-compiler",
            "commitment-tracker",
            "reflection-worker",
            "wisdom-synth",
            "decision-tracker",
            "express",
            "memvid-indexer",
            "evidence-gatherer",
            "morning-brief",
            "promotion",
        ] {
            assert!(
                items.iter().any(|w| w.id == id),
                "configured scheduler missing worker '{id}'"
            );
        }
    }

    // ── SchedulerProfile tests ─────────────────────────────────────────

    /// Expected worker IDs for each profile.
    const FULL_WORKERS: &[&str] = &[
        "memory-curator",
        "subconscious",
        "dream",
        "session-journaler",
        "notion-extractor",
        "wiki-compiler",
        "commitment-tracker",
        "reflection-worker",
        "wisdom-synth",
        "decision-tracker",
        "express",
        "memvid-indexer",
        "evidence-gatherer",
        "morning-brief",
        "promotion",
        "zen-loop",
    ];

    const INAPP_WORKERS: &[&str] = &[
        "memory-curator",
        "subconscious",
        "dream",
        "session-journaler",
        "notion-extractor",
        "wiki-compiler",
        "commitment-tracker",
        "reflection-worker",
        "wisdom-synth",
        "decision-tracker",
        "express",
        "evidence-gatherer",
        "promotion",
        "zen-loop",
    ];

    #[test]
    fn test_full_profile_has_all_16_workers() {
        let scheduler =
            create_configured_scheduler_with(&CronConfig::default(), SchedulerProfile::Full);
        let ids = scheduler.worker_ids();
        assert_eq!(ids.len(), 16, "Full profile must register 16 workers");
        for expected in FULL_WORKERS {
            assert!(
                ids.contains(&expected.to_string()),
                "Full profile missing worker '{expected}'"
            );
        }
    }

    #[test]
    fn test_inapp_profile_has_14_workers() {
        let scheduler =
            create_configured_scheduler_with(&CronConfig::default(), SchedulerProfile::InApp);
        let ids = scheduler.worker_ids();

        // Exactly 14 workers (zen-loop may or may not register depending on
        // load_config; assert >= 14 to handle the config-gated edge).
        assert!(
            ids.len() >= 14,
            "InApp profile must register at least 14 workers, got {}",
            ids.len()
        );

        // Inclusion: all 14 expected workers present.
        for expected in INAPP_WORKERS {
            assert!(
                ids.contains(&expected.to_string()),
                "InApp profile missing worker '{expected}'"
            );
        }

        // Exclusion: daemon-only workers must NOT be present.
        assert!(
            !ids.contains(&"memvid-indexer".to_string()),
            "InApp profile must NOT include 'memvid-indexer' (daemon flock)"
        );
        assert!(
            !ids.contains(&"morning-brief".to_string()),
            "InApp profile must NOT include 'morning-brief' (daemon outbox drainer)"
        );
    }

    #[test]
    fn test_create_configured_scheduler_delegates_to_full() {
        let scheduler = create_configured_scheduler(&CronConfig::default());
        let ids = scheduler.worker_ids();
        assert!(
            ids.contains(&"memvid-indexer".to_string()),
            "create_configured_scheduler must include memvid-indexer (Full profile)"
        );
        assert!(
            ids.contains(&"morning-brief".to_string()),
            "create_configured_scheduler must include morning-brief (Full profile)"
        );
    }

    #[test]
    fn test_inapp_profiles_same_cron_schedules_as_full() {
        let full = create_configured_scheduler_with(&CronConfig::default(), SchedulerProfile::Full);
        let inapp =
            create_configured_scheduler_with(&CronConfig::default(), SchedulerProfile::InApp);

        let full_map: std::collections::HashMap<_, _> = full
            .list()
            .into_iter()
            .map(|w| (w.id.clone(), w.schedule))
            .collect();
        let inapp_map: std::collections::HashMap<_, _> = inapp
            .list()
            .into_iter()
            .map(|w| (w.id.clone(), w.schedule))
            .collect();

        for (id, schedule) in &inapp_map {
            assert_eq!(
                full_map.get(id).map(String::as_str),
                Some(schedule.as_str()),
                "InApp worker '{id}' has a different cron schedule than Full"
            );
        }
    }
}
