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

use crate::delegate_task::{DELEGATE_ROOT_PARENT, DelegateRequest, DelegateTaskTool};
use crate::review::QualityPipeline;

pub const PLAN_TOOL_NAME: &str = "plan.execute";

/// Hard cap on plan size — bigger workflows must be split by the model
/// into multiple plans (001 A.6: consumer-side decision).
pub const MAX_PLAN_TASKS: usize = 12;

/// Hard cap on DAG depth in layers. Three layers cover draft → parallel
/// analysis → synthesis; anything deeper smells like a plan that should
/// itself be delegated (and hits the delegate depth matrix anyway).
pub const MAX_PLAN_LAYERS: usize = 3;

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
            tasks.push(PlanTaskSpec {
                id: id.to_string(),
                agent: agent.to_string(),
                prompt: prompt.to_string(),
                depends_on,
                on_failure: parse_failure_action(item, id)?,
                artifact: parse_artifact(item, id)?,
            });
        }
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
    async fn checkpoint(&self, plan_id: &str, task: &PlanTaskSpec, outcome: &TaskOutcome) {
        if plan_id.is_empty() {
            return;
        }
        let Some(db) = self.db().await else { return };
        let repo = zen_repo::WorkflowRepo::new(db);
        let _ = repo
            .checkpoint_task(zen_repo::TaskCheckpoint {
                plan_id,
                task_id: &task.id,
                agent: &task.agent,
                status: outcome.status,
                response: outcome.value.get("response").and_then(|v| v.as_str()),
                error: outcome.value.get("error").and_then(|v| v.as_str()),
                now: chrono::Utc::now().timestamp(),
            })
            .await;
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
                                "status": { "type": "string", "enum": ["ok", "failed", "skipped"] },
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
                            .claim_plan(&rid, &owner, chrono::Utc::now().timestamp())
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
                        let resumed_now = chrono::Utc::now().timestamp();
                        for t in repo.load_task_rows(&rid).await.unwrap_or_default() {
                            if t.status != "ok" {
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
                                status: "ok",
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
                let plan_id = uuid::Uuid::new_v4().to_string();
                let repo = zen_repo::WorkflowRepo::new(db);
                let spec_json = serde_json::to_string(&args).unwrap_or_default();
                let _ = repo
                    .create_plan(
                        &plan_id,
                        plan.name.as_deref(),
                        &spec_json,
                        chrono::Utc::now().timestamp(),
                    )
                    .await;
                plan_id
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
        for layer in &layers {
            let mut halt_hit = false;
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
                        Some(o) if o.status == "ok" => {
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
                    self.checkpoint(&plan_id, &plan.tasks[i], &outcome).await;
                    outcomes[i] = Some(outcome);
                    continue;
                }
                let req = DelegateRequest {
                    agent: plan.tasks[i].agent.clone(),
                    prompt: compose_task_prompt(&plan.tasks[i].prompt, &upstream),
                    description: Some(format!("plan task `{}`", plan.tasks[i].id)),
                    independent: true,
                    artifact: plan.tasks[i].artifact.clone(),
                };
                if let Some(error) = self.delegate.validate_task(DELEGATE_ROOT_PARENT, 0, &req) {
                    if plan.tasks[i].on_failure == FailureAction::Halt {
                        halt_hit = true;
                    }
                    let outcome = TaskOutcome {
                        status: "failed",
                        value: serde_json::json!({ "error": error }),
                    };
                    self.checkpoint(&plan_id, &plan.tasks[i], &outcome).await;
                    outcomes[i] = Some(outcome);
                    continue;
                }
                runnable.push((i, req));
            }
            for batch in runnable.chunks(self.max_concurrent.max(1) as usize) {
                let futures: Vec<_> = batch
                    .iter()
                    .map(|(_, req)| {
                        let req = req.clone();
                        async move {
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
                            let value = self
                                .delegate
                                .run_single(&req, 0, DELEGATE_ROOT_PARENT)
                                .await;
                            let status = if value.get("error").is_some() {
                                "failed"
                            } else {
                                "ok"
                            };
                            TaskOutcome { status, value }
                        }
                    })
                    .collect();
                let results = futures::future::join_all(futures).await;

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
                    self.checkpoint(&plan_id, &plan.tasks[i], outcome).await;
                }
                for (&(i, _), outcome) in batch.iter().zip(results) {
                    outcomes[i] = Some(outcome);
                }

                for (i, req) in retry_batch {
                    #[cfg(test)]
                    self.test_note_call(&req.agent);
                    let value = self
                        .delegate
                        .run_single(&req, 0, DELEGATE_ROOT_PARENT)
                        .await;
                    let status = if value.get("error").is_some() {
                        "failed"
                    } else {
                        "ok"
                    };
                    let outcome = TaskOutcome { status, value };
                    self.checkpoint(&plan_id, &plan.tasks[i], &outcome).await;
                    outcomes[i] = Some(outcome);
                    retry_count += 1;
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
                "ok" => ok += 1,
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
        let sensitivity = self.delegate.current_sensitivity();
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
            let _ = repo
                .complete_plan(
                    &plan_id,
                    final_status,
                    Some(&summary),
                    Some(gate.plan_approved),
                    Some(gate.delivery_ready),
                    chrono::Utc::now().timestamp(),
                )
                .await;
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
        // W2.2 (D12): the plan-failure rollback boundary persists a rewind
        // memory so the next attempt does not repeat the failed path. Best-
        // effort; a write failure never affects the plan result.
        if failed > 0
            && let Some(path) = record_plan_rewind(&plan, &outcomes)
        {
            audit_entry["rewind"] = serde_json::json!(path.display().to_string());
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
        if !plan_id.is_empty() {
            out["plan_id"] = serde_json::json!(plan_id);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, agent: &str, deps: &[&str]) -> PlanTaskSpec {
        PlanTaskSpec {
            id: id.to_string(),
            agent: agent.to_string(),
            prompt: format!("do {id}"),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            on_failure: FailureAction::Skip,
            artifact: None,
        }
    }

    fn plan(tasks: Vec<PlanTaskSpec>) -> PlanSpec {
        PlanSpec {
            name: Some("test".into()),
            tasks,
        }
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
        assert!(rows.iter().all(|r| r["status"] == "ok"), "{out}");
        assert!(out["hermes"]["plan_approved"].is_boolean(), "{out}");
        assert!(out["hermes"]["delivery_ready"].is_boolean(), "{out}");
        assert!(out["summary"].as_str().unwrap().contains("[ok] d"), "{out}");
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
            status: "ok",
            response: Some("REPLAYED"),
            error: None,
            now: 1_001,
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
        assert_eq!(rows[1]["status"], "ok", "pending task ran fresh: {resumed}");

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
            status: "ok",
            response: Some("STALE"),
            error: None,
            now: 1_001,
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
        assert_eq!(rows[0]["status"], "ok", "agent-changed task reruns: {out}");
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
        };
        repo.checkpoint_task(cp("a", "ok", Some("REPLAYED"), None, 1_001))
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
        assert_eq!(rows[0]["status"], "ok", "{out}");
        assert_eq!(
            rows[0]["response"].as_str(),
            Some("REPLAYED"),
            "ok checkpoint still replays: {out}"
        );
        assert_eq!(rows[1]["status"], "ok", "failed task reruns: {out}");
        assert_ne!(
            rows[1]["response"].as_str(),
            Some("boom"),
            "failed checkpoint must not replay: {out}"
        );
        assert_eq!(rows[2]["status"], "ok", "skipped task reruns: {out}");
        assert_eq!(
            rows[3]["status"], "ok",
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
            assert_eq!(row["status"], "ok", "slot {i} across chunks: {out}");
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
            status: "ok",
            response: Some("REPLAYED"),
            error: None,
            now: now - 120,
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
            repo.claim_plan(&plan_id, "other-run", now).await.unwrap(),
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
        assert_eq!(rows[0]["status"], "ok", "retry succeeds: {out}");
        assert_eq!(
            rows[1]["status"], "ok",
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
        assert!(rows.iter().all(|r| r["status"] == "ok"), "{out}");
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
            rows[1]["status"], "ok",
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
        assert_eq!(rows[1]["status"], "ok", "{out}");
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
}
