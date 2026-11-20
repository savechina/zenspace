//! `plan.execute` — model-driven workflow DAG (001 A.2/A.6, convergence T375).
//!
//! The orchestrator's model drafts a plan: `{name?, tasks: [{id, agent,
//! prompt, depends_on: []}]}`. The tool validates the DAG (unique ids,
//! known deps, acyclic, ≤12 tasks, ≤3 layers), then runs every task
//! through [`DelegateTaskTool::validate_task`] — the SAME hard gate path
//! as `delegate.task` (known agent, tier matrix, depth cap, bounded
//! prompt) — and executes it layer by layer: tasks within one layer run
//! concurrently through [`DelegateTaskTool`]'s `run_single` (inheriting
//! token budget and wall-clock timeout). The merged deliverable goes
//! through the quality gate (Metis→Momus→Hermes) as the
//! plan-completion signal. Downstream tasks of a failed/skipped node are
//! skipped, never run against stale inputs. One `loop.plan.completed`
//! audit line lands in `<logs>/audit.jsonl` per plan.
//!
//! W2.1 (D11): each task carries an optional `on_failure` transition. The
//! default `skip` is the behavior above (byte-identical). `retry` re-queues
//! ONLY the failed node once — verified siblings keep their checkpoints, so
//! repair is localized to the failed node's minimal subgraph (ATG); `halt`
//! stops the layer chain. A failed plan also persists a W2.2 rewind memory
//! (see [`record_plan_rewind`]).

use std::sync::Arc;
use std::time::Instant;

use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use tracing::info;
use zen_core::types::Sensitivity;
use zen_plugin::tools::approval_hook::fire_watchdog_renewal;

use crate::delegate_task::{DELEGATE_ROOT_PARENT, DelegateRequest, DelegateTaskTool, ForkMode};
use crate::review::QualityPipeline;

pub const PLAN_TOOL_NAME: &str = "plan.execute";

/// Hard cap on plan size — bigger workflows must be split by the model
/// into multiple plans (001 A.6: consumer-side decision).
pub const MAX_PLAN_TASKS: usize = 12;

/// W9.4 failure triage: (lowercase needle, proposal template) pairs for
/// RECOGNIZED failure classes only. `{plan_id}` is substituted with the
/// plan's id at render time. A failed task whose error matches no needle
/// contributes NO proposal — the raw error stays visible in the task row
/// instead of a guessed repair. Proposals are text-only; nothing here
/// re-runs anything automatically (human picks, resume reuses ok
/// checkpoints).
const TRIAGE_TABLE: &[(&str, &str)] = &[
    (
        "timed out",
        "1. task timed out — re-run only the failed subgraph: plan.execute {\"resume_plan_id\": \"{plan_id}\"} (ok checkpoints replay, the timed-out node re-runs); consider a smaller fan-out",
    ),
    (
        "timeout",
        "1. task timed out — re-run only the failed subgraph: plan.execute {\"resume_plan_id\": \"{plan_id}\"} (ok checkpoints replay, the timed-out node re-runs); consider a smaller fan-out",
    ),
    (
        "no local provider",
        "2. no local provider reachable — resolve the local LLM (zen provider status), then resume: plan.execute {\"resume_plan_id\": \"{plan_id}\"}",
    ),
    (
        "provider unavailable",
        "2. provider unavailable — check provider credentials/health (zen provider status), then resume: plan.execute {\"resume_plan_id\": \"{plan_id}\"}",
    ),
    (
        "budget",
        "3. budget exhausted — resume with fewer concurrent tasks so the reserve fits: plan.execute {\"resume_plan_id\": \"{plan_id}\"}",
    ),
    (
        "approval",
        "4. approval required — grant the flagged action, then resume: plan.execute {\"resume_plan_id\": \"{plan_id}\"}",
    ),
];

/// W9.4: build the triage proposals for a failed plan. Deduplicates by
/// proposal text, caps at 3, and only matches RECOGNIZED classes —
/// unknown error text yields zero proposals (the pin contract).
fn triage_proposals(plan_id: &str, failed_errors: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for error in failed_errors {
        let lower = error.to_lowercase();
        for (needle, template) in TRIAGE_TABLE {
            if lower.contains(needle) {
                let proposal = template.replace("{plan_id}", plan_id);
                if !out.contains(&proposal) {
                    out.push(proposal);
                }
                break;
            }
        }
        if out.len() >= 3 {
            break;
        }
    }
    out
}

/// Hard cap on DAG depth in layers. Three layers cover draft → parallel
/// analysis → synthesis; anything deeper smells like a plan that should
/// itself be delegated (and hits the delegate depth matrix anyway).
pub const MAX_PLAN_LAYERS: usize = 3;

/// W8.1 progress sink — the streaming host (gateway `hosting.rs`) scopes
/// it task-locally around the turn execution, the SAME pattern as
/// `WATCHDOG_RENEWAL`. The wave loop fires one line per wave transition;
/// the sink forwards it to the live delta transport. No scope (non-stream
/// turns, plain tests) = silent no-op — the line still lands in the
/// result JSON `progress` blob, so both transports carry the same rows.
pub type PlanProgressSink = Arc<dyn Fn(String) + Send + Sync>;

tokio::task_local! {
    pub static PLAN_PROGRESS: std::cell::RefCell<Option<PlanProgressSink>>;
}

/// Fire one progress line through the scoped sink. Never panics outside
/// a scope (task-local `try_with` miss = no-op).
pub(crate) fn fire_plan_progress(line: String) {
    let _ = PLAN_PROGRESS.try_with(|cell| {
        if let Some(sink) = cell.borrow().as_ref() {
            sink(line);
        }
    });
}

/// W2.1 (D11): failure-transition edge for one plan node (GraphPlan,
/// ESWA). `Skip` is the pre-W2 behavior and the default, so a plan without
/// `on_failure` is byte-identical to before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureAction {
    /// Downstream dependents are skipped; the plan continues. Default.
    Skip,
    /// Re-queue THIS task exactly once; verified siblings keep their
    /// checkpoints (minimal-subgraph repair).
    Retry,
    /// Stop the layer chain: no further layers run.
    Halt,
}

impl FailureAction {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "skip" => Ok(Self::Skip),
            "retry" => Ok(Self::Retry),
            "halt" => Ok(Self::Halt),
            other => Err(format!(
                "invalid on_failure `{other}` (expected retry|skip|halt)"
            )),
        }
    }
}

/// One node of the model-drafted DAG.
#[derive(Debug, Clone)]
pub struct PlanTaskSpec {
    pub id: String,
    pub agent: String,
    pub prompt: String,
    pub depends_on: Vec<String>,
    /// W2.1 (D11): failure-transition edge (default [`FailureAction::Skip`]).
    pub on_failure: FailureAction,
    /// W2.2 (D12): optional sandbox-validated artifact output path.
    pub artifact: Option<String>,
    /// W11.5/T5 (SC-021): per-task context fork mode. `None` (absent) keeps
    /// the session-wide `[agentic.delegate] context_inheritance` default
    /// byte-identically; the per-task argument wins when present.
    pub fork_mode: Option<ForkMode>,
    /// W11.5/T5 (SC-022): per-task model hint (catalog-validated, ≤5 distinct
    /// per plan). Resolution: sensitivity > LlmPreference > hint > default;
    /// the runtime override rides D2b — this wave delivers validation + the
    /// falsifiable `model_source` label.
    pub model_hint: Option<String>,
}

/// The full plan as parsed from tool args.
#[derive(Debug, Clone)]
pub struct PlanSpec {
    pub name: Option<String>,
    pub tasks: Vec<PlanTaskSpec>,
}

/// Per-task execution outcome assembled while walking the layers.
#[derive(Debug, Clone)]
struct TaskOutcome {
    status: &'static str, // ok | failed | skipped
    value: serde_json::Value,
}

pub struct PlanExecuteTool {
    delegate: Arc<DelegateTaskTool>,
    pipeline: QualityPipeline,
    max_concurrent: u32,
    state_db: Option<std::path::PathBuf>,
    db: std::sync::OnceLock<Option<Arc<zen_repo::SqliteClient>>>,
    #[cfg(test)]
    test_failures: std::sync::Mutex<std::collections::HashMap<String, u32>>,
    #[cfg(test)]
    test_calls: std::sync::Mutex<std::collections::HashMap<String, u32>>,
    /// G-13 (SC-001/SC-002) wave instrumentation: ordered `(agent, event)`
    /// log, in-flight counter + peak, and per-agent test delays. Compiled
    /// only for unit tests; zero production footprint.
    #[cfg(test)]
    test_wave_log: std::sync::Mutex<Vec<(String, &'static str)>>,
    #[cfg(test)]
    test_in_flight: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    test_peak_in_flight: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    test_wave_delays: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

impl PlanExecuteTool {
    pub fn new(
        delegate: Arc<DelegateTaskTool>,
        pipeline: QualityPipeline,
        max_concurrent: u32,
        state_db: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            delegate,
            pipeline,
            max_concurrent: max_concurrent.clamp(1, 8),
            state_db,
            db: std::sync::OnceLock::new(),
            #[cfg(test)]
            test_failures: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(test)]
            test_calls: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(test)]
            test_wave_log: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            test_in_flight: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            test_peak_in_flight: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            test_wave_delays: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Test seam (W2.1): make the next `times` runs of `agent` fail before a
    /// real sub-turn runs, so the retry edge can be exercised deterministically
    /// (the mock provider never fails). Compiled only for unit tests.
    #[cfg(test)]
    pub(crate) fn inject_test_failure(&self, agent: &str, times: u32) {
        self.test_failures
            .lock()
            .unwrap()
            .insert(agent.to_string(), times);
    }

    #[cfg(test)]
    fn test_take_failure(&self, agent: &str) -> bool {
        let mut failures = self.test_failures.lock().unwrap();
        match failures.get_mut(agent) {
            Some(remaining) if *remaining > 0 => {
                *remaining -= 1;
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    fn test_note_call(&self, agent: &str) {
        *self
            .test_calls
            .lock()
            .unwrap()
            .entry(agent.to_string())
            .or_insert(0) += 1;
    }

    #[cfg(test)]
    pub(crate) fn test_call_count(&self, agent: &str) -> u32 {
        self.test_calls
            .lock()
            .unwrap()
            .get(agent)
            .copied()
            .unwrap_or(0)
    }

    /// G-13 wave instrumentation (test-only): append to the ordered
    /// `(agent, event)` log — `enter` / `exit` / `skipped`.
    #[cfg(test)]
    fn test_log_wave_event(&self, agent: &str, event: &'static str) {
        self.test_wave_log
            .lock()
            .unwrap()
            .push((agent.to_string(), event));
    }

    /// G-13 (test-only): the per-agent hold injected via
    /// `inject_test_wave_delay`, or `None`.
    #[cfg(test)]
    fn test_wave_delay(&self, agent: &str) -> Option<std::time::Duration> {
        let millis = *self.test_wave_delays.lock().unwrap().get(agent)?;
        Some(std::time::Duration::from_millis(millis))
    }

    /// G-13 test seam: hold each run of `agent` for `millis` after its
    /// enter event, so width-2 batch siblings observably overlap (SC-001).
    /// `tokio::time::sleep` always pends on first poll, so the overlap is
    /// structural, not a timing race.
    #[cfg(test)]
    pub(crate) fn inject_test_wave_delay(&self, agent: &str, millis: u64) {
        self.test_wave_delays
            .lock()
            .unwrap()
            .insert(agent.to_string(), millis);
    }

    /// G-13 (test-only): snapshot of the ordered wave event log.
    #[cfg(test)]
    pub(crate) fn test_wave_events(&self) -> Vec<(String, &'static str)> {
        self.test_wave_log.lock().unwrap().clone()
    }

    /// G-13 (test-only): peak in-flight task concurrency observed across
    /// the whole plan run (SC-001 evidence; never derived from time).
    #[cfg(test)]
    pub(crate) fn test_peak_in_flight(&self) -> usize {
        self.test_peak_in_flight
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Lazily open state.db once per tool lifetime; `None` disables
    /// persistence (tests, missing paths) without touching the happy path.
    async fn db(&self) -> Option<&Arc<zen_repo::SqliteClient>> {
        if let Some(client) = self.db.get() {
            return client.as_ref();
        }
        let path = self.state_db.as_ref()?;
        let client = zen_repo::SqliteClient::open_lazy(path).await.ok()?;
        let _ = self.db.set(Some(Arc::new(client)));
        self.db.get().and_then(|c| c.as_ref())
    }

    /// W9.1 dispatch forecast — one advisory line from recorded sub-turn
    /// history, emitted before the first batch. `None` (⇒ no line) when the
    /// plan store is unavailable or NO task's (agent, model) pair has
    /// recorded durations: history-less pairs are omitted, never guessed.
    /// Never errors, never blocks the walk.
    ///
    /// A-02/row13: keys are resolved with the SAME
    /// [`crate::delegate_task::resolved_model_label`] helper the delegate
    /// runtime producer uses, so the forecast looks up exactly what the
    /// checkpoint stores. Rows written by pre-A-02 binaries keyed on the
    /// declared hint are still honored as a fallback (the forecast is
    /// advisory; a legacy match only sharpens the estimate).
    async fn forecast_line(&self, plan: &PlanSpec, sensitivity: Sensitivity) -> Option<String> {
        let db = self.db().await?;
        let repo = zen_repo::WorkflowRepo::new(db);
        let history = repo.duration_history().await.ok()?;
        // Per (agent, model) pair: sorted durations + min/max cost.
        let mut pairs: std::collections::HashMap<(String, String), Vec<i64>> =
            std::collections::HashMap::new();
        let mut pair_costs: std::collections::HashMap<(String, String), Vec<f64>> =
            std::collections::HashMap::new();
        for row in history {
            let key = (row.agent, row.model.unwrap_or_default());
            pairs.entry(key.clone()).or_default().push(row.duration_ms);
            if let Some(cost) = row.cost_usd {
                pair_costs.entry(key).or_default().push(cost);
            }
        }
        let mut lo_ms = 0i64;
        let mut hi_ms = 0i64;
        let mut matched = 0usize;
        let mut has_cost_history = false;
        let mut cost_lo = 0.0f64;
        let mut cost_hi = 0.0f64;
        for t in &plan.tasks {
            // The resolved key is what the checkpoint stores now (real
            // model name); the declared hint is the pre-A-02 row key kept
            // as a fallback so durable older rows still contribute.
            let resolved =
                self.delegate
                    .model_history_key(&t.agent, t.model_hint.as_deref(), sensitivity);
            let legacy = t.model_hint.clone().unwrap_or_default();
            let key = if pairs.contains_key(&(t.agent.clone(), resolved.clone())) {
                (t.agent.clone(), resolved)
            } else {
                (t.agent.clone(), legacy)
            };
            let Some(durations) = pairs.get(&key) else {
                continue;
            };
            let mut sorted = durations.clone();
            sorted.sort_unstable();
            lo_ms += sorted[0];
            hi_ms += sorted[sorted.len() - 1];
            matched += 1;
            if let Some(costs) = pair_costs.get(&key) {
                let mut sorted = costs.clone();
                sorted.sort_by(|a, b| a.total_cmp(b));
                cost_lo += sorted[0];
                cost_hi += sorted[sorted.len() - 1];
                has_cost_history = true;
            }
        }
        if matched == 0 {
            return None;
        }
        let mins_lo = (lo_ms as f64 / 60_000.0).ceil() as i64;
        let mins_hi = (hi_ms as f64 / 60_000.0).ceil() as i64;
        let cost_segment = if has_cost_history {
            format!(" · ~${:.2}-${:.2}", cost_lo.max(0.0), cost_hi.max(0.0))
        } else {
            String::new()
        };
        Some(format!(
            "plan {} tasks · ~{mins_lo}-{mins_hi} min{cost_segment} (estimate)",
            plan.tasks.len()
        ))
    }

    /// W9.3 cancel finalization (W6.2b vocabulary): in-flight tasks are
    /// checkpointed `failed`, every never-started task `skipped`. Tasks
    /// already settled before the signal keep their recorded outcomes —
    /// only the cancelled batch loses its results, so no partial
    /// `completed` can appear after the cancel point.
    async fn mark_cancelled(
        &self,
        plan_id: &str,
        plan: &PlanSpec,
        outcomes: &mut [Option<TaskOutcome>],
        in_flight: &[usize],
        warnings: &mut Vec<String>,
    ) {
        for &i in in_flight {
            if outcomes[i].is_some() {
                continue;
            }
            let outcome = TaskOutcome {
                status: "failed",
                value: serde_json::json!({ "error": "plan cancelled mid-batch" }),
            };
            self.checkpoint_logged(plan_id, &plan.tasks[i], &outcome, warnings)
                .await;
            outcomes[i] = Some(outcome);
        }
        for (i, slot) in outcomes.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            let outcome = TaskOutcome {
                status: "skipped",
                value: serde_json::json!({ "error": "plan cancelled" }),
            };
            self.checkpoint_logged(plan_id, &plan.tasks[i], &outcome, warnings)
                .await;
            *slot = Some(outcome);
            fire_watchdog_renewal();
        }
    }

    /// Parse `{name?, tasks: [{id, agent, prompt, depends_on?}]}`.
    /// Agent existence is delegated to `run_single` (single source of
    /// truth: the registry); everything structural is checked here.
    pub(crate) fn parse_plan(args: &serde_json::Value) -> Result<PlanSpec, String> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let Some(arr) = args.get("tasks").and_then(|v| v.as_array()) else {
            return Err("plan.execute requires a `tasks` array".to_string());
        };
        if arr.is_empty() {
            return Err("plan.execute requires at least one task".to_string());
        }
        if arr.len() > MAX_PLAN_TASKS {
            return Err(format!(
                "plan has {} tasks; the cap is {MAX_PLAN_TASKS} — split into multiple plans",
                arr.len()
            ));
        }
        let mut tasks = Vec::with_capacity(arr.len());
        for item in arr {
            let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                return Err("every plan task needs a string `id`".to_string());
            };
            let Some(agent) = item.get("agent").and_then(|v| v.as_str()) else {
                return Err(format!("task `{id}` needs a string `agent`"));
            };
            let Some(prompt) = item.get("prompt").and_then(|v| v.as_str()) else {
                return Err(format!("task `{id}` needs a string `prompt`"));
            };
            let mut depends_on = Vec::new();
            if let Some(deps) = item.get("depends_on").and_then(|v| v.as_array()) {
                for d in deps {
                    let Some(dep) = d.as_str() else {
                        return Err(format!("task `{id}` has a non-string dependency"));
                    };
                    depends_on.push(dep.to_string());
                }
            }
            let fork_mode = match item.get("fork_mode") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => Some(
                    ForkMode::parse(
                        v.as_str()
                            .ok_or_else(|| format!("task `{id}` has a non-string `fork_mode`"))?,
                    )
                    .map_err(|e| format!("task `{id}`: {e}"))?,
                ),
            };
            let model_hint = match item.get("model") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(m)) if !m.trim().is_empty() => Some(m.clone()),
                Some(serde_json::Value::String(_)) => {
                    return Err(format!("task `{id}` has an empty `model` hint"));
                }
                Some(_) => return Err(format!("task `{id}` has a non-string `model` hint")),
            };
            tasks.push(PlanTaskSpec {
                id: id.to_string(),
                agent: agent.to_string(),
                prompt: prompt.to_string(),
                depends_on,
                on_failure: parse_failure_action(item, id)?,
                artifact: parse_artifact(item, id)?,
                fork_mode,
                model_hint,
            });
        }
        // W11.5/T5 (SC-022): ONE shared hint-set rule with delegate.task —
        // catalog-validated, ≤5 distinct across the plan batch.
        let hints: Vec<String> = tasks.iter().filter_map(|t| t.model_hint.clone()).collect();
        DelegateTaskTool::validate_model_hint_values(&hints)?;
        // Unique ids + dependencies that actually exist.
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        for (i, t) in tasks.iter().enumerate() {
            if ids[i + 1..].contains(&t.id.as_str()) {
                return Err(format!("duplicate task id `{}`", t.id));
            }
            for dep in &t.depends_on {
                if !ids.contains(&dep.as_str()) {
                    return Err(format!("task `{}` depends on unknown task `{dep}`", t.id));
                }
            }
        }
        Ok(PlanSpec { name, tasks })
    }

    /// Kahn layering: indices grouped so every task sits after all of its
    /// dependencies. `Err` on a dependency cycle.
    pub(crate) fn topological_layers(plan: &PlanSpec) -> Result<Vec<Vec<usize>>, String> {
        let n = plan.tasks.len();
        let index_of = |id: &str| plan.tasks.iter().position(|t| t.id == id);
        let mut indegree = vec![0usize; n];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, t) in plan.tasks.iter().enumerate() {
            for dep in &t.depends_on {
                let Some(j) = index_of(dep) else {
                    return Err(format!("task `{}` depends on unknown task `{dep}`", t.id));
                };
                indegree[i] += 1;
                dependents[j].push(i);
            }
        }
        let mut layers = Vec::new();
        let mut ready: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut placed = 0usize;
        while !ready.is_empty() {
            layers.push(ready.clone());
            placed += ready.len();
            let mut next = Vec::new();
            for &i in &ready {
                for &d in &dependents[i] {
                    indegree[d] -= 1;
                    if indegree[d] == 0 {
                        next.push(d);
                    }
                }
            }
            ready = next;
        }
        if placed < n {
            return Err("plan contains a dependency cycle".to_string());
        }
        if layers.len() > MAX_PLAN_LAYERS {
            return Err(format!(
                "plan spans {} layers; the cap is {MAX_PLAN_LAYERS} — restructure or split the plan",
                layers.len()
            ));
        }
        Ok(layers)
    }

    /// Persist one task outcome checkpoint; no-op without state.db.
    /// W12.18 (review Q5): persistence failures propagate — a discarded
    /// checkpoint error is a silent double-spend risk on resume (the task
    /// re-runs although its result exists). Callers go through
    /// [`Self::checkpoint_logged`].
    async fn checkpoint(
        &self,
        plan_id: &str,
        task: &PlanTaskSpec,
        outcome: &TaskOutcome,
    ) -> Result<(), String> {
        if plan_id.is_empty() {
            return Ok(());
        }
        let Some(db) = self.db().await else {
            return Ok(());
        };
        let repo = zen_repo::WorkflowRepo::new(db);
        repo.checkpoint_task(zen_repo::TaskCheckpoint {
            plan_id,
            task_id: &task.id,
            agent: &task.agent,
            status: outcome.status,
            response: outcome.value.get("response").and_then(|v| v.as_str()),
            error: outcome.value.get("error").and_then(|v| v.as_str()),
            now: chrono::Utc::now().timestamp(),
            // A-02/row13: `model` is the REAL runtime model name the
            // delegate produced (`resolved_model_label`: the steering hint
            // or the provider's configured default). The `model_source`
            // label survives only as an honest fallback for outcomes that
            // could not resolve a real name. `cost_usd` is metered from the
            // provider's real usage echo — absent (NULL), never estimated.
            model: outcome
                .value
                .get("model")
                .or_else(|| outcome.value.get("model_source"))
                .and_then(|v| v.as_str()),
            duration_ms: outcome.value.get("duration_ms").and_then(|v| v.as_u64()),
            cost_usd: outcome.value.get("cost_usd").and_then(|v| v.as_f64()),
        })
        .await
        .map_err(|e| e.to_string())
    }

    /// W12.18 (review Q5): checkpoint + structured persistence-failure
    /// capture. A failed checkpoint is a WARN with plan/task context and a
    /// `persistence_warnings` entry in the result payload — visible, never
    /// silent — while sibling tasks continue untouched (per-task error
    /// isolation, the batch-loop discipline).
    async fn checkpoint_logged(
        &self,
        plan_id: &str,
        task: &PlanTaskSpec,
        outcome: &TaskOutcome,
        warnings: &mut Vec<String>,
    ) {
        if let Err(e) = self.checkpoint(plan_id, task, outcome).await {
            tracing::warn!(
                plan_id,
                task = %task.id,
                error = %e,
                "plan checkpoint persistence failed — the outcome is not durable and a resume may re-run this task"
            );
            warnings.push(format!("checkpoint task `{}` failed: {e}", task.id));
        }
    }

    /// W12.15 (review N2): dispatch one plan sub-turn under the PLAN's
    /// recorded policy. The captured TurnContext keeps the session's
    /// knowledge/history; only `sensitivity` is restored from spec_json so
    /// provider routing + hook gates classify under the plan's policy —
    /// a Confidential plan resumed by a Public session never leaks to
    /// cloud. `None` = no recorded policy ⇒ turn default, byte-identical.
    async fn run_task_under_policy(
        &self,
        req: &DelegateRequest,
        plan_policy: Option<Sensitivity>,
    ) -> serde_json::Value {
        match plan_policy {
            None => self.delegate.run_single(req, 0, DELEGATE_ROOT_PARENT).await,
            Some(policy) => {
                let mut ctx = self.delegate.capture_turn_context();
                ctx.sensitivity = policy;
                zen_core::turn_context::TURN_CONTEXT
                    .scope(ctx, self.delegate.run_single(req, 0, DELEGATE_ROOT_PARENT))
                    .await
            }
        }
    }

    /// W12.16 (review Q1): the terminal plan-row write with the
    /// finalization-window cancel re-check. A cancel landing between the
    /// quality gate and this write (quality-gate tail window) must not be
    /// silently reverted: the watch signal is checked first (in-process
    /// `plan.control` cancels), and the SQL `status = 'running'` guard
    /// inside `complete_plan` is the authoritative backstop for flips this
    /// process never saw (drain finalizer, mark-only stale sweep,
    /// cross-process cancel). Returns `true` when the cancel won the tail
    /// window — the caller records `cancelled` and never rewrites the row.
    async fn complete_plan_checked(
        &self,
        repo: &zen_repo::WorkflowRepo<'_>,
        completion: PlanCompletion<'_>,
        cancel_rx: Option<&tokio::sync::watch::Receiver<bool>>,
        warnings: &mut Vec<String>,
    ) -> bool {
        let PlanCompletion {
            plan_id,
            final_status,
            summary,
            plan_approved,
            delivery_ready,
        } = completion;
        if cancel_rx.is_some_and(|rx| *rx.borrow()) {
            tracing::warn!(
                plan_id,
                "plan cancel observed in the finalization window — row stays terminal-cancelled, no complete_plan rewrite"
            );
            return true;
        }
        match repo
            .complete_plan(
                plan_id,
                final_status,
                Some(summary),
                Some(plan_approved),
                Some(delivery_ready),
                chrono::Utc::now().timestamp(),
            )
            .await
        {
            Ok(true) => false,
            Ok(false) => {
                tracing::warn!(
                    plan_id,
                    attempted = final_status,
                    "plan completion lost the terminal race — row already terminal (cancel wins), outcome preserved not rewritten"
                );
                true
            }
            Err(e) => {
                // W12.18 (review Q5): structured persistence failure — the
                // row stays `running` so resume remains possible; the
                // failure surfaces in the result payload, never silently.
                tracing::warn!(plan_id, error = %e, "plan completion persistence failed");
                warnings.push(format!("complete_plan failed: {e}"));
                false
            }
        }
    }

    /// The pipeline `Task` for the plan review — same shape as the
    /// orchestrator's turn-review task (entropy 0.0, sensitivity metadata).
    fn review_task(summary: &str, sensitivity: Sensitivity) -> zen_core::types::Task {
        // semantic_entropy is a placeholder (0.0) until T169's calibrated
        // complexity classifier feeds real values into the P8 routing tiers.
        let mut task = zen_core::types::Task::new(summary, 0.0, zen_core::types::TaskType::Text);
        task.metadata
            .insert("sensitivity".to_string(), sensitivity.to_string());
        task
    }

    /// One `loop.plan.completed` line in `<logs>/audit.jsonl`.
    fn append_plan_audit(&self, entry: serde_json::Value) {
        let Ok(paths) = zen_core::paths::ZenPaths::detect() else {
            return;
        };
        let log_path = paths.logs().join("audit.jsonl");
        if let Some(dir) = log_path.parent()
            && std::fs::create_dir_all(dir).is_ok()
        {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                let _ = writeln!(f, "{entry}");
            }
        }
    }
}

struct PlanCompletion<'a> {
    plan_id: &'a str,
    final_status: &'a str,
    summary: &'a str,
    plan_approved: bool,
    delivery_ready: bool,
}

const UPSTREAM_SNIPPET_MAX_CHARS: usize = 4000;

/// Parse a task's optional `on_failure` (absent/null ⇒ [`FailureAction::Skip`).
fn parse_failure_action(item: &serde_json::Value, id: &str) -> Result<FailureAction, String> {
    match item.get("on_failure") {
        None | Some(serde_json::Value::Null) => Ok(FailureAction::Skip),
        Some(serde_json::Value::String(value)) => {
            FailureAction::parse(value).map_err(|e| format!("task `{id}` has {e}"))
        }
        Some(_) => Err(format!("task `{id}` has a non-string `on_failure`")),
    }
}

/// Parse a task's optional artifact output path (absent/null ⇒ `None`;
/// present ⇒ non-empty).
fn parse_artifact(item: &serde_json::Value, id: &str) -> Result<Option<String>, String> {
    match item.get("artifact") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.clone()))
        }
        Some(serde_json::Value::String(_)) => {
            Err(format!("task `{id}` has an empty `artifact` path"))
        }
        Some(_) => Err(format!("task `{id}` has a non-string `artifact` path")),
    }
}

/// Extract the human-readable answer from a `run_single` outcome value —
/// the success payload carries the sub-agent answer in `response`; any
/// other shape falls back to the JSON itself.
fn upstream_snippet(value: &serde_json::Value) -> String {
    let text = match value.get("response").and_then(|v| v.as_str()) {
        Some(response) => response.to_string(),
        None => value.to_string(),
    };
    let mut snippet: String = text.chars().take(UPSTREAM_SNIPPET_MAX_CHARS).collect();
    if text.chars().count() > UPSTREAM_SNIPPET_MAX_CHARS {
        snippet.push_str("…[truncated]");
    }
    snippet
}

/// Append upstream OK outputs to a dependent task's prompt (eng-review D2:
/// Fresh-run persistence (T376 + W6.2b claim-at-create): a uuid row,
/// spec_json snapshot, owner claim from the first moment — a first run
/// is never ownerless while live.
/// Mint a fresh plan row for a `plan.execute` invocation (no resume id).
///
/// T6 (F6+OV-3): routes through the ATOMIC mint — claim-at-create in one
/// transaction (SC-017), no transient ownerless `running` window. The
/// previous create+claim pair discarded both results (`let _ =`), so a
/// failed create produced a phantom plan run: orphaned task rows, an
/// unpersisted "success", an impossible resume — all silent. Failures now
/// propagate as a structured, model-readable rejection (the surrounding
/// `plan claim failed` error shape), never a phantom run.
/// The plan-todos `spec_json` sensitivity tag (D24 §5) — mirrored here
/// because `plan_todos.rs` keeps its helper private and this file may not
/// grow a cross-module dependency for one match arm.
fn sensitivity_tag(sensitivity: &Sensitivity) -> &'static str {
    match sensitivity {
        Sensitivity::Public => "public",
        Sensitivity::Private => "private",
        Sensitivity::Confidential => "confidential",
    }
}

/// W12.15 (review N2): the sensitivity a plan row recorded at mint time.
/// Both producers use the lowercase tag shape (`plan.todos` D24 §5 and the
/// W12.17 direct-mint marker); the capitalized `Sensitivity::to_string()`
/// forms are accepted for forward compatibility. `None` = legacy row with
/// no recorded policy — the resuming session's policy stays in force
/// (byte-identical pre-W12.15 behavior).
fn plan_recorded_sensitivity(spec_json: &str) -> Option<Sensitivity> {
    let raw = serde_json::from_str::<serde_json::Value>(spec_json)
        .ok()?
        .get("sensitivity")?
        .as_str()?
        .to_lowercase();
    match raw.as_str() {
        "public" => Some(Sensitivity::Public),
        "private" => Some(Sensitivity::Private),
        "confidential" => Some(Sensitivity::Confidential),
        _ => None,
    }
}

async fn mint_fresh_plan(
    db: &Arc<zen_repo::SqliteClient>,
    args: &serde_json::Value,
    name: Option<&str>,
    session: &str,
    sensitivity: &Sensitivity,
) -> Result<String, serde_json::Value> {
    let plan_id = uuid::Uuid::new_v4().to_string();
    let repo = zen_repo::WorkflowRepo::new(db);
    // W12.17 (review Q4): the fresh-mint path writes the SAME session_id +
    // sensitivity marker `plan.todos` set writes (D24 §5 spec_json shape) —
    // without it the mint-once guard (`latest_plan_for_session`) is blind
    // to direct-mint plans and W12.15's resume policy restore has nothing
    // to read. The model's args are preserved verbatim as sibling keys.
    let mut spec_json = match args.as_object() {
        Some(obj) => obj.clone(),
        None => serde_json::Map::new(),
    };
    spec_json.insert("sensitivity".to_string(), json_sensitivity(sensitivity));
    spec_json.insert(
        "session_id".to_string(),
        serde_json::Value::String(session.to_string()),
    );
    let spec_json = serde_json::Value::Object(spec_json).to_string();
    repo.mint_plan_with_tasks(
        &plan_id,
        name,
        &spec_json,
        session,
        &[],
        chrono::Utc::now().timestamp(),
    )
    .await
    .map_err(|e| serde_json::json!({ "error": format!("plan mint failed: {e}") }))?;
    crate::plan_todos::register_turn_plan(session, &plan_id, Vec::new());
    Ok(plan_id)
}

fn json_sensitivity(sensitivity: &Sensitivity) -> serde_json::Value {
    serde_json::Value::String(sensitivity_tag(sensitivity).to_string())
}

/// `depends_on` previously ordered execution only — downstream tasks ran
/// their static prompts blind, so fan-in synthesis had no inputs).
fn compose_task_prompt(base: &str, upstream: &[(String, String)]) -> String {
    if upstream.is_empty() {
        return base.to_string();
    }
    let mut prompt = String::from(base);
    prompt.push_str(
        "\n\n## Upstream task outputs\nYour task depends on the tasks below; their outputs are your inputs:",
    );
    for (id, snippet) in upstream {
        prompt.push_str(&format!("\n\n### upstream task `{id}` output\n{snippet}"));
    }
    prompt
}

/// W2.2 (D12): persist a rewind memory for a failed plan at the
/// `vault/wiki/wisdom/rewinds/` boundary (reuses the reflection helper
/// shape). The first failed task's error is the falsifier; the next attempt
/// reads the guidance instead of repeating the failure. `None` when no
/// `ZenPaths` is available or the first failure carries no error.
fn record_plan_rewind(
    plan: &PlanSpec,
    outcomes: &[Option<TaskOutcome>],
) -> Option<std::path::PathBuf> {
    let paths = zen_core::paths::ZenPaths::detect().ok()?;
    let rewinds = paths.wiki().join("wisdom/rewinds");
    let (task_id, error) = plan.tasks.iter().enumerate().find_map(|(i, t)| {
        let outcome = outcomes[i].as_ref()?;
        (outcome.status == "failed").then(|| {
            let error = outcome
                .value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("task failed")
                .to_string();
            (t.id.clone(), error)
        })
    })?;
    let claim = format!(
        "plan `{}` task `{task_id}` failed",
        plan.name.as_deref().unwrap_or("<unnamed>")
    );
    zen_vault::distill::reflection::record_rewind(
        &rewinds,
        &claim,
        &error,
        "plan.execute task failure",
    )
    .ok()
}

/// SC-010 (D21 §4): a successful batch (≥2 tasks, ≥1 completed, none
/// failed) writes one `wisdom/plans/<slug>.md` success page — the twin of
/// the rewind path, which stays failure-only. Best-effort; a write failure
/// never affects the plan result.
fn record_plan_archive(
    plan: &PlanSpec,
    outcomes: &[Option<TaskOutcome>],
    plan_id: &str,
    plan_approved: bool,
    delivery_ready: bool,
    duration_ms: u128,
) -> Option<std::path::PathBuf> {
    let paths = zen_core::paths::ZenPaths::detect().ok()?;
    let plans_dir = paths.wiki().join("wisdom/plans");
    let tasks: Vec<zen_vault::distill::reflection::PlanArchiveTask<'_>> = plan
        .tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let outcome = outcomes[i].as_ref();
            let response = outcome
                .map(|o| {
                    o.value
                        .get("response")
                        .and_then(|v| v.as_str())
                        .or_else(|| o.value.get("error").and_then(|v| v.as_str()))
                        .unwrap_or("")
                })
                .unwrap_or("");
            zen_vault::distill::reflection::PlanArchiveTask {
                id: &t.id,
                agent: &t.agent,
                status: outcome.map_or("skipped", |o| o.status),
                response,
            }
        })
        .collect();
    let archive = zen_vault::distill::reflection::PlanArchive {
        plan_name: plan.name.as_deref(),
        plan_id,
        tasks: &tasks,
        plan_approved,
        delivery_ready,
        duration_ms,
    };
    zen_vault::distill::reflection::record_plan_archive(&plans_dir, &archive).ok()
}

#[async_trait::async_trait]
impl Tool for PlanExecuteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: PLAN_TOOL_NAME.to_string(),
            description: "Execute a multi-step workflow plan as a dependency DAG. \
                Draft the plan as tasks with stable ids; list upstream task ids in \
                `depends_on`. Independent tasks run in parallel; each task runs in a \
                dedicated sub-agent context with its own tool grants. A failed task \
                skips its downstream dependents. Each downstream task's prompt is \
                automatically extended with the outputs of its direct `depends_on` \
                prerequisites, so write dependent prompts as synthesis over those \
                outputs rather than repeating the upstream brief. Use this instead \
                of many manual delegate.task calls whenever the work has more than \
                one step or any step depends on another. Caps: 12 tasks, 3 \
                dependency layers."
                .to_string(),
            args_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Short plan label for audit logs"
                    },
                    "resume_plan_id": {
                        "type": "string",
                        "description": "D25 A6.1: resume a previously persisted plan by id — completed-checkpoints replay, pending tasks re-run; claim-fenced (one live resume per plan)"
                    },
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 12,
                        "description": "Ordered task nodes; order within a layer is irrelevant",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Stable task id" },
                                "agent": { "type": "string", "description": "Sub-agent name (same catalog as delegate.task)" },
                                "prompt": { "type": "string", "description": "Self-contained brief for the task" },
                                "depends_on": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Ids of tasks that must finish OK first"
                                },
                                "on_failure": {
                                    "type": "string",
                                    "enum": ["retry", "skip", "halt"],
                                    "description": "Failure transition: skip (default) skips dependents; retry re-queues this task once; halt stops the layer chain"
                                },
                                "artifact": {
                                    "type": "string",
                                    "description": "Optional sandbox-validated path this task's sub-agent should write its deliverable to (requires the agent's fs.write grant)"
                                },
                                "fork_mode": {
                                    "type": "string",
                                    "enum": ["none", "knowledge", "history"],
                                    "description": "Per-task context fork (SC-021): none (default) empty context; knowledge inherits the parent knowledge block; history adds the stripped parent conversation. Absent keeps the session-wide default"
                                },
                                "model": {
                                    "type": "string",
                                    "description": "Per-task model hint (SC-022): catalog-validated, ≤5 distinct per plan; resolution sensitivity > LlmPreference > hint > default (runtime override deferred)"
                                }
                            },
                            "required": ["id", "agent", "prompt"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
            result_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": { "type": ["string", "null"] },
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "agent": { "type": "string" },
                                "status": { "type": "string", "enum": ["completed", "failed", "skipped"] },
                                "response": { "type": "string" },
                                "error": { "type": "string" },
                                "artifact_path": { "type": "string" },
                                "summary": { "type": "string" }
                            }
                        }
                    },
                    "summary": { "type": "string" },
                    "hermes": {
                        "type": "object",
                        "properties": {
                            "plan_approved": { "type": "boolean" },
                            "delivery_ready": { "type": "boolean" }
                        }
                    }
                }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, KernelError> {
        let started = Instant::now();
        let plan = match Self::parse_plan(&args) {
            Ok(p) => p,
            Err(e) => return Ok(serde_json::json!({ "error": e })),
        };
        let layers = match Self::topological_layers(&plan) {
            Ok(l) => l,
            Err(e) => return Ok(serde_json::json!({ "error": e })),
        };
        let session = zen_plugin::task_registry::current_session_scope();
        // W6.3: lives to the end of invoke so every exit path (run
        // completion, ledger-item refusal, structured error) unmarks the
        // session's batch exactly once.
        let mut _batch_guard: Option<crate::plan_todos::BatchGuard> = None;

        // Persistence (T376): fresh runs get a uuid row; `resume_plan_id`
        // replays a running plan's ok-checkpoints and re-runs the rest.
        let resumed_id = args
            .get("resume_plan_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let mut resumed = false;
        let mut outcomes: Vec<Option<TaskOutcome>> = vec![None; plan.tasks.len()];
        // T218 (design F3-D3, REPORT-ONLY): staleness risk — a replayed
        // ok-checkpoint's recorded output is injected into downstream prompts
        // as a 4000-char snippet (`upstream_snippet`/`compose_task_prompt`)
        // REGARDLESS of its age, so it can be inconsistent with re-executed
        // sibling tasks. TTL invalidation was rejected: it would need an
        // invented staleness threshold. The age rides the completion audit
        // line instead, making staleness observable without gating on it.
        let mut replayed_ages: Vec<(String, i64)> = Vec::new();
        // W12.15 (review N2): the plan's RECORDED policy overrides the
        // resuming session's for every sub-turn (a Confidential plan
        // resumed by a Public session must never leak to cloud). Set from
        // spec_json on resume/ledger-attach; fresh mints carry the turn's
        // own policy in the marker (W12.17) — no override needed.
        let mut plan_policy: Option<Sensitivity> = None;
        let plan_id = match (self.db().await, resumed_id) {
            (Some(db), Some(rid)) => {
                let repo = zen_repo::WorkflowRepo::new(db);
                match repo.load_plan(&rid).await {
                    Ok(Some(row)) if row.status == "running" => {
                        // /review #6: exactly one resume may run a plan.
                        // The claim is a conditional UPDATE against the
                        // single writer — a second concurrent resume loses
                        // the race and refuses with a readable error.
                        let owner = uuid::Uuid::new_v4().to_string();
                        match repo
                            .claim_plan(
                                &rid,
                                &owner,
                                chrono::Utc::now().timestamp(),
                                zen_core::config::resolve_stale_claim_secs(),
                            )
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => {
                                return Ok(serde_json::json!({
                                    "error":
                                        format!("plan {rid} is already being resumed by another run")
                                }));
                            }
                            Err(e) => {
                                return Ok(serde_json::json!({
                                    "error": format!("plan claim failed: {e}")
                                }));
                            }
                        }
                        resumed = true;
                        plan_policy = plan_recorded_sensitivity(&row.spec_json);
                        crate::plan_todos::register_turn_plan(&session, &rid, Vec::new());
                        let resumed_now = chrono::Utc::now().timestamp();
                        for t in repo.load_task_rows(&rid).await.unwrap_or_default() {
                            if t.status != "completed" {
                                continue;
                            }
                            let Some(i) = plan.tasks.iter().position(|p| p.id == t.task_id) else {
                                continue;
                            };
                            // A checkpoint only replays when the re-drafted
                            // task still targets the same agent; otherwise
                            // the model changed the plan and the task reruns.
                            if plan.tasks[i].agent != t.agent {
                                continue;
                            }
                            // Age source is the existing `workflow_tasks`
                            // row timestamp — no schema change (T218).
                            replayed_ages
                                .push((t.task_id.clone(), (resumed_now - t.updated_at).max(0)));
                            outcomes[i] = Some(TaskOutcome {
                                status: "completed",
                                value: serde_json::json!({
                                    "response": t.response.unwrap_or_default()
                                }),
                            });
                        }
                        rid
                    }
                    Ok(Some(row)) => {
                        return Ok(serde_json::json!({
                            "error": format!("plan {rid} already terminal ({})", row.status)
                        }));
                    }
                    Ok(None) => {
                        return Ok(serde_json::json!({ "error": format!("unknown plan {rid}") }));
                    }
                    Err(e) => {
                        return Ok(
                            serde_json::json!({ "error": format!("plan store read failed: {e}") }),
                        );
                    }
                }
            }
            (Some(db), None) => {
                // W6.3 (D21 §3 amended): batches are rounds on the ONE plan
                // row `plan.todos` minted — no per-batch plan row. A live
                // session ledger record wins over a fresh mint: attach,
                // claim-fence, and write per-item status back through the
                // same WorkflowRepo rows (checkpoint_task below). The claim
                // doubles as the second-batch guard — a concurrent dispatch
                // on a non-terminal batch loses the fence and refuses; a
                // settled record releases the session for a fresh mint.
                let ledger = crate::plan_todos::peek_turn_plan(&session).map(|r| r.plan_id);
                let working = match ledger {
                    Some(lid) => {
                        let repo = zen_repo::WorkflowRepo::new(db);
                        let now = chrono::Utc::now().timestamp();
                        let claimed = repo
                            .claim_plan(
                                &lid,
                                &session,
                                now,
                                zen_core::config::resolve_stale_claim_secs(),
                            )
                            .await;
                        match claimed {
                            Ok(true) => Some(lid),
                            Ok(false) => match repo.load_plan(&lid).await {
                                Ok(Some(row)) if row.status != "running" => {
                                    crate::plan_todos::clear_turn_plan(&session, &lid);
                                    None
                                }
                                Ok(Some(_)) => {
                                    return Ok(serde_json::json!({
                                        "error": format!(
                                            "plan {lid} is in flight — a second batch is refused while one is non-terminal"
                                        )
                                    }));
                                }
                                Ok(None) => {
                                    crate::plan_todos::clear_turn_plan(&session, &lid);
                                    None
                                }
                                Err(e) => {
                                    return Ok(serde_json::json!({
                                        "error": format!("plan store read failed: {e}")
                                    }));
                                }
                            },
                            Err(e) => {
                                return Ok(serde_json::json!({
                                    "error": format!("plan claim failed: {e}")
                                }));
                            }
                        }
                    }
                    None => None,
                };
                match working {
                    // Ledger attach: the batch runs on the minted row.
                    // Item-id contract — a batch drafts tasks from the
                    // pending ledger items; an id outside the ledger would
                    // checkpoint an orphan row.
                    Some(lid) => {
                        // W6.3: the DB claim fence admits same-owner
                        // re-claims (crash recovery), so the second-batch
                        // refusal lives in the registry marker.
                        if !crate::plan_todos::try_begin_batch(&session, &lid) {
                            return Ok(serde_json::json!({
                                "error": format!(
                                    "plan {lid} already has a batch in flight — a second batch is refused while one is non-terminal"
                                )
                            }));
                        }
                        _batch_guard = Some(crate::plan_todos::BatchGuard::arm(&session));
                        let repo = zen_repo::WorkflowRepo::new(db);
                        let rows = repo.load_task_rows(&lid).await.unwrap_or_default();
                        // W12.15: the ledger row's recorded policy governs
                        // this batch's sub-turns.
                        let mut attach_spec: Option<String> = None;
                        if let Ok(Some(row)) = repo.load_plan(&lid).await {
                            plan_policy = plan_recorded_sensitivity(&row.spec_json);
                            attach_spec = Some(row.spec_json.clone());
                        }
                        if let Some(missing) = plan
                            .tasks
                            .iter()
                            .find(|t| !rows.iter().any(|r| r.task_id == t.id))
                        {
                            return Ok(serde_json::json!({
                                "error": format!(
                                    "task `{}` is not a ledger item of plan {lid} — draft batch tasks from the plan.todos ledger",
                                    missing.id
                                )
                            }));
                        }
                        // W14.1 (eng-review T5): the mint wrote only the titles-only items —
                        // merge this batch's DAG so the induction corpus can observe the
                        // ATTACHED shape. Additive-by-key: `items`, `session_id` (mint-once
                        // LIKE guard), `sensitivity` (W12.15 resume policy) are preserved
                        // verbatim; only `tasks` is replaced (last-dispatched shape). Best
                        // effort with a warn — observability must never fail the batch.
                        if let Some(existing) = attach_spec.take().as_deref()
                            && let Ok(mut spec) =
                                serde_json::from_str::<serde_json::Value>(existing)
                            && let Some(obj) = spec.as_object_mut()
                        {
                            obj.insert(
                                "tasks".to_string(),
                                serde_json::json!(plan.tasks.iter().map(|t| serde_json::json!({
                                    "id": t.id, "agent": t.agent, "prompt": t.prompt, "depends_on": t.depends_on,
                                })).collect::<Vec<_>>()),
                            );
                            if let Ok(merged) = serde_json::to_string(&spec)
                                && let Err(e) = repo
                                    .attach_batch_spec(
                                        &lid,
                                        &merged,
                                        chrono::Utc::now().timestamp(),
                                    )
                                    .await
                            {
                                tracing::warn!(plan = %lid, error = %e, "attach spec_json merge failed — induction corpus stays stale");
                            }
                        }
                        crate::plan_todos::register_turn_plan(&session, &lid, Vec::new());
                        lid
                    }
                    None => {
                        // W12.17: the fresh-mint marker records the TURN's
                        // sensitivity (TurnContext-first, shared-cell fallback).
                        match mint_fresh_plan(
                            db,
                            &args,
                            plan.name.as_deref(),
                            &session,
                            &self.delegate.current_sensitivity(),
                        )
                        .await
                        {
                            Ok(id) => id,
                            Err(error) => return Ok(error),
                        }
                    }
                }
            }
            (None, Some(_)) => {
                return Ok(serde_json::json!({
                    "error": "plan store unavailable; resume is impossible without state.db"
                }));
            }
            (None, None) => String::new(),
        };
        let mut retried = vec![false; plan.tasks.len()];
        let mut retry_count = 0usize;
        let mut halted = false;
        let mut cancelled_mid = false;
        let mut progress_lines: Vec<String> = Vec::new();
        // W12.18 (review Q5): checkpoint/complete_plan persistence failures
        // land here — surfaced as `persistence_warnings` in the result
        // payload, never silently discarded.
        let mut persistence_warnings: Vec<String> = Vec::new();
        // W9.1: one advisory forecast line before the first batch —
        // omitted entirely when no task's pair has recorded history.
        let forecast_sensitivity =
            plan_policy.unwrap_or_else(|| self.delegate.current_sensitivity());
        if let Some(line) = self.forecast_line(&plan, forecast_sensitivity).await {
            fire_plan_progress(line.clone());
            progress_lines.push(line);
        }
        // W9.3: live registration + cancel subscription. A pre-signal
        // (plan.control cancel racing ahead of the executor) joins the
        // same channel and is observed at the first batch boundary.
        let (_live_guard, mut cancel_rx) = if plan_id.is_empty() {
            (None, None)
        } else {
            let (guard, rx) = crate::plan_control::register_live_plan(&plan_id);
            (Some(guard), Some(rx))
        };
        'layers: for layer in &layers {
            // W9.3: between-batch cancellation check — a signal that
            // arrived while no batch was in flight still stops the walk.
            if let Some(rx) = cancel_rx.as_mut() {
                let cancel_now = *rx.borrow_and_update();
                if cancel_now {
                    self.mark_cancelled(
                        &plan_id,
                        &plan,
                        &mut outcomes,
                        &[],
                        &mut persistence_warnings,
                    )
                    .await;
                    cancelled_mid = true;
                    crate::plan_todos::clear_turn_plan(&session, &plan_id);
                    break 'layers;
                }
            }
            let mut halt_hit = false;
            // W8.1: the last task settled in THIS layer (newly scheduled —
            // checkpoint replays do not count) anchors the transition line.
            let mut last_settled: Option<usize> = None;
            let mut layer_failed: Vec<usize> = Vec::new();
            let mut runnable = Vec::new();
            for &i in layer {
                if outcomes[i].is_some() {
                    continue; // checkpoint replay: keep the recorded result
                }
                let mut upstream: Vec<(String, String)> = Vec::new();
                let mut blocked = false;
                for dep in &plan.tasks[i].depends_on {
                    let j = plan
                        .tasks
                        .iter()
                        .position(|t| &t.id == dep)
                        .expect("deps validated in parse_plan");
                    match outcomes[j].as_ref() {
                        Some(o) if o.status == "completed" => {
                            upstream.push((dep.clone(), upstream_snippet(&o.value)));
                        }
                        _ => blocked = true,
                    }
                }
                if blocked {
                    let outcome = TaskOutcome {
                        status: "skipped",
                        value: serde_json::json!({
                            "error": "upstream task did not complete OK"
                        }),
                    };
                    #[cfg(test)]
                    self.test_log_wave_event(&plan.tasks[i].agent, "skipped");
                    self.checkpoint_logged(
                        &plan_id,
                        &plan.tasks[i],
                        &outcome,
                        &mut persistence_warnings,
                    )
                    .await;
                    outcomes[i] = Some(outcome);
                    last_settled = Some(i);
                    continue;
                }
                let req = DelegateRequest {
                    agent: plan.tasks[i].agent.clone(),
                    prompt: compose_task_prompt(&plan.tasks[i].prompt, &upstream),
                    description: Some(format!("plan task `{}`", plan.tasks[i].id)),
                    independent: true,
                    artifact: plan.tasks[i].artifact.clone(),
                    // W11.5/T5 (SC-021/022): the plan task's own fork/model
                    // surface flows through the same forked compose + hint
                    // resolution as delegate.task (absent ⇒ session default).
                    fork_mode: plan.tasks[i].fork_mode,
                    model_hint: plan.tasks[i].model_hint.clone(),
                };
                if let Some(error) = self.delegate.validate_task(DELEGATE_ROOT_PARENT, 0, &req) {
                    if plan.tasks[i].on_failure == FailureAction::Halt {
                        halt_hit = true;
                    }
                    let outcome = TaskOutcome {
                        status: "failed",
                        value: serde_json::json!({ "error": error }),
                    };
                    self.checkpoint_logged(
                        &plan_id,
                        &plan.tasks[i],
                        &outcome,
                        &mut persistence_warnings,
                    )
                    .await;
                    outcomes[i] = Some(outcome);
                    last_settled = Some(i);
                    layer_failed.push(i);
                    continue;
                }
                runnable.push((i, req));
            }
            for batch in runnable.chunks(self.max_concurrent.max(1) as usize) {
                if !plan_id.is_empty() {
                    crate::plan_todos::update_turn_plan_unsettled(
                        &session,
                        &plan_id,
                        batch
                            .iter()
                            .map(|(i, _)| plan.tasks[*i].id.clone())
                            .collect(),
                    );
                }
                let futures: Vec<_> = batch
                    .iter()
                    .map(|(_, req)| {
                        let req = req.clone();
                        async move {
                            #[cfg(test)]
                            let _wave = tests::PlanWaveGuard::enter(self, req.agent.as_str());
                            #[cfg(test)]
                            if let Some(delay) = self.test_wave_delay(&req.agent) {
                                tokio::time::sleep(delay).await;
                            }
                            #[cfg(test)]
                            {
                                self.test_note_call(&req.agent);
                                if self.test_take_failure(&req.agent) {
                                    return TaskOutcome {
                                        status: "failed",
                                        value: serde_json::json!({
                                            "error": "injected test failure"
                                        }),
                                    };
                                }
                            }
                            let value = self.run_task_under_policy(&req, plan_policy).await;
                            let status = if value.get("error").is_some() {
                                "failed"
                            } else {
                                "completed"
                            };
                            TaskOutcome { status, value }
                        }
                    })
                    .collect();
                crate::plan_control::set_in_batch(&plan_id, true);
                let batch_indices: Vec<usize> = batch.iter().map(|(i, _)| *i).collect();
                // W9.3: race the batch against the cancel watch. The
                // race itself contains NO await in its arms — the watch
                // Ref wraps a non-Send read guard, so every await (the
                // cancel finalization) happens strictly after the race
                // expression ends and the guard is released.
                enum BatchRace {
                    Done(Vec<TaskOutcome>),
                    Cancelled,
                }
                let raced = if let Some(rx) = cancel_rx.as_mut() {
                    tokio::select! {
                        done = futures::future::join_all(futures) => {
                            BatchRace::Done(done)
                        }
                        _ = rx.wait_for(|v| *v) => BatchRace::Cancelled,
                    }
                } else {
                    BatchRace::Done(futures::future::join_all(futures).await)
                };
                crate::plan_control::set_in_batch(&plan_id, false);
                let results = match raced {
                    BatchRace::Done(results) => results,
                    // W9.3: cancel mid-batch — the in-flight sub-turns
                    // were dropped (never checkpointed as completed),
                    // then finalized per W6.2b: in-flight `failed`,
                    // pending `skipped`.
                    BatchRace::Cancelled => {
                        self.mark_cancelled(
                            &plan_id,
                            &plan,
                            &mut outcomes,
                            &batch_indices,
                            &mut persistence_warnings,
                        )
                        .await;
                        cancelled_mid = true;
                        crate::plan_todos::clear_turn_plan(&session, &plan_id);
                        break 'layers;
                    }
                };

                // W2.1 (D11): failure-transition edges. Default `skip` leaves
                // this block inert (byte-identical). `retry` re-queues ONLY the
                // failed node once (verified siblings keep their checkpoints —
                // minimal-subgraph repair); `halt` stops the layer chain.
                let mut retry_batch: Vec<(usize, DelegateRequest)> = Vec::new();
                for (&(i, ref req), outcome) in batch.iter().zip(&results) {
                    if outcome.status != "failed" {
                        continue;
                    }
                    match plan.tasks[i].on_failure {
                        FailureAction::Retry if !retried[i] => {
                            retried[i] = true;
                            retry_batch.push((i, req.clone()));
                        }
                        FailureAction::Halt => halt_hit = true,
                        _ => {}
                    }
                }

                for (&(i, _), outcome) in batch.iter().zip(&results) {
                    self.checkpoint_logged(
                        &plan_id,
                        &plan.tasks[i],
                        outcome,
                        &mut persistence_warnings,
                    )
                    .await;
                    // W9.5-F3: one watchdog heartbeat per settled task.
                    fire_watchdog_renewal();
                }
                if !plan_id.is_empty() {
                    crate::plan_todos::update_turn_plan_unsettled(&session, &plan_id, Vec::new());
                }
                for (&(i, _), outcome) in batch.iter().zip(results) {
                    outcomes[i] = Some(outcome);
                    last_settled = Some(i);
                    if outcomes[i].as_ref().is_some_and(|o| o.status == "failed") {
                        layer_failed.push(i);
                    }
                }

                for (i, req) in retry_batch {
                    if !plan_id.is_empty() {
                        crate::plan_todos::update_turn_plan_unsettled(
                            &session,
                            &plan_id,
                            vec![plan.tasks[i].id.clone()],
                        );
                    }
                    #[cfg(test)]
                    self.test_note_call(&req.agent);
                    let value = self.run_task_under_policy(&req, plan_policy).await;
                    let status = if value.get("error").is_some() {
                        "failed"
                    } else {
                        "completed"
                    };
                    let outcome = TaskOutcome { status, value };
                    self.checkpoint_logged(
                        &plan_id,
                        &plan.tasks[i],
                        &outcome,
                        &mut persistence_warnings,
                    )
                    .await;
                    outcomes[i] = Some(outcome);
                    retry_count += 1;
                    last_settled = Some(i);
                    if outcomes[i].as_ref().is_some_and(|o| o.status == "failed") {
                        layer_failed.push(i);
                    }
                    fire_watchdog_renewal();
                    if !plan_id.is_empty() {
                        crate::plan_todos::update_turn_plan_unsettled(
                            &session,
                            &plan_id,
                            Vec::new(),
                        );
                    }
                }
            }
            // W8.1: one progress line per wave transition, fired at the
            // settled count of the layer's last task; per-failed-task ✗
            // lines ride the same sink. `⚠` is reserved for veto markers —
            // never used here.
            if let Some(li) = last_settled {
                let done = outcomes.iter().filter(|o| o.is_some()).count();
                let total = plan.tasks.len();
                let task = &plan.tasks[li];
                let line = format!("⚙ plan {done}/{total} · {} {}", task.id, task.agent);
                fire_plan_progress(line.clone());
                progress_lines.push(line);
                for &fi in &layer_failed {
                    let ftask = &plan.tasks[fi];
                    let fline = format!("✗ plan {done}/{total} · {} failed", ftask.id);
                    fire_plan_progress(fline.clone());
                    progress_lines.push(fline);
                }
            }
            if halt_hit {
                halted = true;
                break;
            }
        }

        // Assemble per-task output + merged deliverable summary.
        let mut task_rows = Vec::with_capacity(plan.tasks.len());
        let mut summary_lines = Vec::new();
        let (mut ok, mut failed, mut skipped) = (0usize, 0usize, 0usize);
        for (i, t) in plan.tasks.iter().enumerate() {
            let outcome = outcomes[i].clone().unwrap_or(TaskOutcome {
                status: "skipped",
                value: serde_json::json!({ "error": "never scheduled" }),
            });
            match outcome.status {
                "completed" => ok += 1,
                "failed" => failed += 1,
                _ => skipped += 1,
            }
            let excerpt = outcome
                .value
                .get("response")
                .and_then(|v| v.as_str())
                .or_else(|| outcome.value.get("error").and_then(|v| v.as_str()))
                .unwrap_or("");
            let cut: String = excerpt.chars().take(300).collect();
            summary_lines.push(format!("[{}] {}/{}: {cut}", outcome.status, t.id, t.agent));
            let mut row = serde_json::json!({
                "id": t.id,
                "agent": t.agent,
                "status": outcome.status,
            });
            if let Some(resp) = outcome.value.get("response").cloned() {
                row["response"] = resp;
            }
            if let Some(err) = outcome.value.get("error").cloned() {
                row["error"] = err;
            }
            if let Some(artifact) = outcome.value.get("artifact_path").cloned() {
                row["artifact_path"] = artifact;
            }
            if let Some(summary) = outcome.value.get("summary").cloned() {
                row["summary"] = summary;
            }
            task_rows.push(row);
        }
        let summary = summary_lines.join("\n");

        // Completion signal: the merged deliverable goes through the same
        // quality gate as an orchestrator turn, classified under the SAME
        // session sensitivity the delegate tool mirrors. A veto does not
        // roll back completed work — it marks the plan not delivery-ready.
        // W12.15: the gate review classifies under the plan's recorded
        // policy when one is in force.
        let sensitivity = plan_policy.unwrap_or_else(|| self.delegate.current_sensitivity());
        let review_task = Self::review_task(&summary, sensitivity);
        let plan_str = serde_json::to_string(&args).unwrap_or_default();
        let gate = self
            .pipeline
            .execute(&review_task, &plan_str, |d: String| {
                Box::pin(async move { d })
            })
            .await;
        let hermes = serde_json::json!({
            "plan_approved": gate.plan_approved,
            "delivery_ready": gate.delivery_ready,
        });

        let final_status = if failed + skipped == 0 {
            "completed"
        } else {
            "failed"
        };
        if !plan_id.is_empty()
            && let Some(db) = self.db().await
        {
            let repo = zen_repo::WorkflowRepo::new(db);
            // W6.3 (D21 §3 amended): batches are rounds on the ONE ledger
            // row — the plan completes only when the LEDGER is fully
            // terminal, never when one batch's drafted subset settles. A
            // ledger with still-pending items keeps the row `running` (and
            // the session record alive) for the next batch; fresh and
            // resumed runs see all rows terminal here and behave exactly
            // as before.
            let ledger_pending = match crate::plan_todos::peek_turn_plan(&session) {
                Some(record) if record.plan_id == plan_id => repo
                    .load_task_rows(&plan_id)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|row| row.status == "pending" || row.status == "progressing"),
                _ => false,
            };
            if !ledger_pending && !cancelled_mid {
                // W12.16 (review Q1): cancel-vs-finalize tail window — the
                // watch re-check + SQL `status='running'` guard decide the
                // winner; a cancel landing here keeps the row terminal-
                // cancelled (no completion rewrite).
                let cancel_won = self
                    .complete_plan_checked(
                        &repo,
                        PlanCompletion {
                            plan_id: &plan_id,
                            final_status,
                            summary: &summary,
                            plan_approved: gate.plan_approved,
                            delivery_ready: gate.delivery_ready,
                        },
                        cancel_rx.as_ref(),
                        &mut persistence_warnings,
                    )
                    .await;
                if cancel_won {
                    cancelled_mid = true;
                }
                crate::plan_todos::clear_turn_plan(&session, &plan_id);
            }
        }
        let mut audit_entry = serde_json::json!({
            "kind": "loop.plan.completed",
            "plan": plan.name.as_deref().unwrap_or("<unnamed>"),
            "plan_id": if plan_id.is_empty() { None } else { Some(&plan_id) },
            "tasks_total": plan.tasks.len(),
            "tasks_ok": ok,
            "tasks_failed": failed,
            "tasks_skipped": skipped,
            "plan_approved": gate.plan_approved,
            "delivery_ready": gate.delivery_ready,
            "duration_ms": started.elapsed().as_millis() as u64,
        });
        // T218 additive field (resume runs with replays only): per replayed
        // task, the checkpoint age at resume — existing consumers keep
        // parsing (all pre-T218 fields unchanged).
        if resumed && !replayed_ages.is_empty() {
            audit_entry["replayed_checkpoints"] = replayed_ages
                .iter()
                .map(|(task_id, age)| {
                    serde_json::json!({ "task": task_id, "checkpoint_age_secs": age })
                })
                .collect::<Vec<_>>()
                .into();
        }
        // W2.1 additive fields — emitted ONLY for plans that use the new
        // failure edges, so a default (`skip`) plan's audit line stays
        // byte-identical.
        if retry_count > 0 {
            audit_entry["task_retries"] = serde_json::json!(retry_count);
        }
        if halted {
            audit_entry["halted"] = serde_json::json!(true);
        }
        if cancelled_mid {
            audit_entry["cancelled"] = serde_json::json!(true);
        }
        // W12.18 (review Q5): persistence failures are visible — one
        // additive audit key (zero lines when clean) and the same list in
        // the result payload below.
        if !persistence_warnings.is_empty() {
            audit_entry["persistence_warnings"] = serde_json::json!(persistence_warnings);
        }
        // W2.2 (D12): the plan-failure rollback boundary persists a rewind
        // memory so the next attempt does not repeat the failed path. Best-
        // effort; a write failure never affects the plan result.
        if failed > 0
            && !cancelled_mid
            && let Some(path) = record_plan_rewind(&plan, &outcomes)
        {
            audit_entry["rewind"] = serde_json::json!(path.display().to_string());
        }
        // SC-010: the success twin — mutually exclusive with the rewind
        // (any failed task ⇒ rewind only) and gated on a real batch
        // (≥2 tasks, ≥1 completed; a single-task turn writes neither).
        let mut archived_path: Option<String> = None;
        if failed == 0
            && plan.tasks.len() >= 2
            && ok >= 1
            && let Some(path) = record_plan_archive(
                &plan,
                &outcomes,
                &plan_id,
                gate.plan_approved,
                gate.delivery_ready,
                started.elapsed().as_millis(),
            )
        {
            archived_path = Some(path.display().to_string());
            audit_entry["archived"] = serde_json::json!(archived_path);
        }
        self.append_plan_audit(audit_entry);
        info!(
            plan = plan.name.as_deref().unwrap_or("<unnamed>"),
            ok, failed, skipped, "plan.execute completed"
        );

        let mut out = serde_json::json!({
            "plan": plan.name,
            "tasks": task_rows,
            "summary": summary,
            "hermes": hermes,
            "resumed": resumed,
        });
        if !progress_lines.is_empty() {
            out["progress"] = serde_json::json!(progress_lines);
        }
        if !persistence_warnings.is_empty() {
            out["persistence_warnings"] = serde_json::json!(persistence_warnings);
        }
        // W9.4: triage proposals for recognized failure classes only —
        // absent entirely when no class matched (raw errors stay visible
        // in the task rows). Never executes anything; the human picks.
        let failed_errors: Vec<&str> = outcomes
            .iter()
            .filter_map(|o| o.as_ref())
            .filter(|o| o.status == "failed")
            .filter_map(|o| o.value.get("error").and_then(|v| v.as_str()))
            .collect();
        let triage = triage_proposals(&plan_id, &failed_errors);
        if !triage.is_empty() {
            out["triage"] = serde_json::json!(triage);
        }
        if !plan_id.is_empty() {
            out["plan_id"] = serde_json::json!(plan_id);
        }
        if let Some(archived) = &archived_path {
            out["archived"] = serde_json::json!(archived);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    // D25 A6.1: resume_plan_id is read at invoke but must also be declared
    // in the args schema — an undeclared arg is invisible to model-driven
    // tool selection and drifts from the runtime contract.
    #[test]
    fn plan_execute_schema_declares_resume_plan_id() {
        let tool = plan_tool();
        let schema = tool.schema();
        let prop = schema.args_schema["properties"]["resume_plan_id"]
            .as_object()
            .expect("resume_plan_id declared in args schema");
        assert_eq!(prop["type"], "string", "{prop:?}");
        assert_eq!(
            schema.args_schema["required"],
            serde_json::json!(["tasks"]),
            "resume_plan_id stays optional: {prop:?}"
        );
    }

    use super::*;

    fn spec(id: &str, agent: &str, deps: &[&str]) -> PlanTaskSpec {
        PlanTaskSpec {
            id: id.to_string(),
            agent: agent.to_string(),
            prompt: format!("do {id}"),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            on_failure: FailureAction::Skip,
            artifact: None,
            fork_mode: None,
            model_hint: None,
        }
    }

    fn plan(tasks: Vec<PlanTaskSpec>) -> PlanSpec {
        PlanSpec {
            name: Some("test".into()),
            tasks,
        }
    }

    /// G-13 (SC-001/SC-002) wave-overlap guard: records `enter` and bumps
    /// the in-flight counter at construction, records `exit` and decrements
    /// on drop — i.e. on every settle path (ok / failed / early return),
    /// because the guard lives in the task-body scope. `pub(super)`: the
    /// cfg(test) call site in `execute` (parent module) names this type.
    pub(super) struct PlanWaveGuard<'a> {
        tool: &'a PlanExecuteTool,
        agent: String,
    }

    impl<'a> PlanWaveGuard<'a> {
        pub(super) fn enter(tool: &'a PlanExecuteTool, agent: &str) -> Self {
            tool.test_log_wave_event(agent, "enter");
            let now = tool
                .test_in_flight
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let _ = tool
                .test_peak_in_flight
                .fetch_max(now, std::sync::atomic::Ordering::SeqCst);
            Self {
                tool,
                agent: agent.to_string(),
            }
        }
    }

    impl std::ops::Drop for PlanWaveGuard<'_> {
        fn drop(&mut self) {
            let _ = self
                .tool
                .test_in_flight
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            self.tool.test_log_wave_event(&self.agent, "exit");
        }
    }

    // T278: the verified Kahn core mounted from formal/ (one physical
    // source, two consumers — verus verifies it there; here we pin the REAL
    // `topological_layers` to the same behavior on the fixtures below).
    #[path = "formal_plan_layers.rs"]
    mod formal_plan_layers;

    fn resolved_deps(plan: &PlanSpec) -> (Vec<Vec<usize>>, usize) {
        let n = plan.tasks.len();
        let index_of = |id: &str| plan.tasks.iter().position(|t| t.id == id);
        let deps: Vec<Vec<usize>> = plan
            .tasks
            .iter()
            .map(|t| {
                t.depends_on
                    .iter()
                    .map(|dep| index_of(dep).expect("resolved by parse"))
                    .collect()
            })
            .collect();
        (deps, n)
    }

    fn assert_congruent(plan: &PlanSpec) {
        let (deps, n) = resolved_deps(plan);
        let real = PlanExecuteTool::topological_layers(plan);
        let verified = formal_plan_layers::topo_layers_exec(&deps, n);
        match (real, verified) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "layer divergence on n={n}"),
            (Err(msg), Err(err)) => match err {
                formal_plan_layers::LayerError::Cycle => {
                    assert!(msg.contains("cycle"), "{msg}")
                }
                formal_plan_layers::LayerError::TooDeep => {
                    assert!(msg.contains("cap"), "{msg}")
                }
            },
            (Ok(a), Err(err)) => {
                panic!("real Ok / verified Err({err:?}) on n={n}: {a:?}")
            }
            (Err(msg), Ok(b)) => {
                panic!("real Err({msg}) / verified Ok on n={n}: {b:?}")
            }
        }
    }

    #[test]
    fn formal_consts_match_plan_caps() {
        assert_eq!(formal_plan_layers::PIN_MAX_PLAN_TASKS, MAX_PLAN_TASKS);
        assert_eq!(formal_plan_layers::PIN_MAX_PLAN_LAYERS, MAX_PLAN_LAYERS);
    }

    #[test]
    fn formal_congruent_empty_plan() {
        assert_congruent(&plan(vec![]));
    }

    #[test]
    fn formal_congruent_linear_chain() {
        assert_congruent(&plan(vec![
            spec("a", "Explore", &[]),
            spec("b", "Explore", &["a"]),
            spec("c", "Explore", &["b"]),
        ]));
    }

    #[test]
    fn formal_congruent_diamond() {
        assert_congruent(&plan(vec![
            spec("a", "Explore", &[]),
            spec("b", "Librarian", &["a"]),
            spec("c", "Hephaestus", &["a"]),
            spec("d", "Explore", &["b", "c"]),
        ]));
    }

    #[test]
    fn formal_congruent_duplicate_deps() {
        assert_congruent(&plan(vec![
            spec("a", "Explore", &[]),
            spec("b", "Explore", &["a", "a"]),
            spec("c", "Explore", &["b", "a"]),
        ]));
    }

    #[test]
    fn formal_congruent_cycle() {
        assert_congruent(&plan(vec![
            spec("a", "Explore", &["b"]),
            spec("b", "Explore", &["a"]),
        ]));
    }

    #[test]
    fn formal_congruent_over_deep() {
        assert_congruent(&plan(vec![
            spec("a", "Explore", &[]),
            spec("b", "Explore", &["a"]),
            spec("c", "Explore", &["b"]),
            spec("d", "Explore", &["c"]),
        ]));
    }

    #[test]
    fn topological_layers_linear_chain() {
        let p = plan(vec![
            spec("a", "Explore", &[]),
            spec("b", "Explore", &["a"]),
            spec("c", "Explore", &["b"]),
        ]);
        let layers = PlanExecuteTool::topological_layers(&p).unwrap();
        assert_eq!(layers, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn topological_layers_diamond() {
        let p = plan(vec![
            spec("a", "Explore", &[]),
            spec("b", "Librarian", &["a"]),
            spec("c", "Hephaestus", &["a"]),
            spec("d", "Explore", &["b", "c"]),
        ]);
        let layers = PlanExecuteTool::topological_layers(&p).unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec![0]);
        assert_eq!(layers[1], vec![1, 2]);
        assert_eq!(layers[2], vec![3]);
    }

    #[test]
    fn topological_layers_rejects_cycle() {
        let p = plan(vec![
            spec("a", "Explore", &["b"]),
            spec("b", "Explore", &["a"]),
        ]);
        let err = PlanExecuteTool::topological_layers(&p).unwrap_err();
        assert!(err.contains("cycle"), "{err}");
    }

    #[test]
    fn parse_plan_rejects_unknown_dependency() {
        let args = serde_json::json!({
            "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x", "depends_on": ["ghost"] }]
        });
        let err = PlanExecuteTool::parse_plan(&args).unwrap_err();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn parse_plan_rejects_duplicate_ids_and_oversize() {
        let dup = serde_json::json!({
            "tasks": [
                { "id": "a", "agent": "Explore", "prompt": "x" },
                { "id": "a", "agent": "Explore", "prompt": "y" }
            ]
        });
        assert!(
            PlanExecuteTool::parse_plan(&dup)
                .unwrap_err()
                .contains("duplicate")
        );

        let mut tasks = Vec::new();
        for i in 0..13 {
            tasks.push(
                serde_json::json!({ "id": format!("t{i}"), "agent": "Explore", "prompt": "x" }),
            );
        }
        let big = serde_json::json!({ "tasks": tasks });
        assert!(
            PlanExecuteTool::parse_plan(&big)
                .unwrap_err()
                .contains("cap")
        );
    }

    #[test]
    fn topological_layers_rejects_too_deep() {
        let p = plan(vec![
            spec("a", "Explore", &[]),
            spec("b", "Explore", &["a"]),
            spec("c", "Explore", &["b"]),
            spec("d", "Explore", &["c"]),
        ]);
        let err = PlanExecuteTool::topological_layers(&p).unwrap_err();
        assert!(err.contains("layers"), "{err}");
    }

    fn plan_tool() -> PlanExecuteTool {
        plan_tool_with(None)
    }

    fn plan_tool_with(state_db: Option<std::path::PathBuf>) -> PlanExecuteTool {
        plan_tool_concurrency(4, state_db)
    }

    fn plan_tool_concurrency(
        max_concurrent: u32,
        state_db: Option<std::path::PathBuf>,
    ) -> PlanExecuteTool {
        use std::sync::Mutex;
        use std::time::Duration;
        let delegate = crate::delegate_task::DelegateTaskTool::new(
            std::sync::Arc::new(crate::wiring::ZenWiring::new()),
            zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
                default_provider: Some("mock".to_string()),
                ..Default::default()
            }),
            Vec::new(),
            None,
            std::sync::Arc::new(Mutex::new(Sensitivity::Public)),
            std::sync::Arc::new(Mutex::new(Vec::new())),
            std::sync::Arc::new(Mutex::new(Vec::new())),
            std::sync::Arc::new(rig_compose::budget::AtomicTokenBudget::new(100_000)),
            Duration::from_secs(30),
            1,
            4,
        );
        PlanExecuteTool::new(
            std::sync::Arc::new(delegate),
            QualityPipeline::new(),
            max_concurrent,
            state_db,
        )
    }

    #[tokio::test]
    async fn plan_execute_runs_diamond_and_reports_hermes() {
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "name": "diamond",
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "sweep docs" },
                    { "id": "b", "agent": "Librarian", "prompt": "organize", "depends_on": ["a"] },
                    { "id": "c", "agent": "Explore", "prompt": "verify", "depends_on": ["a"] },
                    { "id": "d", "agent": "Hephaestus", "prompt": "synthesize", "depends_on": ["b", "c"] }
                ]
            }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "{out}");
        let rows = out["tasks"].as_array().expect("task rows");
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| r["status"] == "completed"), "{out}");
        assert!(out["hermes"]["plan_approved"].is_boolean(), "{out}");
        assert!(out["hermes"]["delivery_ready"].is_boolean(), "{out}");
        assert!(
            out["summary"].as_str().unwrap().contains("[completed] d"),
            "{out}"
        );
    }

    // W8.1: scoped sink receives exactly one ⚙ line per wave transition,
    // DURING the tool run (wave-1 line observable while invoke is still
    // executing), and no line ever opens with the reserved `⚠` marker.
    #[tokio::test]
    async fn plan_progress_one_line_per_wave_mid_turn_never_warning() {
        use std::sync::Mutex;
        let tool = plan_tool();
        tool.inject_test_wave_delay("Hephaestus", 250);
        let collected: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_collector = Arc::clone(&collected);
        let sink: PlanProgressSink = Arc::new(move |line: String| {
            sink_collector.lock().unwrap().push(line);
        });
        let invoke_tool = Arc::new(tool);
        let run_tool = Arc::clone(&invoke_tool);
        let handle = tokio::spawn(super::PLAN_PROGRESS.scope(
            std::cell::RefCell::new(Some(sink)),
            async move {
                run_tool
                    .invoke(serde_json::json!({
                        "tasks": [
                            { "id": "a", "agent": "Explore", "prompt": "p1" },
                            { "id": "b", "agent": "Explore", "prompt": "p2" },
                            { "id": "c", "agent": "Explore", "prompt": "p3" },
                            { "id": "d", "agent": "Explore", "prompt": "p4", "depends_on": ["a", "b"] },
                            { "id": "e", "agent": "Hephaestus", "prompt": "p5", "depends_on": ["c"] }
                        ]
                    }))
                    .await
            },
        ));
        // Wave-1 (a,b,c → 3/5) must land while the turn is still running.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let wave1_seen = loop {
            if handle.is_finished() {
                panic!("invoke finished before wave-1 progress line was observed");
            }
            if std::time::Instant::now() > deadline {
                panic!("wave-1 progress line never arrived");
            }
            if collected
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("⚙ plan 3/5 · c Explore"))
            {
                break true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        assert!(wave1_seen);
        let out = handle.await.unwrap().unwrap();
        let gears: Vec<String> = collected
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("⚙ "))
            .cloned()
            .collect();
        assert_eq!(
            gears.len(),
            2,
            "exactly one line per wave transition: {gears:?}"
        );
        assert_eq!(
            collected.lock().unwrap().last().map(String::as_str),
            Some("⚙ plan 5/5 · e Hephaestus"),
            "wave-2 transition is the final line: {:?}",
            collected.lock().unwrap()
        );
        assert!(
            collected
                .lock()
                .unwrap()
                .iter()
                .all(|l| !l.starts_with('⚠')),
            "⚠ is reserved for veto markers: {:?}",
            collected.lock().unwrap()
        );
        assert!(out["error"].is_null(), "{out}");
    }

    // W8.1: a failed task emits a `✗ ... failed` line mid-turn (before the
    // turn ends) and never a `⚠` line.
    #[tokio::test]
    async fn plan_progress_failure_line_mid_turn() {
        use std::sync::Mutex;
        let tool = plan_tool();
        tool.inject_test_failure("Atlas", 1);
        tool.inject_test_wave_delay("Explore", 150);
        let collected: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_collector = Arc::clone(&collected);
        let sink: PlanProgressSink = Arc::new(move |line: String| {
            sink_collector.lock().unwrap().push(line);
        });
        let out = super::PLAN_PROGRESS
            .scope(std::cell::RefCell::new(Some(sink)), async {
                tool.invoke(serde_json::json!({
                    "tasks": [
                        { "id": "a", "agent": "Atlas", "prompt": "boom" },
                        { "id": "b", "agent": "Explore", "prompt": "slow" },
                        { "id": "c", "agent": "Explore", "prompt": "next", "depends_on": ["b"] }
                    ]
                }))
                .await
                .unwrap()
            })
            .await;
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "failed", "{out}");
        assert!(
            collected
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("✗ plan ") && l.contains("· a failed")),
            "failure line present: {:?}",
            collected.lock().unwrap()
        );
        assert!(
            collected
                .lock()
                .unwrap()
                .iter()
                .all(|l| !l.starts_with('⚠')),
            "⚠ never used: {:?}",
            collected.lock().unwrap()
        );
    }

    // W8.1: the non-stream blob — the result JSON `progress` array equals
    // the sink rows exactly (one transport, same rows).
    #[tokio::test]
    async fn plan_progress_blob_in_result_equals_sink_rows() {
        use std::sync::Mutex;
        let tool = plan_tool();
        let collected: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_collector = Arc::clone(&collected);
        let sink: PlanProgressSink = Arc::new(move |line: String| {
            sink_collector.lock().unwrap().push(line);
        });
        let out = super::PLAN_PROGRESS
            .scope(std::cell::RefCell::new(Some(sink)), async {
                tool.invoke(serde_json::json!({
                    "tasks": [
                        { "id": "a", "agent": "Explore", "prompt": "p1" },
                        { "id": "b", "agent": "Explore", "prompt": "p2", "depends_on": ["a"] }
                    ]
                }))
                .await
                .unwrap()
            })
            .await;
        let blob = out["progress"].as_array().expect("progress blob");
        let sink_rows = collected.lock().unwrap().clone();
        assert_eq!(
            blob.iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            sink_rows,
            "result blob == sink rows: {blob:?} vs {sink_rows:?}"
        );
    }

    // W8.1 byte-identity: no scoped sink = fire_plan_progress is a silent
    // no-op (non-stream turns and payload-dropping clients see nothing new
    // except the result blob).
    #[tokio::test]
    async fn fire_plan_progress_is_noop_without_scope() {
        fire_plan_progress("⚙ plan 1/1 · a Explore".to_string());
    }

    // SC-010 acceptance: a successful ≥2-task plan writes exactly one
    // wisdom/plans page and reports the path in the audit line.
    #[tokio::test]
    async fn archive_page_written_with_path_in_audit() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        let tool = plan_tool_with(Some(state_db));
        let out = tool
            .invoke(serde_json::json!({
                "name": "archive-me",
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "sweep" },
                    { "id": "b", "agent": "Explore", "prompt": "verify", "depends_on": ["a"] }
                ]
            }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "{out}");
        let archived = out["archived"].as_str().expect("archived path in result");
        assert!(std::path::Path::new(archived).is_file(), "{archived}");
        let plans_dir = dir.path().join("vault/wiki/wisdom/plans");
        let pages: Vec<_> = std::fs::read_dir(&plans_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(pages.len(), 1, "exactly one page: {archived}");
        let body = std::fs::read_to_string(archived).unwrap();
        assert!(body.contains("type: plan"), "{body}");
        assert!(body.contains("plan_id: "), "{body}");
        let delivery_ready = out["hermes"]["delivery_ready"].as_bool().unwrap();
        assert!(
            body.contains(&format!("delivery_ready: {delivery_ready}")),
            "frontmatter echoes the gate verdict: {body}"
        );

        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl"))
            .expect("audit file under ZEN_HOME");
        let completed = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.plan.completed")
            .expect("completion audit line");
        assert_eq!(
            completed["archived"].as_str(),
            Some(archived),
            "{completed}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // SC-010 exclusivity: a failed task keeps the rewind path — the result
    // carries no archived path and the audit line names the rewind only.
    #[tokio::test]
    async fn failed_plan_writes_rewind_not_archive() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let tool = plan_tool_with(Some(dir.path().join("state.db")));
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "a", "agent": "NoSuchAgent", "prompt": "boom" },
                    { "id": "b", "agent": "Explore", "prompt": "after" }
                ]
            }))
            .await
            .unwrap();
        assert!(out["archived"].is_null(), "no archive on failure: {out}");
        let plans_dir = dir.path().join("vault/wiki/wisdom/plans");
        assert!(
            !plans_dir.exists() || std::fs::read_dir(&plans_dir).unwrap().next().is_none(),
            "no page written: {plans_dir:?}"
        );
        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl"))
            .expect("audit file under ZEN_HOME");
        let completed = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.plan.completed")
            .expect("completion audit line");
        assert!(
            completed["rewind"].as_str().is_some(),
            "failure rewinds: {completed}"
        );
        assert!(completed["archived"].is_null(), "{completed}");
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // SC-010 exclusivity: a single-task turn writes neither archive nor
    // rewind even when it succeeds.
    #[tokio::test]
    async fn single_task_plan_writes_neither() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let tool = plan_tool_with(Some(dir.path().join("state.db")));
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [{ "id": "a", "agent": "Explore", "prompt": "solo" }]
            }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "{out}");
        assert!(out["archived"].is_null(), "{out}");
        let plans_dir = dir.path().join("vault/wiki/wisdom/plans");
        assert!(
            !plans_dir.exists() || std::fs::read_dir(&plans_dir).unwrap().next().is_none(),
            "no page: {plans_dir:?}"
        );
        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl"))
            .expect("audit file under ZEN_HOME");
        let completed = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.plan.completed")
            .expect("completion audit line");
        assert!(
            completed["rewind"].is_null() && completed["archived"].is_null(),
            "{completed}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W6.3 acceptance (D21 §3 amended): two batches run sequentially on
    // the ONE plan.todos-minted row — batch 1 settles with a ledger
    // mutation and the row still `running`; batch 2 attaches to the same
    // row and completes the plan. No per-batch row is minted.
    #[tokio::test]
    async fn ledger_two_batches_share_one_plan_row() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let todos = crate::plan_todos::PlanTodosTool::new(
            Some(state_db.clone()),
            Arc::new(std::sync::Mutex::new(zen_core::types::Sensitivity::Private)),
        );
        let minted = todos
            .invoke(serde_json::json!({"op": "set", "items": [
                {"id": "a", "title": "first", "status": "pending"},
                {"id": "b", "title": "second", "status": "pending"},
            ]}))
            .await
            .unwrap();
        let plan_id = minted["plan_id"].as_str().unwrap().to_string();

        let plan = plan_tool_with(Some(state_db.clone()));
        let out1 = plan
            .invoke(serde_json::json!({
                "name": "two-batch",
                "tasks": [{"id": "a", "agent": "Explore", "prompt": "p1"}]
            }))
            .await
            .unwrap();
        assert!(out1["error"].is_null(), "batch 1: {out1}");

        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        let rows = repo.load_task_rows(&plan_id).await.unwrap();
        let a = rows.iter().find(|r| r.task_id == "a").unwrap();
        assert_eq!(
            a.status,
            "completed",
            "ledger mutation after batch 1: statuses {:?}",
            rows.iter()
                .map(|r| (&r.task_id, r.status.as_str()))
                .collect::<Vec<_>>()
        );
        let b = rows.iter().find(|r| r.task_id == "b").unwrap();
        assert_eq!(
            b.status,
            "pending",
            "batch 2's item untouched: statuses {:?}",
            rows.iter()
                .map(|r| (&r.task_id, r.status.as_str()))
                .collect::<Vec<_>>()
        );
        let plan_row = repo.load_plan(&plan_id).await.unwrap().unwrap();
        assert_eq!(
            plan_row.status, "running",
            "batch 1 must NOT complete the ledger plan"
        );

        let out2 = plan
            .invoke(serde_json::json!({
                "name": "two-batch",
                "tasks": [{"id": "b", "agent": "Explore", "prompt": "p2"}]
            }))
            .await
            .unwrap();
        assert!(out2["error"].is_null(), "batch 2: {out2}");

        let plan_row = repo.load_plan(&plan_id).await.unwrap().unwrap();
        assert_eq!(plan_row.status, "completed", "batch 2 completes the plan");
        let rows = repo.load_task_rows(&plan_id).await.unwrap();
        assert!(
            rows.iter().all(|r| r.status == "completed"),
            "statuses {:?}",
            rows.iter()
                .map(|r| (&r.task_id, r.status.as_str()))
                .collect::<Vec<_>>()
        );
    }

    // W6.3: a second batch is refused while one is non-terminal. The DB
    // claim fence admits same-owner re-claims (crash recovery), so the
    // refusal lives in the registry's in-flight marker; the two-batch
    // acceptance pin above proves the real begin/end bracket releases it.
    #[tokio::test]
    async fn ledger_second_batch_refused_while_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let todos = crate::plan_todos::PlanTodosTool::new(
            Some(state_db.clone()),
            Arc::new(std::sync::Mutex::new(zen_core::types::Sensitivity::Private)),
        );
        let minted = todos
            .invoke(serde_json::json!({"op": "set", "items": [
                {"id": "a", "title": "first", "status": "pending"},
            ]}))
            .await
            .unwrap();
        let plan_id = minted["plan_id"].as_str().unwrap().to_string();

        assert!(
            crate::plan_todos::try_begin_batch("local", &plan_id),
            "first batch admits"
        );
        let plan = plan_tool_with(Some(state_db.clone()));
        let out = plan
            .invoke(serde_json::json!({
                "name": "collide",
                "tasks": [{"id": "a", "agent": "Explore", "prompt": "p"}]
            }))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .unwrap()
                .contains("already has a batch in flight"),
            "{out}"
        );
        crate::plan_todos::end_batch("local");
        let out = plan
            .invoke(serde_json::json!({
                "name": "collide",
                "tasks": [{"id": "a", "agent": "Explore", "prompt": "p"}]
            }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "marker released: {out}");
    }

    #[tokio::test]
    async fn plan_execute_persists_and_resumes_from_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let tool = plan_tool_with(Some(state_db.clone()));

        let out = tool
            .invoke(serde_json::json!({
                "name": "persist",
                "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x" }]
            }))
            .await
            .unwrap();
        let plan_id = out["plan_id"]
            .as_str()
            .expect("plan_id returned")
            .to_string();

        // Terminal plans refuse to resume.
        let again = tool
            .invoke(serde_json::json!({
                "resume_plan_id": plan_id,
                "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x" }]
            }))
            .await
            .unwrap();
        assert!(
            again["error"].as_str().unwrap().contains("terminal"),
            "{again}"
        );

        // A plan stuck in `running` with an ok checkpoint replays it.
        let repo_plan_id = "stuck-plan".to_string();
        let client = zen_repo::SqliteClient::open_lazy(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan(&repo_plan_id, None, "{}", 1_000)
            .await
            .unwrap();
        repo.checkpoint_task(zen_repo::TaskCheckpoint {
            plan_id: &repo_plan_id,
            task_id: "a",
            agent: "Explore",
            status: "completed",
            response: Some("REPLAYED"),
            error: None,
            now: 1_001,
            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();

        let resumed = tool
            .invoke(serde_json::json!({
                "resume_plan_id": repo_plan_id,
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "already done" },
                    { "id": "b", "agent": "Librarian", "prompt": "fresh work" }
                ]
            }))
            .await
            .unwrap();
        assert_eq!(resumed["resumed"], serde_json::json!(true), "{resumed}");
        let rows = resumed["tasks"].as_array().unwrap();
        assert_eq!(
            rows[0]["response"].as_str(),
            Some("REPLAYED"),
            "checkpoint replayed, not re-run"
        );
        assert_eq!(
            rows[1]["status"], "completed",
            "pending task ran fresh: {resumed}"
        );

        let row = repo.load_plan(&repo_plan_id).await.unwrap().unwrap();
        assert_eq!(row.status, "completed", "resumed plan closed out");
    }

    #[tokio::test]
    async fn plan_execute_skips_downstream_of_failed_task() {
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "a", "agent": "NoSuchAgent", "prompt": "boom" },
                    { "id": "b", "agent": "Explore", "prompt": "cleanup", "depends_on": ["a"] }
                ]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().expect("task rows");
        assert_eq!(rows[0]["status"], "failed", "{out}");
        assert_eq!(rows[1]["status"], "skipped", "{out}");
        assert!(
            rows[1]["error"].as_str().unwrap().contains("upstream"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn plan_task_rejects_orchestrator_agent_via_tier_matrix() {
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [{ "id": "x", "agent": "Sisyphus", "prompt": "self-spawn" }]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().expect("task rows");
        assert_eq!(
            rows[0]["status"], "failed",
            "Sisyphus must not be a plan task"
        );
        assert!(
            rows[0]["error"]
                .as_str()
                .unwrap()
                .contains("spawn hierarchy"),
            "tier matrix must gate plan tasks exactly like delegate.task: {out}"
        );
    }

    #[tokio::test]
    async fn plan_task_rejects_unbounded_prompt() {
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [{
                    "id": "big",
                    "agent": "Explore",
                    "prompt": "y".repeat(crate::delegate_task::BOUNDED_PROMPT_CHARS + 1)
                }]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().expect("task rows");
        assert_eq!(rows[0]["status"], "failed", "{out}");
        assert!(
            rows[0]["error"].as_str().unwrap().contains("unbounded"),
            "the bounded 32k hard gate must apply to plan tasks: {out}"
        );
    }

    #[test]
    fn review_task_carries_sensitivity_metadata() {
        let task = PlanExecuteTool::review_task("summary", Sensitivity::Confidential);
        assert_eq!(
            task.metadata.get("sensitivity").map(|v| v.as_str()),
            Some("Confidential")
        );
        let public = PlanExecuteTool::review_task("s", Sensitivity::Public);
        assert_eq!(
            public.metadata.get("sensitivity").map(|v| v.as_str()),
            Some("Public")
        );
    }

    #[tokio::test]
    async fn replay_skips_checkpoint_when_agent_changed() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let tool = plan_tool_with(Some(state_db.clone()));

        let plan_id = "agent-drift".to_string();
        let client = zen_repo::SqliteClient::open_lazy(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan(&plan_id, None, "{}", 1_000).await.unwrap();
        repo.checkpoint_task(zen_repo::TaskCheckpoint {
            plan_id: &plan_id,
            task_id: "a",
            agent: "Explore",
            status: "completed",
            response: Some("STALE"),
            error: None,
            now: 1_001,
            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();

        let out = tool
            .invoke(serde_json::json!({
                "resume_plan_id": plan_id,
                "tasks": [{ "id": "a", "agent": "Librarian", "prompt": "same id, new agent" }]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(
            rows[0]["status"], "completed",
            "agent-changed task reruns: {out}"
        );
        assert_ne!(
            rows[0]["response"].as_str(),
            Some("STALE"),
            "stale checkpoint must NOT replay across an agent change: {out}"
        );
    }
    #[test]
    fn parse_plan_rejects_missing_empty_and_malformed_tasks() {
        let missing = serde_json::json!({ "name": "no tasks key" });
        assert!(PlanExecuteTool::parse_plan(&missing).is_err());

        let empty = serde_json::json!({ "tasks": [] });
        assert!(PlanExecuteTool::parse_plan(&empty).is_err());

        let bad_id =
            serde_json::json!({ "tasks": [{ "id": 1, "agent": "Explore", "prompt": "x" }] });
        assert!(PlanExecuteTool::parse_plan(&bad_id).is_err());

        let bad_agent = serde_json::json!({ "tasks": [{ "id": "a", "agent": 2, "prompt": "x" }] });
        assert!(PlanExecuteTool::parse_plan(&bad_agent).is_err());

        let bad_prompt =
            serde_json::json!({ "tasks": [{ "id": "a", "agent": "Explore", "prompt": true }] });
        assert!(PlanExecuteTool::parse_plan(&bad_prompt).is_err());

        let bad_dep = serde_json::json!({
            "tasks": [{
                "id": "a", "agent": "Explore", "prompt": "x",
                "depends_on": [7]
            }]
        });
        assert!(PlanExecuteTool::parse_plan(&bad_dep).is_err());
    }

    #[tokio::test]
    async fn resume_rejects_unknown_plan_and_missing_store() {
        // Without state.db, resume is impossible.
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "resume_plan_id": "any",
                "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x" }]
            }))
            .await
            .unwrap();
        assert!(
            out["error"].as_str().unwrap().contains("unavailable"),
            "{out}"
        );

        // With a store, an unknown plan id refuses.
        let dir = tempfile::tempdir().unwrap();
        let tool = plan_tool_with(Some(dir.path().join("state.db")));
        let out = tool
            .invoke(serde_json::json!({
                "resume_plan_id": "ghost",
                "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x" }]
            }))
            .await
            .unwrap();
        assert!(out["error"].as_str().unwrap().contains("unknown"), "{out}");
    }

    #[tokio::test]
    async fn resume_reruns_failed_and_skipped_and_revives_dependents() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let tool = plan_tool_with(Some(state_db.clone()));

        let plan_id = "mixed-checkpoints".to_string();
        let client = zen_repo::SqliteClient::open_lazy(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan(&plan_id, None, "{}", 1_000).await.unwrap();
        let cp = |task: &'static str,
                  status: &'static str,
                  resp: Option<&'static str>,
                  error: Option<&'static str>,
                  now: i64| zen_repo::TaskCheckpoint {
            plan_id: plan_id.as_str(),
            task_id: task,
            agent: "Explore",
            status,
            response: resp,
            error,
            now,
            model: None,
            duration_ms: None,
            cost_usd: None,
        };
        repo.checkpoint_task(cp("a", "completed", Some("REPLAYED"), None, 1_001))
            .await
            .unwrap();
        repo.checkpoint_task(cp("b", "failed", None, Some("boom"), 1_002))
            .await
            .unwrap();
        repo.checkpoint_task(cp("c", "skipped", None, Some("upstream"), 1_003))
            .await
            .unwrap();

        let out = tool
            .invoke(serde_json::json!({
                "resume_plan_id": plan_id,
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "done already" },
                    { "id": "b", "agent": "Explore", "prompt": "retry me" },
                    { "id": "c", "agent": "Librarian", "prompt": "was skipped" },
                    { "id": "d", "agent": "Explore", "prompt": "needs b", "depends_on": ["b"] }
                ]
            }))
            .await
            .unwrap();
        assert_eq!(out["resumed"], serde_json::json!(true), "{out}");
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "completed", "{out}");
        assert_eq!(
            rows[0]["response"].as_str(),
            Some("REPLAYED"),
            "ok checkpoint still replays: {out}"
        );
        assert_eq!(rows[1]["status"], "completed", "failed task reruns: {out}");
        assert_ne!(
            rows[1]["response"].as_str(),
            Some("boom"),
            "failed checkpoint must not replay: {out}"
        );
        assert_eq!(rows[2]["status"], "completed", "skipped task reruns: {out}");
        assert_eq!(
            rows[3]["status"], "completed",
            "dependent of a rerun-ok task now runs: {out}"
        );
    }

    #[tokio::test]
    async fn plan_chunks_wide_batches_by_max_concurrent() {
        let tool = plan_tool_concurrency(2, None);
        let agents = [
            "Explore",
            "Librarian",
            "Explore",
            "Librarian",
            "Explore",
            "Librarian",
        ];
        let tasks: Vec<serde_json::Value> = agents
            .iter()
            .enumerate()
            .map(|(i, agent)| {
                serde_json::json!({ "id": format!("t{i}"), "agent": agent, "prompt": format!("task {i}") })
            })
            .collect();
        let out = tool
            .invoke(serde_json::json!({ "name": "wide", "tasks": tasks }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "{out}");
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows.len(), 6, "every chunked task still runs: {out}");
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row["id"], serde_json::json!(format!("t{i}")), "{out}");
            assert_eq!(row["status"], "completed", "slot {i} across chunks: {out}");
        }
    }

    /// G-13 (SC-001/SC-002): sibling of
    /// `plan_chunks_wide_batches_by_max_concurrent` asserting the full wave
    /// contract. 5-task DAG {t1,t2} → {t3,t4} → {t5} at width 2, with t2
    /// failing (injected) so its dependent t4 is skipped. SC-001: peak
    /// in-flight counted from enter/exit events (never elapsed time) is
    /// ≥2 and ≤ max_concurrent. SC-002: every layer-(k+1) task enters only
    /// after ALL layer-k tasks settled — including the failed t2 and the
    /// skipped t4.
    #[tokio::test]
    async fn plan_waves_barriers_and_peak_in_flight_sc001_sc002() {
        let tool = plan_tool_concurrency(2, None);
        for (agent, millis) in [("Explore", 50), ("Librarian", 50), ("Junior", 50)] {
            tool.inject_test_wave_delay(agent, millis);
        }
        tool.inject_test_failure("Atlas", 1);

        let args = serde_json::json!({
            "tasks": [
                { "id": "t1", "agent": "Explore", "prompt": "x" },
                { "id": "t2", "agent": "Atlas", "prompt": "x" },
                { "id": "t3", "agent": "Librarian", "prompt": "x", "depends_on": ["t1"] },
                { "id": "t4", "agent": "Hephaestus", "prompt": "x", "depends_on": ["t2"] },
                { "id": "t5", "agent": "Junior", "prompt": "x", "depends_on": ["t3"] }
            ]
        });
        let out = tool.invoke(args.clone()).await.unwrap();
        assert!(out["error"].is_null(), "{out}");
        let rows = out["tasks"].as_array().unwrap();
        let status_of = |id: &str| {
            rows.iter()
                .find(|r| r["id"] == serde_json::json!(id))
                .expect("task row")["status"]
                .clone()
        };
        assert_eq!(status_of("t1"), "completed", "{out}");
        assert_eq!(status_of("t2"), "failed", "{out}");
        assert_eq!(status_of("t3"), "completed", "{out}");
        assert_eq!(status_of("t4"), "skipped", "{out}");
        assert_eq!(status_of("t5"), "completed", "{out}");

        // SC-001: peak in-flight from enter/exit events, never time.
        let peak = tool.test_peak_in_flight();
        assert!(
            peak >= 2,
            "SC-001: a width-2 layer must observably overlap (peak={peak})"
        );
        assert!(
            peak <= 2,
            "SC-001: in-flight must never exceed max_concurrent=2 (peak={peak})"
        );

        // SC-002: waves are barriers between layers, skipped tasks included.
        let spec = PlanExecuteTool::parse_plan(&args).unwrap();
        let layers = PlanExecuteTool::topological_layers(&spec).unwrap();
        let events = tool.test_wave_events();
        let pos_of = |agent: &str, event: &str| -> Option<usize> {
            events.iter().position(|(a, e)| a == agent && *e == event)
        };
        let settle_of = |agent: &str| {
            pos_of(agent, "exit")
                .or_else(|| pos_of(agent, "skipped"))
                .unwrap_or_else(|| panic!("missing settle event for {agent}: {events:?}"))
        };
        for pair in layers.windows(2) {
            let last_settle_prev = pair[0]
                .iter()
                .map(|&i| settle_of(spec.tasks[i].agent.as_str()))
                .max()
                .expect("non-empty layer");
            let first_enter_next = pair[1]
                .iter()
                .filter_map(|&i| pos_of(spec.tasks[i].agent.as_str(), "enter"))
                .min()
                .expect("fixture keeps a runnable task in every layer but the last");
            assert!(
                last_settle_prev < first_enter_next,
                "SC-002: layer barrier violated ({events:?})"
            );
        }
    }

    /// T218: a replayed ok-checkpoint's age rides the `loop.plan.completed`
    /// audit line (report-only staleness observability, design F3-D3) — read
    /// from the existing `workflow_tasks.updated_at`, no schema change.
    #[tokio::test]
    async fn resume_audit_records_checkpoint_age() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        let tool = plan_tool_with(Some(state_db.clone()));

        let plan_id = "aged-checkpoints".to_string();
        let client = zen_repo::SqliteClient::open_lazy(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        let now = chrono::Utc::now().timestamp();
        repo.create_plan(&plan_id, None, "{}", now).await.unwrap();
        repo.checkpoint_task(zen_repo::TaskCheckpoint {
            plan_id: &plan_id,
            task_id: "a",
            agent: "Explore",
            status: "completed",
            response: Some("REPLAYED"),
            error: None,
            now: now - 120,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();
        drop(client);

        let out = tool
            .invoke(serde_json::json!({
                "resume_plan_id": plan_id,
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "already done" },
                    { "id": "b", "agent": "Explore", "prompt": "fresh work" }
                ]
            }))
            .await
            .unwrap();
        assert_eq!(out["resumed"], serde_json::json!(true), "{out}");

        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl"))
            .expect("audit file under ZEN_HOME");
        let completed = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.plan.completed")
            .expect("completion audit line");
        let replayed = completed["replayed_checkpoints"]
            .as_array()
            .expect("additive replayed_checkpoints field");
        assert_eq!(replayed.len(), 1, "only the replayed task is listed");
        assert_eq!(replayed[0]["task"], "a");
        let age = replayed[0]["checkpoint_age_secs"].as_i64().unwrap();
        assert!(
            (100..600).contains(&age),
            "age ≈ the 120s seeded offset, got {age}"
        );
        // Pre-T218 fields stay present (existing consumers keep parsing).
        for field in ["tasks_total", "tasks_ok", "plan_approved", "duration_ms"] {
            assert!(completed.get(field).is_some(), "{field} missing");
        }
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    #[tokio::test]
    async fn resume_while_claimed_by_another_run_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let tool = plan_tool_with(Some(state_db.clone()));

        let plan_id = "fenced-plan".to_string();
        let client = zen_repo::SqliteClient::open_lazy(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        // Seed with the real clock: a fake epoch would count as a stale
        // claim and be legally stolen by the fresh resume.
        let now = chrono::Utc::now().timestamp();
        repo.create_plan(&plan_id, None, "{}", now).await.unwrap();
        assert!(
            repo.claim_plan(
                &plan_id,
                "other-run",
                now,
                zen_repo::DEFAULT_STALE_CLAIM_SECS
            )
            .await
            .unwrap(),
            "seeding the foreign claim must succeed"
        );

        let out = tool
            .invoke(serde_json::json!({
                "resume_plan_id": plan_id,
                "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x" }]
            }))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .unwrap()
                .contains("already being resumed"),
            "a second concurrent resume must lose the claim race: {out}"
        );

        // The refusal leaves the plan running for its rightful owner.
        let row = repo.load_plan(&plan_id).await.unwrap().unwrap();
        assert_eq!(row.status, "running");
    }

    #[test]
    fn upstream_snippet_prefers_response_field_and_truncates() {
        let value = serde_json::json!({
            "agent": "Explore",
            "response": "found 3 notions",
            "rounds": 1
        });
        assert_eq!(upstream_snippet(&value), "found 3 notions");

        let opaque = serde_json::json!({ "error": "budget exhausted" });
        assert_eq!(upstream_snippet(&opaque), r#"{"error":"budget exhausted"}"#);

        let long = "x".repeat(UPSTREAM_SNIPPET_MAX_CHARS + 10);
        let snippet = upstream_snippet(&serde_json::json!({ "response": long }));
        assert!(snippet.starts_with(&"x".repeat(UPSTREAM_SNIPPET_MAX_CHARS)));
        assert!(snippet.ends_with("…[truncated]"));
    }

    #[test]
    fn compose_task_prompt_passthrough_without_upstream() {
        assert_eq!(compose_task_prompt("do the thing", &[]), "do the thing");
    }

    #[test]
    fn compose_task_prompt_appends_upstream_outputs() {
        let upstream = vec![
            ("analysis".to_string(), "3 key findings".to_string()),
            ("fetch".to_string(), "raw dataset rows".to_string()),
        ];
        let prompt = compose_task_prompt("synthesize a report", &upstream);
        assert!(prompt.starts_with("synthesize a report"));
        assert!(prompt.contains("upstream task `analysis` output"));
        assert!(prompt.contains("3 key findings"));
        assert!(prompt.contains("upstream task `fetch` output"));
        assert!(prompt.contains("raw dataset rows"));
    }

    #[test]
    fn parse_plan_validates_failure_edge_and_artifact() {
        for (raw, expected) in [
            ("retry", FailureAction::Retry),
            ("skip", FailureAction::Skip),
            ("halt", FailureAction::Halt),
        ] {
            let args = serde_json::json!({
                "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x", "on_failure": raw }]
            });
            assert_eq!(
                PlanExecuteTool::parse_plan(&args).unwrap().tasks[0].on_failure,
                expected
            );
        }

        let bad = serde_json::json!({
            "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x", "on_failure": "explode" }]
        });
        assert!(
            PlanExecuteTool::parse_plan(&bad)
                .unwrap_err()
                .contains("on_failure"),
            "unknown failure action must be rejected"
        );

        let empty = serde_json::json!({
            "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x", "artifact": "  " }]
        });
        assert!(
            PlanExecuteTool::parse_plan(&empty)
                .unwrap_err()
                .contains("artifact"),
            "an empty artifact path must be rejected"
        );

        let none = serde_json::json!({
            "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x" }]
        });
        let spec = &PlanExecuteTool::parse_plan(&none).unwrap().tasks[0];
        assert_eq!(spec.on_failure, FailureAction::Skip, "default is skip");
        assert!(spec.artifact.is_none(), "artifact is optional");
    }

    /// W2.1 byte-identity pin: a plan with NO `on_failure` emits exactly the
    /// pre-W2 audit keys — no `task_retries`/`halted`/`rewind` fields.
    #[tokio::test]
    async fn default_plan_audit_line_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "name": "default",
                "tasks": [{ "id": "a", "agent": "Explore", "prompt": "x" }]
            }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "{out}");

        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl"))
            .expect("audit file under ZEN_HOME");
        let completed = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.plan.completed")
            .expect("completion audit line");
        let keys: std::collections::BTreeSet<&str> = completed
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "kind",
            "plan",
            "plan_id",
            "tasks_total",
            "tasks_ok",
            "tasks_failed",
            "tasks_skipped",
            "plan_approved",
            "delivery_ready",
            "duration_ms",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected, "default plan audit line is byte-identical");
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    #[tokio::test]
    async fn retry_edge_reruns_failed_task_once_then_downstream_runs() {
        let tool = plan_tool();
        tool.inject_test_failure("Atlas", 1);
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "a", "agent": "Atlas", "prompt": "flaky", "on_failure": "retry" },
                    { "id": "b", "agent": "Explore", "prompt": "downstream", "depends_on": ["a"] }
                ]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "completed", "retry succeeds: {out}");
        assert_eq!(
            rows[1]["status"], "completed",
            "downstream runs after retry: {out}"
        );
        assert_eq!(tool.test_call_count("Atlas"), 2, "exactly one retry");
    }

    /// W2.1 minimal-subgraph repair: only the failed node re-runs; a verified
    /// sibling in the same layer keeps its single execution/checkpoint.
    #[tokio::test]
    async fn retry_reuses_verified_siblings() {
        let tool = plan_tool();
        tool.inject_test_failure("Atlas", 1);
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "root", "agent": "Explore", "prompt": "root" },
                    { "id": "sib", "agent": "Explore", "prompt": "sibling", "depends_on": ["root"] },
                    { "id": "flaky", "agent": "Atlas", "prompt": "flaky", "depends_on": ["root"], "on_failure": "retry" },
                    { "id": "join", "agent": "Explore", "prompt": "join", "depends_on": ["sib", "flaky"] }
                ]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert!(rows.iter().all(|r| r["status"] == "completed"), "{out}");
        assert_eq!(
            tool.test_call_count("Atlas"),
            2,
            "only the failed node retries"
        );
        assert_eq!(
            tool.test_call_count("Explore"),
            3,
            "root + sibling + join each ran exactly once (no subtree re-execution)"
        );
    }

    #[tokio::test]
    async fn halt_edge_stops_the_layer_chain() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let tool = plan_tool();
        tool.inject_test_failure("Atlas", 1);
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "a", "agent": "Atlas", "prompt": "boom", "on_failure": "halt" },
                    { "id": "b", "agent": "Explore", "prompt": "independent" },
                    { "id": "c", "agent": "Explore", "prompt": "next layer", "depends_on": ["b"] }
                ]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "failed", "{out}");
        assert_eq!(
            rows[1]["status"], "completed",
            "same-layer independent task ran: {out}"
        );
        assert_eq!(rows[2]["status"], "skipped", "next layer never ran: {out}");
        assert_eq!(rows[2]["error"], "never scheduled", "{out}");

        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl")).unwrap();
        let completed = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.plan.completed")
            .expect("completion audit line");
        assert_eq!(completed["halted"], serde_json::json!(true), "{completed}");
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    #[tokio::test]
    async fn halt_edge_on_validation_failure_stops_chain() {
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "a", "agent": "NoSuchAgent", "prompt": "x", "on_failure": "halt" },
                    { "id": "b", "agent": "Explore", "prompt": "independent" },
                    { "id": "c", "agent": "Explore", "prompt": "next layer", "depends_on": ["b"] }
                ]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "failed", "{out}");
        assert_eq!(rows[1]["status"], "completed", "{out}");
        assert_eq!(rows[2]["status"], "skipped", "{out}");
    }

    #[tokio::test]
    async fn plan_failure_writes_rewind_picked_up_by_reader() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "name": "nightly",
                "tasks": [{ "id": "compile", "agent": "NoSuchAgent", "prompt": "x" }]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "failed", "{out}");

        let rewinds_dir = dir.path().join("vault/wiki/wisdom/rewinds");
        let files: Vec<_> = std::fs::read_dir(&rewinds_dir)
            .expect("rewinds dir created")
            .collect();
        assert_eq!(files.len(), 1, "one rewind memory per failed plan");

        let rewinds = zen_vault::distill::reflection::recent_rewinds(&rewinds_dir, 5);
        assert_eq!(rewinds.len(), 1, "next injection picks up the rewind");
        assert!(rewinds[0].contains("nightly"), "{}", rewinds[0]);
        assert!(
            rewinds[0].to_lowercase().contains("do not re-propose"),
            "guidance present: {}",
            rewinds[0]
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    #[tokio::test]
    async fn plan_task_artifact_without_grant_fails() {
        let tool = plan_tool();
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [{
                    "id": "x",
                    "agent": "Explore",
                    "prompt": "write it",
                    "artifact": "out.md"
                }]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "failed", "{out}");
        assert!(
            rows[0]["error"].as_str().unwrap().contains("fs.write"),
            "no-grant agent cannot declare an artifact: {out}"
        );
    }

    // W9.1: a plan whose tasks have no recorded (agent, model) duration
    // history emits NO forecast line at all — the omission is the contract.
    #[tokio::test]
    async fn no_history_pair_emits_no_forecast_line() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let tool = plan_tool_with(Some(dir.path().join("state.db")));
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "x" },
                    { "id": "b", "agent": "Explore", "prompt": "y" }
                ]
            }))
            .await
            .unwrap();
        let progress = out["progress"].as_array().cloned().unwrap_or_default();
        assert!(
            !progress
                .iter()
                .any(|l| l.as_str().is_some_and(|s| s.contains("(estimate)"))),
            "no forecast line without history: {progress:?}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W9.1: recorded durations produce the exact advisory line — lower =
    // Σ per-task min, upper = Σ per-task max, minutes ceil'd.
    #[tokio::test]
    async fn forecast_line_uses_recorded_duration_history() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan("hist", None, "{}", 1_000).await.unwrap();
        for (tid, ms) in [("h1", 120_000i64), ("h2", 240_000i64)] {
            repo.checkpoint_task(zen_repo::TaskCheckpoint {
                plan_id: "hist",
                task_id: tid,
                agent: "Explore",
                status: "completed",
                response: Some("done"),
                error: None,
                now: 1_100,
                model: None,
                duration_ms: Some(ms as u64),
                cost_usd: None,
            })
            .await
            .unwrap();
        }
        let tool = plan_tool_with(Some(state_db));
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "x" },
                    { "id": "b", "agent": "Explore", "prompt": "y" }
                ]
            }))
            .await
            .unwrap();
        let progress = out["progress"].as_array().expect("progress blob");
        assert_eq!(
            progress[0],
            serde_json::json!("plan 2 tasks · ~4-8 min (estimate)"),
            "forecast line first: {progress:?}"
        );
        assert!(
            !progress[0].as_str().unwrap().contains('$'),
            "no cost segment without cost history: {progress:?}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W9.1: the `$lo-$hi` segment appears only when the contributing pairs
    // carry recorded cost — summed min..max, two decimals.
    #[tokio::test]
    async fn forecast_line_includes_cost_segment_from_cost_history() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan("hist", None, "{}", 1_000).await.unwrap();
        for (tid, cost) in [("h1", 0.01f64), ("h2", 0.03f64)] {
            repo.checkpoint_task(zen_repo::TaskCheckpoint {
                plan_id: "hist",
                task_id: tid,
                agent: "Explore",
                status: "completed",
                response: Some("done"),
                error: None,
                now: 1_100,
                model: None,
                duration_ms: Some(60_000),
                cost_usd: Some(cost),
            })
            .await
            .unwrap();
        }
        let tool = plan_tool_with(Some(state_db));
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "x" },
                    { "id": "b", "agent": "Explore", "prompt": "y" }
                ]
            }))
            .await
            .unwrap();
        let progress = out["progress"].as_array().expect("progress blob");
        assert_eq!(
            progress[0],
            serde_json::json!("plan 2 tasks · ~2-2 min · ~$0.02-$0.06 (estimate)"),
            "cost segment from history: {progress:?}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W9.4: recognized failure classes produce numbered proposals that
    // cite resume_plan_id (checkpoint reuse is the existing resume path).
    #[test]
    fn triage_proposals_recognized_classes_cite_resume_plan_id() {
        let proposals = triage_proposals(
            "p1",
            &[
                "task timed out after 600s",
                "no local provider is reachable",
            ],
        );
        assert_eq!(proposals.len(), 2, "{proposals:?}");
        for p in &proposals {
            assert!(p.contains("resume_plan_id"), "{p}");
        }
        assert!(proposals[0].contains("timed out"), "{proposals:?}");
        assert!(proposals[1].contains("local provider"), "{proposals:?}");
    }

    // W9.4: error text matching no table needle yields ZERO proposals —
    // the raw error stays in the task row instead of a guessed repair.
    #[test]
    fn triage_proposals_unknown_class_yields_zero_proposals() {
        let proposals = triage_proposals("p1", &["failed to build delegate agent \"X\""]);
        assert!(proposals.is_empty(), "{proposals:?}");
        let proposals = triage_proposals("p1", &[]);
        assert!(proposals.is_empty(), "{proposals:?}");
    }

    // W9.4: proposals deduplicate by class text and cap at 3.
    #[test]
    fn triage_proposals_dedup_and_cap_at_three() {
        let proposals = triage_proposals(
            "p1",
            &[
                "task timed out",
                "timeout waiting for first token",
                "budget exhausted",
                "approval required",
            ],
        );
        assert!(proposals.len() <= 3, "{proposals:?}");
        assert_eq!(proposals.len(), 3, "dedup keeps distinct classes");
        let all = proposals.join("\n");
        assert_eq!(all.matches("timed out").count(), 1, "{all}");
    }

    // W9.3: a cancel signal that lands before the first batch stops the
    // walk with ZERO completed tasks — every task exits skipped, the audit
    // line carries `cancelled: true`, and no `completed` can appear.
    #[tokio::test]
    async fn pre_signal_cancels_before_first_batch_yields_no_completed() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan("pre", None, "{}", 1_000).await.unwrap();
        // The guard must stay held across the invoke: the registry entry
        // lives until its last guard drops, and the executor JOINS the
        // existing channel only while the entry exists.
        let (_guard, rx) = crate::plan_control::register_live_plan("pre");
        assert!(crate::plan_control::signal_cancel("pre"));
        assert!(*rx.borrow(), "signal observed");
        let tool = plan_tool_with(Some(state_db));
        let out = tool
            .invoke(serde_json::json!({
                "resume_plan_id": "pre",
                "tasks": [
                    { "id": "a", "agent": "Explore", "prompt": "x" },
                    { "id": "b", "agent": "Explore", "prompt": "y" }
                ]
            }))
            .await
            .unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{out}");
        for row in rows {
            assert_eq!(row["status"], "skipped", "{row}");
            assert!(row["response"].is_null(), "no partial completed: {row}");
        }
        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl"))
            .expect("audit file under ZEN_HOME");
        let completed = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.plan.completed")
            .expect("completion audit line");
        assert_eq!(
            completed["cancelled"],
            serde_json::json!(true),
            "{completed}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W9.3: cancel DURING a live batch drops the in-flight sub-turns —
    // they exit `failed` (never completed) and the plan never records a
    // partial success.
    #[tokio::test]
    async fn cancel_mid_batch_drops_inflight_to_failed() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan("pre2", None, "{}", 1_000).await.unwrap();
        let tool = plan_tool_with(Some(state_db));
        tool.inject_test_wave_delay("Explore", 5_000);
        let handle = tokio::spawn({
            async move {
                tool.invoke(serde_json::json!({
                    "resume_plan_id": "pre2",
                    "tasks": [
                        { "id": "a", "agent": "Explore", "prompt": "x" },
                        { "id": "b", "agent": "Explore", "prompt": "y" }
                    ]
                }))
                .await
            }
        });
        // Wait until the executor is INSIDE the batch (in_batch=false
        // means a batch is in flight), then cancel.
        let mut in_batch = false;
        for _ in 0..1000 {
            if crate::plan_control::is_live_between_batches("pre2") == Some(false) {
                in_batch = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(in_batch, "executor never entered the batch");
        assert!(crate::plan_control::signal_cancel("pre2"));
        let out = handle.await.unwrap().unwrap();
        let rows = out["tasks"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{out}");
        for row in rows {
            assert_eq!(row["status"], "failed", "{row}");
            assert!(
                row["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("cancelled mid-batch")),
                "{row}"
            );
        }
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    /// W11.5/T5 (SC-021): plan tasks accept a per-task fork_mode.
    #[test]
    fn parse_plan_accepts_per_task_fork_mode() {
        let args = serde_json::json!({
            "tasks": [{"id": "a", "agent": "Explore", "prompt": "p", "fork_mode": "knowledge"}]
        });
        let plan = PlanExecuteTool::parse_plan(&args).unwrap();
        assert_eq!(plan.tasks[0].fork_mode, Some(ForkMode::Knowledge));
        assert!(plan.tasks[0].model_hint.is_none());
    }

    /// W11.5/T5 (SC-021/022): absent fork/model keeps the session-wide
    /// default byte-identically (both fields None).
    #[test]
    fn parse_plan_absent_fork_model_keeps_session_default() {
        let args = serde_json::json!({
            "tasks": [{"id": "a", "agent": "Explore", "prompt": "p"}]
        });
        let plan = PlanExecuteTool::parse_plan(&args).unwrap();
        assert!(plan.tasks[0].fork_mode.is_none());
        assert!(plan.tasks[0].model_hint.is_none());
    }

    /// W11.5/T5: an invalid plan-task fork_mode is a structured rejection.
    #[test]
    fn parse_plan_rejects_invalid_fork_mode() {
        let args = serde_json::json!({
            "tasks": [{"id": "a", "agent": "Explore", "prompt": "p", "fork_mode": "bogus"}]
        });
        let err = PlanExecuteTool::parse_plan(&args).unwrap_err();
        assert!(err.contains("fork_mode"), "{err}");
    }

    /// W11.5/T5 (SC-022): the 6th distinct model hint is rejected — ONE
    /// shared rule with delegate.task (≤5 distinct per batch).
    #[test]
    fn parse_plan_rejects_sixth_distinct_model_hint() {
        let tasks: Vec<serde_json::Value> = (0..6)
            .map(|i| {
                serde_json::json!({
                    "id": format!("t{i}"), "agent": "Explore", "prompt": "p",
                    "model": format!("hint-{i}")
                })
            })
            .collect();
        let args = serde_json::json!({ "tasks": tasks });
        let err = PlanExecuteTool::parse_plan(&args).unwrap_err();
        assert!(err.contains("distinct model hints"), "{err}");
    }

    /// W11.6/T6 (F6+OV-3): a forced mint failure is a STRUCTURED rejection
    /// and leaves zero orphan rows — never a phantom plan run.
    #[tokio::test]
    async fn mint_fresh_plan_failure_is_structured_with_no_orphan_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mint_fail.db");
        let client = Arc::new(zen_repo::SqliteClient::open(&db).await.unwrap());
        // Force the plan INSERT to abort (simulates disk-full/locked DB).
        client
            .writer()
            .call(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER reject_plan BEFORE INSERT ON workflow_plans \
                     BEGIN SELECT RAISE(ABORT, 'forced mint failure'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let args = serde_json::json!({ "tasks": [] });
        let err = mint_fresh_plan(&client, &args, None, "s1", &Sensitivity::Private)
            .await
            .expect_err("a forced create failure must reject");
        assert!(
            err["error"].as_str().unwrap().contains("plan mint failed"),
            "structured rejection: {err}"
        );
        // The atomic mint rolled back: no phantom plan, no orphan tasks.
        let (plans, tasks): (i64, i64) = client
            .writer()
            .call(|conn| {
                let p: i64 =
                    conn.query_row("SELECT COUNT(*) FROM workflow_plans", [], |r| r.get(0))?;
                let t: i64 =
                    conn.query_row("SELECT COUNT(*) FROM workflow_tasks", [], |r| r.get(0))?;
                Ok((p, t))
            })
            .await
            .unwrap();
        assert_eq!(plans, 0, "no phantom plan row");
        assert_eq!(tasks, 0, "no orphan task rows");
    }

    /// W12.15 pin (half 1/2): the spec_json reader accepts both producer
    /// shapes — lowercase tags from `plan.todos` / the direct mint, and the
    /// capitalized `Sensitivity::to_string()` form for forward
    /// compatibility — while absent/invalid/non-string/non-JSON markers
    /// stay `None` (legacy row ⇒ the resuming session's policy, the
    /// byte-identical pre-W12.15 behavior).
    #[test]
    fn plan_recorded_sensitivity_maps_every_tag_shape() {
        for (raw, want) in [
            (
                r#"{"sensitivity":"confidential"}"#,
                Sensitivity::Confidential,
            ),
            (r#"{"sensitivity":"private"}"#, Sensitivity::Private),
            (r#"{"sensitivity":"public"}"#, Sensitivity::Public),
            (
                r#"{"sensitivity":"Confidential"}"#,
                Sensitivity::Confidential,
            ),
        ] {
            assert_eq!(plan_recorded_sensitivity(raw), Some(want), "{raw}");
        }
        for raw in [
            "{}",
            r#"{"sensitivity":"bogus"}"#,
            r#"{"sensitivity":3}"#,
            "not json",
        ] {
            assert_eq!(plan_recorded_sensitivity(raw), None, "{raw}");
        }
    }

    /// W12.15 pin (half 2/2): a plan row recorded Confidential is resumed
    /// by a session whose delegate shared-cell is Public (`plan_tool_with`
    /// hard-codes it) and STILL completes. The sub-turn policy itself is a
    /// task-local inside `run_task_under_policy` — not observable through
    /// the public seam — so the contract is pinned by the reader test above
    /// plus this mismatched-cell resume: the record is read at resume
    /// (`plan_recorded_sensitivity`) and the turn runs under it.
    #[tokio::test]
    async fn resume_scopes_sub_turns_under_recorded_plan_sensitivity() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        // A crashed Confidential plan: same spec_json shape `plan.todos`
        // writes (D24 §5), owner released by the crash, aged past the
        // resume fence's staleness window so the claim can be taken.
        let spec = serde_json::json!({
            "items": [{"id": "a", "title": "audit", "status": "pending"}],
            "sensitivity": "confidential",
            "session_id": "local",
        })
        .to_string();
        let plan_id = "confidential-plan".to_string();
        repo.mint_plan_with_tasks(
            &plan_id,
            None,
            &spec,
            "crashed-run",
            &[zen_repo::TaskDraft {
                id: "a".into(),
                status: "pending".into(),
            }],
            chrono::Utc::now().timestamp() - 7_200,
        )
        .await
        .unwrap();
        let row = repo.load_plan(&plan_id).await.unwrap().expect("row");
        assert_eq!(
            plan_recorded_sensitivity(&row.spec_json),
            Some(Sensitivity::Confidential),
            "recorded plan policy: {}",
            row.spec_json
        );

        let out = plan_tool_with(Some(state_db))
            .invoke(serde_json::json!({
                "resume_plan_id": plan_id,
                "tasks": [{"id": "a", "agent": "Explore", "prompt": "audit"}]
            }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "{out}");
        assert_eq!(out["resumed"], serde_json::json!(true), "{out}");
        assert_eq!(out["tasks"][0]["status"], "completed", "{out}");
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    /// W12.16b pin: a cancel that landed in the finalization window (the
    /// watch is ALREADY signalled when the terminal write is attempted)
    /// wins — `complete_plan_checked` reports the cancel and NEVER rewrites
    /// the row, so a cancelled plan cannot be resurrected as completed.
    ///
    /// W12.16c deviation (W12.6 precedent): the DST twin is not runnable
    /// at this stack depth. A 40-seed shuttle sweep over the same race was
    /// attempted and panics inside tokio-rusqlite's background OS thread
    /// (`ExecutionState is not set` — the writer thread's real-tokio oneshot
    /// wake cannot enter a Shuttle schedule), so this deterministic pin plus
    /// the SQL-guard pin in `workflow_repo.rs` are the gate.
    #[tokio::test]
    async fn cancel_signal_in_finalization_window_wins_without_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan("p1", None, "{}", 1_000).await.unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        tx.send(true).unwrap();
        let mut warnings: Vec<String> = Vec::new();
        let cancel_won = plan_tool()
            .complete_plan_checked(
                &repo,
                PlanCompletion {
                    plan_id: "p1",
                    final_status: "completed",
                    summary: "late engine outcome",
                    plan_approved: true,
                    delivery_ready: true,
                },
                Some(&rx),
                &mut warnings,
            )
            .await;
        assert!(cancel_won, "the pre-signalled cancel owns the tail window");
        assert!(
            warnings.is_empty(),
            "a won cancel is not a warning: {warnings:?}"
        );
        let row = repo.load_plan("p1").await.unwrap().expect("row");
        assert_eq!(row.status, "running", "no complete_plan rewrite");
        assert!(row.summary.is_none(), "engine summary discarded");
    }

    /// W12.17 pin: the direct fresh mint writes the SAME session_id +
    /// sensitivity marker `plan.todos` writes, so the mint-once guard
    /// (`latest_plan_for_session`) sees it — the blind spot where
    /// `plan.execute`-minted plans were invisible to a second `set` is
    /// closed.
    #[tokio::test]
    async fn fresh_mint_marker_visible_to_mint_once_guard() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        let client = Arc::new(zen_repo::SqliteClient::open(&state_db).await.unwrap());
        let plan_id = mint_fresh_plan(
            &client,
            &serde_json::json!({"tasks": []}),
            None,
            "local",
            &Sensitivity::Confidential,
        )
        .await
        .expect("fresh mint");

        let repo = zen_repo::WorkflowRepo::new(&client);
        let row = repo.load_plan(&plan_id).await.unwrap().expect("minted row");
        let spec: serde_json::Value = serde_json::from_str(&row.spec_json).unwrap();
        assert_eq!(
            spec["session_id"],
            serde_json::json!("local"),
            "session marker: {}",
            row.spec_json
        );
        assert_eq!(
            spec["sensitivity"],
            serde_json::json!("confidential"),
            "sensitivity marker: {}",
            row.spec_json
        );

        // The mint-once guard now sees the direct-mint row: a ledger set
        // for the same session is refused instead of minting a second
        // active plan.
        let todos = crate::plan_todos::PlanTodosTool::new(
            Some(state_db),
            Arc::new(std::sync::Mutex::new(Sensitivity::Private)),
        );
        let out = todos
            .invoke(serde_json::json!({"op": "set", "items": [
                {"id": "x", "title": "second ledger", "status": "pending"},
            ]}))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .is_some_and(|e| e.contains("mint-once")),
            "direct-mint row is visible to the guard: {out}"
        );
        assert_eq!(out["plan_id"], serde_json::json!(plan_id), "{out}");
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    /// W12.18 pin: a checkpoint persistence failure is not discarded — it
    /// surfaces as a `persistence_warnings` entry in the result payload
    /// while the sibling outcome stays visible (fail-visible, not
    /// fail-silent double-spend).
    #[tokio::test]
    async fn checkpoint_failure_surfaces_in_persistence_warnings() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        // Mint the ledger first (its task inserts must succeed), THEN make
        // every later task write abort — the checkpoint path.
        let todos = crate::plan_todos::PlanTodosTool::new(
            Some(state_db.clone()),
            Arc::new(std::sync::Mutex::new(Sensitivity::Private)),
        );
        let minted = todos
            .invoke(serde_json::json!({"op": "set", "items": [
                {"id": "a", "title": "only", "status": "pending"},
            ]}))
            .await
            .unwrap();
        assert!(minted["plan_id"].is_string(), "{minted}");

        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        client
            .writer()
            .call(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER reject_checkpoint BEFORE INSERT ON workflow_tasks \
                     BEGIN SELECT RAISE(ABORT, 'forced checkpoint failure'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let out = plan_tool_with(Some(state_db))
            .invoke(serde_json::json!({
                "tasks": [{"id": "a", "agent": "Explore", "prompt": "x"}]
            }))
            .await
            .unwrap();
        assert_eq!(out["tasks"][0]["status"], "completed", "{out}");
        let warnings = out["persistence_warnings"]
            .as_array()
            .expect("persistence_warnings array");
        assert!(
            warnings.iter().any(|w| w
                .as_str()
                .is_some_and(|s| s.contains("checkpoint task `a` failed"))),
            "checkpoint failure surfaced: {out}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    /// W12.25a pin: the checkpoint records a model label from the delegate
    /// output — a real `model` key wins, otherwise the falsifiable
    /// `model_source` label (the only producer today) is stored. cost_usd
    /// stays honest NULL (no usage-cost producer at the delegate sites).
    #[tokio::test]
    async fn checkpoint_records_model_from_source_label_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan("p", None, "{}", 1_000).await.unwrap();
        let tool = plan_tool_with(Some(state_db));

        tool.checkpoint(
            "p",
            &spec("a", "Explore", &[]),
            &TaskOutcome {
                status: "completed",
                value: serde_json::json!({
                    "response": "r1",
                    "model_source": "hint",
                    "duration_ms": 1_500,
                }),
            },
        )
        .await
        .unwrap();
        tool.checkpoint(
            "p",
            &spec("b", "Explore", &[]),
            &TaskOutcome {
                status: "completed",
                value: serde_json::json!({
                    "response": "r2",
                    "model": "literal-model",
                    "model_source": "hint",
                    "duration_ms": 2_500,
                }),
            },
        )
        .await
        .unwrap();

        let history = repo.duration_history().await.unwrap();
        let models: Vec<Option<String>> = history.iter().map(|r| r.model.clone()).collect();
        assert!(
            models.contains(&Some("hint".to_string())),
            "model_source fallback recorded: {models:?}"
        );
        assert!(
            models.contains(&Some("literal-model".to_string())),
            "a real model key wins over the label: {models:?}"
        );
        assert!(
            history.iter().all(|r| r.cost_usd.is_none()),
            "no fabricated cost: {history:?}"
        );
    }

    /// W12.25b/c pin: forecast history is keyed by (agent, model) on BOTH
    /// sides. Rows written with a real model label only match a task that
    /// declares the same `model` hint; the hintless row of the same agent
    /// (the old unconditional `""` key) must not contribute.
    #[tokio::test]
    async fn forecast_reads_real_model_history_key() {
        let home = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", home.path()) };
        // The task's model hint must exist in the configured catalog.
        std::fs::write(
            home.path().join("config.toml"),
            "[providers.openai.models.hint-x]\nmodel = \"hint-x\"\n",
        )
        .unwrap();
        zen_core::config::invalidate_config_cache();

        let state_db = home.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan("hist", None, "{}", 1_000).await.unwrap();
        for (tid, model, ms) in [
            ("h1", Some("hint-x"), 120_000i64),
            // The discriminator: the OLD lookup key ("" for every task)
            // would sum this hintless row's 10 minutes instead.
            ("h2", None, 600_000i64),
        ] {
            repo.checkpoint_task(zen_repo::TaskCheckpoint {
                plan_id: "hist",
                task_id: tid,
                agent: "Explore",
                status: "completed",
                response: Some("done"),
                error: None,
                now: 1_100,
                model,
                duration_ms: Some(ms as u64),
                cost_usd: None,
            })
            .await
            .unwrap();
        }

        let out = plan_tool_with(Some(state_db))
            .invoke(serde_json::json!({
                "tasks": [{
                    "id": "a", "agent": "Explore", "prompt": "x", "model": "hint-x"
                }]
            }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "{out}");
        let progress = out["progress"].as_array().expect("progress blob");
        assert_eq!(
            progress[0],
            serde_json::json!("plan 1 tasks · ~2-2 min (estimate)"),
            "the model-keyed history (2 min), not the hintless row (10 min): {progress:?}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W14.1 (eng-review T5): a dispatched ledger batch merges its DAG into
    // the minted plan's spec_json, so the induction corpus can observe the
    // ATTACHED shape while `items`/`session_id`/`sensitivity` stay intact.
    #[tokio::test]
    async fn attach_merges_batch_dag_into_spec_json_for_induction() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let todos = crate::plan_todos::PlanTodosTool::new(
            Some(state_db.clone()),
            Arc::new(std::sync::Mutex::new(zen_core::types::Sensitivity::Private)),
        );
        let minted = todos
            .invoke(serde_json::json!({"op": "set", "items": [
                {"id": "a", "title": "first", "status": "pending"},
                {"id": "b", "title": "second", "status": "pending"},
            ]}))
            .await
            .unwrap();
        let plan_id = minted["plan_id"].as_str().unwrap().to_string();

        let plan = plan_tool_with(Some(state_db.clone()));
        let out = plan
            .invoke(serde_json::json!({
                "name": "attach-dag",
                "tasks": [
                    {"id": "a", "agent": "Explore", "prompt": "do a", "depends_on": []},
                    {"id": "b", "agent": "Librarian", "prompt": "do b", "depends_on": []},
                ]
            }))
            .await
            .unwrap();
        assert!(out["error"].is_null(), "{out}");

        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        let row = repo.load_plan(&plan_id).await.unwrap().expect("plan row");
        let spec: serde_json::Value = serde_json::from_str(&row.spec_json).unwrap();
        let tasks = spec["tasks"].as_array().expect("merged tasks array");
        assert_eq!(
            tasks[0]["agent"],
            serde_json::json!("Explore"),
            "tasks[0]: {tasks:?}"
        );

        match crate::workflow_induction::shape_signature_from_spec_json(&row.spec_json) {
            Ok(crate::workflow_induction::SpecShape::Dag { signature, .. }) => {
                assert!(!signature.is_empty(), "non-empty DAG signature");
            }
            other => panic!("expected a DAG shape, got {other:?}"),
        }

        assert!(
            spec.get("items").is_some(),
            "items preserved: {}",
            row.spec_json
        );
        assert!(
            spec.get("session_id").is_some(),
            "session_id preserved: {}",
            row.spec_json
        );
        assert!(
            spec.get("sensitivity").is_some(),
            "sensitivity preserved: {}",
            row.spec_json
        );
    }
}
