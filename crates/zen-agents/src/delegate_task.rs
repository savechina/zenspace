//! `delegate.task` — model-driven sub-agent delegation (006, US1/D1-D3).
//!
//! One tool, OpenCode `task.ts` shape: `{agent, prompt, description?}`.
//! Each invocation runs a REAL LLM sub-turn (identity + prompt assembly +
//! own tool grants + mini tool loop) and returns the sub-agent's final
//! response as the tool output. Depth-1 guard is by construction: sub-agent
//! grants are filtered of every `delegate.*` name, so a delegate can never
//! delegate.

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rig_compose::budget::{AtomicTokenBudget, TokenBudget};
use rig_compose::normalizer::dispatch_tool_invocations_with_hooks;
use rig_compose::tool::{Tool, ToolSchema};
use tracing::{info, warn};
use zen_core::types::{Sensitivity, SessionContext};
use zen_provider::DefaultRouter;

use crate::AgentContext;
use crate::agent_profile::Role;
use crate::delegate_tools;
use crate::executor::AgentExecutor;
use crate::registry::{AgentRegistry, DefaultAgentRegistry};
use crate::wiring::ZenWiring;
use crate::zen_agent::ZenAgent;

pub const DELEGATE_TOOL_NAME: &str = "delegate.task";

/// The delegation chain root — `delegate.task` is only injected into
/// Sisyphus's tool grants, so an unset task-local parent means the
/// orchestrator's own turn.
pub(crate) const DELEGATE_ROOT_PARENT: &str = "Sisyphus";

tokio::task_local! {
    /// Current delegation depth (0 = orchestrator turn). Unset → 0.
    static DELEGATE_DEPTH: Cell<u32>;
    /// Agent name that spawned the current delegation hop.
    static DELEGATE_PARENT: RefCell<Option<String>>;
}

/// Inner tool-loop cap for one delegated sub-turn. Sub-work is scoped by
/// definition — the parent keeps its own `[agentic.tool_loop]` budget.
const MAX_DELEGATE_ROUNDS: usize = 4;

/// Shared cell carrying the parent session's sensitivity into delegated
/// sub-turns (routing + hook gates). The orchestrator updates it where it
/// already calls `wiring.set_sensitivity`.
pub type SharedSensitivity = Arc<Mutex<Sensitivity>>;

pub struct DelegateTaskTool {
    wiring: Arc<ZenWiring>,
    executor: AgentExecutor,
    registry: DefaultAgentRegistry,
    overlay: Vec<String>,
    memvid_store: Option<rig_memvid::MemvidStore>,
    sensitivity: SharedSensitivity,
    token_budget: Arc<AtomicTokenBudget>,
    timeout: Duration,
    max_depth: u32,
    max_concurrent: u32,
}

impl DelegateTaskTool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        wiring: Arc<ZenWiring>,
        router: DefaultRouter,
        overlay: Vec<String>,
        memvid_store: Option<rig_memvid::MemvidStore>,
        sensitivity: SharedSensitivity,
        token_budget: Arc<AtomicTokenBudget>,
        timeout: Duration,
        max_depth: u32,
        max_concurrent: u32,
    ) -> Self {
        Self {
            wiring,
            executor: AgentExecutor::new(router),
            registry: DefaultAgentRegistry::new(),
            overlay,
            memvid_store,
            sensitivity,
            token_budget,
            timeout,
            max_depth,
            max_concurrent,
        }
    }

    /// 001 arch A.1 spawn-hierarchy rules: Orchestrator(L0) may spawn
    /// Planner/Specialist/Worker; Planner(L1) may spawn Specialist/Worker;
    /// Specialist/Worker(L2) are leaves.
    fn spawn_allowed(&self, parent: &str, child: &str) -> Result<(), String> {
        let role_of = |name: &str| {
            self.registry
                .find_by_name(name)
                .map(|p| p.role.clone())
                .map_err(|e| format!("no profile for agent \"{name}\": {e}"))
        };
        let parent_role = role_of(parent)?;
        let child_role = role_of(child)?;
        let allowed = match parent_role {
            Role::Orchestrator => child_role != Role::Orchestrator,
            Role::Planner => !matches!(child_role, Role::Orchestrator | Role::Planner),
            Role::Specialist | Role::Worker => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(format!(
                "spawn hierarchy violation: {parent_role} cannot spawn {child_role} (001 arch A.1)"
            ))
        }
    }

    /// Current hop depth + parent from the task-local chain (unset → root).
    fn chain_position() -> (u32, String) {
        let depth = DELEGATE_DEPTH.try_with(|c| c.get()).unwrap_or(0);
        let parent = DELEGATE_PARENT
            .try_with(|c| c.borrow().clone())
            .ok()
            .flatten()
            .unwrap_or_else(|| DELEGATE_ROOT_PARENT.to_string());
        (depth, parent)
    }

    /// Build the sub-agent exactly like `AgentOrchestrator::build_agent`.
    /// `delegate.*` grants survive only when the child sits below the
    /// configured depth cap (`max_depth`); at the cap the name filter is
    /// the hard stop (006 depth-1 behavior is the `max_depth = 1` case).
    /// `plan.execute` is stripped unconditionally: it is a Sisyphus-only
    /// tool and must never ride an overlay grant into a sub-agent.
    fn build_sub_agent(
        &self,
        agent_name: &str,
        allow_child_delegation: bool,
    ) -> anyhow::Result<ZenAgent> {
        let skills = delegate_tools::resolve_skill_ids_for_agent(agent_name);
        let mut tools: Vec<String> = delegate_tools::resolve_agent_tool_grants(
            agent_name,
            &self.overlay,
            &self.wiring.tools,
        )
        .into_iter()
        .collect();
        if !allow_child_delegation {
            tools.retain(|name| !name.starts_with("delegate."));
        }
        tools.retain(|name| name != crate::plan_task::PLAN_TOOL_NAME);

        let mut builder = ZenAgent::builder(agent_name);
        for skill_id in &skills {
            builder = builder.with_skill(skill_id.as_str());
        }
        for tool_id in &tools {
            builder = builder.with_tool(tool_id.as_str());
        }
        if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
            builder = builder.with_paths(paths);
        }
        if let Some(store) = self.memvid_store.clone() {
            builder = builder.with_memvid_store(store);
        }
        builder.build(&self.wiring, self.executor.router())
    }

    fn deadline_exceeded(deadline: Instant) -> Option<String> {
        if Instant::now() >= deadline {
            Some("delegate sub-turn exceeded its wall-clock budget; partial work returned".into())
        } else {
            None
        }
    }

    /// Parse the single-task form (`agent`+`prompt`) or the fan-out form
    /// (`tasks: [...]`). Mixed forms prefer `tasks`.
    fn parse_requests(args: &serde_json::Value) -> Result<Vec<DelegateRequest>, String> {
        if let Some(arr) = args.get("tasks").and_then(|v| v.as_array()) {
            let mut tasks = Vec::with_capacity(arr.len());
            for item in arr {
                let Some(agent) = item.get("agent").and_then(|v| v.as_str()) else {
                    return Err("each tasks[] item requires an \"agent\" string".to_string());
                };
                let Some(prompt) = item.get("prompt").and_then(|v| v.as_str()) else {
                    return Err("each tasks[] item requires a \"prompt\" string".to_string());
                };
                tasks.push(DelegateRequest {
                    agent: agent.to_string(),
                    prompt: prompt.to_string(),
                    description: item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    independent: item
                        .get("independent")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                });
            }
            if tasks.is_empty() {
                return Err("tasks[] must not be empty".to_string());
            }
            return Ok(tasks);
        }
        let agent = args
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or("delegate.task requires an \"agent\" string argument (or tasks[])")?;
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or("delegate.task requires a \"prompt\" string argument (or tasks[])")?;
        Ok(vec![DelegateRequest {
            agent: agent.to_string(),
            prompt: prompt.to_string(),
            description: args
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            independent: args
                .get("independent")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        }])
    }

    /// Advisory pre-spawn gates (Codex SPAWN_AGENT_CONTRACT adapted):
    /// independent = model-declared; consumer_decision = the parent can
    /// digest the batch width; bounded = self-contained prompt under the
    /// hard limit; worth_it = context spend below the advisory line.
    fn evaluate_gates(task: &DelegateRequest, batch_width: usize) -> GateReport {
        GateReport {
            independent: task.independent,
            consumer_decision: batch_width <= 4,
            bounded: task.prompt.len() <= BOUNDED_PROMPT_CHARS,
            worth_it: (task.prompt.len() / 4 + 1024) as u64 <= ADVISORY_TOKEN_LINE,
        }
    }

    /// Shared per-task gate path (006 D1 + T374 hard gates). Used by
    /// `invoke` and by `plan.execute` so both entry points enforce the
    /// SAME rejection rules: known agent, tier matrix, depth cap, and
    /// the bounded-prompt hard limit. Advisory gates (independent /
    /// consumer-decision / worth-it) stay caller-side.
    pub(crate) fn validate_task(
        &self,
        parent: &str,
        depth: u32,
        task: &DelegateRequest,
    ) -> Option<String> {
        if !delegate_tools::is_builtin_agent(&task.agent) {
            return Some(format!("unknown delegate agent \"{}\"", task.agent));
        }
        if let Err(violation) = self.spawn_allowed(parent, &task.agent) {
            return Some(violation);
        }
        let child_depth = depth + 1;
        if child_depth > self.max_depth {
            return Some(format!(
                "delegation depth cap reached ({depth}/{max}); returning to the parent loop",
                depth = depth,
                max = self.max_depth
            ));
        }
        if task.prompt.len() > BOUNDED_PROMPT_CHARS {
            return Some(
                "unbounded delegation rejected: prompt exceeds the bounded-task limit".to_string(),
            );
        }
        None
    }

    /// Current session sensitivity mirrored from the orchestrator — the
    /// plan-quality gate reuses it so plan reviews classify under the
    /// same policy as turn reviews.
    pub(crate) fn current_sensitivity(&self) -> Sensitivity {
        *self
            .sensitivity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One `loop.delegate.gates` line in `<logs>/audit.jsonl`.
    fn append_gates_audit(
        &self,
        parent: &str,
        depth: u32,
        tasks: &[DelegateRequest],
        reports: &[GateReport],
    ) {
        let Ok(paths) = zen_core::paths::ZenPaths::detect() else {
            return;
        };
        let entries: Vec<serde_json::Value> = tasks
            .iter()
            .zip(reports)
            .map(|(t, r)| {
                serde_json::json!({
                    "agent": t.agent,
                    "prompt_chars": t.prompt.len(),
                    "independent": r.independent,
                    "consumer_decision": r.consumer_decision,
                    "bounded": r.bounded,
                    "worth_it": r.worth_it,
                })
            })
            .collect();
        let entry = serde_json::json!({
            "kind": "loop.delegate.gates",
            "parent": parent,
            "depth": depth,
            "batch_width": tasks.len(),
            "gates": entries,
        });
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

    /// Execute one validated delegation request (tier/depth/bounded checks
    /// already passed in `invoke`). Always returns a readable Value.
    pub(crate) async fn run_single(
        &self,
        req: &DelegateRequest,
        depth: u32,
        parent: &str,
    ) -> serde_json::Value {
        let started = Instant::now();
        let agent_name = req.agent.as_str();
        let prompt = req.prompt.as_str();
        let child_depth = depth + 1;
        let allow_child_delegation = child_depth < self.max_depth;

        let Ok(agent) = self.build_sub_agent(agent_name, allow_child_delegation) else {
            return serde_json::json!({
                "error": format!("failed to build delegate agent \"{agent_name}\"")
            });
        };

        let Ok(profile) = self.registry.find_by_name(agent_name).cloned() else {
            return serde_json::json!({
                "error": format!("no profile registered for delegate agent \"{agent_name}\"")
            });
        };

        let sensitivity = *self
            .sensitivity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut session = SessionContext::new(agent_name.to_string(), String::new());
        session.sensitivity_policy = sensitivity;
        let mut context = AgentContext::new(profile.clone(), prompt.to_string(), session)
            .with_preferences(profile.llm_preferences.clone());
        context
            .metadata
            .insert("delegate_depth".to_string(), serde_json::json!(child_depth));
        context
            .metadata
            .insert("delegate_parent".to_string(), serde_json::json!(parent));

        // Budget: reserve a conservative estimate for the whole sub-turn;
        // an exhausted parent budget refuses the delegation up front.
        let estimated = (prompt.len() / 4 + 1024) as u64;
        let reservation = match self.token_budget.try_reserve_tokens(estimated).await {
            Ok(Some(reservation)) => reservation,
            Ok(None) => {
                return serde_json::json!({
                    "error": "token budget exhausted; delegation refused"
                });
            }
            Err(e) => {
                return serde_json::json!({
                    "error": format!("token budget reservation failed: {e}")
                });
            }
        };

        let (early_error, mut response, rounds, timed_out) = DELEGATE_DEPTH
            .scope(
                Cell::new(child_depth),
                DELEGATE_PARENT.scope(
                    RefCell::new(Some(agent_name.to_string())),
                    async {
        let manifest = agent.tool_manifest();
        let deadline = started + self.timeout;
        let mut feedback = String::new();
        let mut response = String::new();
        let mut rounds = 0usize;
        let mut timed_out = false;

        while rounds < MAX_DELEGATE_ROUNDS {
            if let Some(note) = Self::deadline_exceeded(deadline) {
                timed_out = true;
                warn!(agent = agent_name, rounds, "delegate sub-turn hit deadline");
                response.push_str(&format!("\n\n[{note}]"));
                break;
            }
            let Ok(execution) = self
                .executor
                .execute_round(&context, &agent, &manifest, &feedback)
            else {
                return (
                    Some(serde_json::json!({
                        "error": format!("delegate agent \"{agent_name}\" LLM round failed")
                    })),
                    response,
                    rounds,
                    timed_out,
                );
            };
            rounds += 1;
            response = execution.response;

            let (invocations, parse_errors) =
                crate::orchestrator::AgentOrchestrator::parse_tool_invocations_verbose(&response);
            if invocations.is_empty() {
                if parse_errors.is_empty() {
                    break;
                }
                feedback = format!(
                    "Your previous tool call block could not be parsed and was NOT executed:\n{}\nRe-emit exactly one valid ```json block with {{\"tool\": \"<name>\", \"args\": {{...}}}}, or answer directly.",
                    parse_errors.join("\n")
                );
                continue;
            }
            let hooks = self.wiring.dispatch_hooks();
            match dispatch_tool_invocations_with_hooks(agent.generic.tools(), &invocations, &hooks)
                .await
            {
                Ok(results) => {
                    feedback = crate::orchestrator::AgentOrchestrator::results_to_prompt(&results);
                }
                Err(e) => {
                    feedback = format!(
                        "tool dispatch blocked by sandbox: {e}\nAnswer the user with what you have."
                    );
                }
            }
        }
                        (None, response, rounds, timed_out)
                    },
                ),
            )
            .await;
        if let Some(error) = early_error {
            return error;
        }

        if rounds >= MAX_DELEGATE_ROUNDS && !timed_out {
            response.push_str("\n\n[delegate round cap reached; returning current answer]");
        }

        let actual = (prompt.len() / 4 + response.len() / 4) as u64;
        self.token_budget
            .record_usage(reservation, actual, actual)
            .await;

        let output = match req.description {
            Some(ref description) => serde_json::json!({
                "agent": agent_name,
                "response": response,
                "rounds": rounds,
                "duration_ms": started.elapsed().as_millis() as u64,
                "description": description,
            }),
            None => serde_json::json!({
                "agent": agent_name,
                "response": response,
                "rounds": rounds,
                "duration_ms": started.elapsed().as_millis() as u64,
            }),
        };
        info!(
            agent = agent_name,
            rounds,
            duration_ms = started.elapsed().as_millis() as u64,
            "delegate.task sub-turn completed"
        );
        output
    }
}

/// One delegation subtask — the fan-out unit (001 A.6 collect mode).
#[derive(Debug, Clone)]
pub struct DelegateRequest {
    pub agent: String,
    pub prompt: String,
    pub description: Option<String>,
    pub independent: bool,
}

/// Advisory pre-spawn gate verdicts (audited, T374).
#[derive(Debug, Clone, Copy)]
pub struct GateReport {
    pub independent: bool,
    pub consumer_decision: bool,
    pub bounded: bool,
    pub worth_it: bool,
}

/// Hard limit: a delegation prompt must be a self-contained bounded brief.
pub(crate) const BOUNDED_PROMPT_CHARS: usize = 32_000;
/// Advisory line above which a delegation is flagged not-worth-it.
const ADVISORY_TOKEN_LINE: u64 = 50_000;

#[async_trait::async_trait]
impl Tool for DelegateTaskTool {
    fn schema(&self) -> ToolSchema {
        let agents = delegate_tools::builtin_agent_names().join(", ");
        ToolSchema {
            name: DELEGATE_TOOL_NAME.to_string(),
            description: format!(
                "Delegate a focused subtask to a specialist sub-agent. The sub-agent runs \
                 in its own context with its own tool grants and returns its final answer \
                 as this tool's result. Use for research sweeps, deep analysis, plan \
                 drafting, or review passes — one focused subtask per call. \
                 Available agents: {agents}."
            ),
            args_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Specialist to run (see list in the tool description)"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Self-contained subtask brief with all context the sub-agent needs"
                    },
                    "description": {
                        "type": "string",
                        "description": "Short (3-5 words) label for this delegation"
                    },
                    "tasks": {
                        "type": "array",
                        "description": "Parallel fan-out (001 A.6 collect mode): run several independent subtasks concurrently. Each item: {\"agent\", \"prompt\", \"description?\", \"independent?\"}. Use for research sweeps / multi-file audits; results return as a batch.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "agent": {"type": "string"},
                                "prompt": {"type": "string"},
                                "description": {"type": "string"},
                                "independent": {"type": "boolean", "description": "true when this subtask needs no other subtask's output (pre-spawn gate)"}
                            },
                            "required": ["agent", "prompt"]
                        }
                    },
                    "independent": {
                        "type": "boolean",
                        "description": "Single-task form: true when the parent needs no intermediate state (default true)"
                    }
                },
                "required": ["agent", "prompt"]
            }),
            result_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {"type": "string"},
                    "response": {"type": "string"},
                    "rounds": {"type": "integer"},
                    "duration_ms": {"type": "integer"}
                }
            }),
        }
    }

    /// Always returns `Ok` — failures surface as a structured `error` output
    /// the model can read and react to, mirroring how sandbox-blocked
    /// dispatches are surfaced in the parent loop.
    async fn invoke(
        &self,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, rig_compose::registry::KernelError> {
        let tasks = match Self::parse_requests(&args) {
            Ok(tasks) => tasks,
            Err(e) => return Ok(serde_json::json!({ "error": e })),
        };

        let (depth, parent) = Self::chain_position();
        let rejected: Vec<Option<String>> = tasks
            .iter()
            .map(|task| self.validate_task(&parent, depth, task))
            .collect();

        // Codex-style pre-spawn gates (T374): bounded is a hard limit, the
        // rest are advisory heuristics logged to the audit trail.
        let reports: Vec<GateReport> = tasks
            .iter()
            .map(|t| Self::evaluate_gates(t, tasks.len()))
            .collect();
        self.append_gates_audit(&parent, depth, &tasks, &reports);

        // Slot-ordered assembly: results[i] always corresponds to tasks[i],
        // whether the task was rejected, unbounded, or executed.
        let mut slots: Vec<Option<serde_json::Value>> = Vec::with_capacity(tasks.len());
        let mut runnable: Vec<usize> = Vec::with_capacity(tasks.len());
        for (i, task) in tasks.iter().enumerate() {
            if let Some(error) = &rejected[i] {
                slots.push(Some(
                    serde_json::json!({ "agent": task.agent, "error": error }),
                ));
            } else if !reports[i].bounded {
                slots.push(Some(serde_json::json!({
                    "agent": task.agent,
                    "error": "unbounded delegation rejected: prompt exceeds the bounded-task limit"
                })));
            } else {
                slots.push(None);
                runnable.push(i);
            }
        }

        for chunk in runnable.chunks(self.max_concurrent as usize) {
            let futures: Vec<_> = chunk
                .iter()
                .map(|&i| self.run_single(&tasks[i], depth, &parent))
                .collect();
            let results = futures::future::join_all(futures).await;
            for (&i, outcome) in chunk.iter().zip(results) {
                slots[i] = Some(outcome);
            }
        }
        let results: Vec<serde_json::Value> = slots
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(
                    || serde_json::json!({ "error": "delegation produced no result" }),
                )
            })
            .collect();

        if tasks.len() == 1 {
            return Ok(results.into_iter().next().unwrap_or_else(
                || serde_json::json!({ "error": "delegation produced no result" }),
            ));
        }
        Ok(serde_json::json!({ "results": results }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_router() -> DefaultRouter {
        zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        })
    }

    fn probe_tool_schema(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: name.to_string(),
            args_schema: serde_json::json!({}),
            result_schema: serde_json::json!({}),
        }
    }

    struct ProbeTool(&'static str);

    #[async_trait::async_trait]
    impl Tool for ProbeTool {
        fn schema(&self) -> ToolSchema {
            probe_tool_schema(self.0)
        }
        async fn invoke(
            &self,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value, rig_compose::registry::KernelError> {
            Ok(serde_json::json!({ "ran": self.0 }))
        }
    }

    fn tool_with(
        wiring: Arc<ZenWiring>,
        overlay: Vec<String>,
        budget: Arc<AtomicTokenBudget>,
    ) -> DelegateTaskTool {
        tool_with_depth(wiring, overlay, budget, 1)
    }

    fn tool_with_depth(
        wiring: Arc<ZenWiring>,
        overlay: Vec<String>,
        budget: Arc<AtomicTokenBudget>,
        max_depth: u32,
    ) -> DelegateTaskTool {
        DelegateTaskTool::new(
            wiring,
            mock_router(),
            overlay,
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Public)),
            budget,
            Duration::from_secs(300),
            max_depth,
            4,
        )
    }

    #[tokio::test]
    async fn delegate_requires_agent_and_prompt_arguments() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let missing = tool.invoke(serde_json::json!({})).await.unwrap();
        assert!(
            missing["error"].as_str().unwrap().contains("agent"),
            "missing args must surface an agent diagnostic: {missing}"
        );
        let no_prompt = tool
            .invoke(serde_json::json!({ "agent": "Explore" }))
            .await
            .unwrap();
        assert!(
            no_prompt["error"].as_str().unwrap().contains("prompt"),
            "missing prompt must surface a prompt diagnostic: {no_prompt}"
        );
    }

    #[tokio::test]
    async fn delegate_rejects_unknown_agent() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({ "agent": "Nope", "prompt": "x" }))
            .await
            .unwrap();
        assert!(out["error"].as_str().unwrap().contains("unknown delegate"));
    }

    #[tokio::test]
    async fn delegate_sub_turn_runs_real_llm_round() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore",
                "prompt": "summarize the zen architecture",
                "description": "arch summary"
            }))
            .await
            .unwrap();
        assert_eq!(out["agent"], "Explore");
        let response = out["response"].as_str().unwrap();
        assert!(!response.is_empty(), "sub-turn must return the model reply");
        assert!(
            out["rounds"].as_u64().unwrap() >= 1,
            "at least one LLM round ran: {out}"
        );
        assert!(
            out["duration_ms"].as_u64().is_some(),
            "duration metadata present: {out}"
        );
    }

    #[tokio::test]
    async fn depth_one_guard_strips_delegate_tools_from_sub_agent() {
        let wiring = Arc::new(ZenWiring::new());
        wiring.tools.register(Arc::new(ProbeTool("delegate.fake")));

        // Given an overlay granting delegate.*: the sub-agent grant filter
        // must remove every delegate-prefixed name (spec D3).
        let tool = tool_with(
            Arc::clone(&wiring),
            vec!["delegate.*".to_string()],
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let sub = tool.build_sub_agent("Explore", false).unwrap();
        assert!(
            sub.generic.tools().get("delegate.fake").is_err(),
            "delegate.* grants must not survive the depth-1 filter"
        );

        assert!(
            wiring.tools.get("delegate.fake").is_ok(),
            "the probe tool must be registry-reachable for the grant pass to be meaningful"
        );
    }

    #[tokio::test]
    async fn budget_exhausted_delegation_is_refused_with_error_output() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(0)),
        );
        let out = tool
            .invoke(serde_json::json!({ "agent": "Explore", "prompt": "hello" }))
            .await
            .unwrap();
        assert!(
            out["error"].as_str().unwrap().contains("token budget"),
            "exhausted budget must refuse with a model-readable error: {out}"
        );
    }

    #[test]
    fn spawn_hierarchy_enforces_tier_rules() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        // Orchestrator → Planner/Worker ok; Orchestrator → Orchestrator banned.
        assert!(tool.spawn_allowed("Sisyphus", "Prometheus").is_ok());
        assert!(tool.spawn_allowed("Sisyphus", "Junior").is_ok());
        assert!(tool.spawn_allowed("Sisyphus", "Sisyphus").is_err());
        // Planner → Specialist/Worker ok; Planner → Planner/Orchestrator banned.
        assert!(tool.spawn_allowed("Prometheus", "Explore").is_ok());
        assert!(tool.spawn_allowed("Prometheus", "Momus").is_err());
        assert!(tool.spawn_allowed("Prometheus", "Sisyphus").is_err());
        // Leaves spawn nothing.
        assert!(tool.spawn_allowed("Explore", "Junior").is_err());
        assert!(tool.spawn_allowed("Junior", "Explore").is_err());
        // Unknown agents are a profile error, not a panic.
        assert!(tool.spawn_allowed("Nope", "Explore").is_err());
    }

    #[tokio::test]
    async fn depth_cap_returns_structured_error_at_max() {
        let tool = tool_with_depth(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
            1,
        );
        // Root hop (depth 0 → child 1) is allowed...
        let grant = DELEGATE_DEPTH.scope(Cell::new(1), async {
            // ...but at depth 1 with max_depth=1 the cap rejects before build.
            tool.invoke(serde_json::json!({ "agent": "Explore", "prompt": "x" }))
                .await
                .unwrap()
        });
        let out = grant.await;
        assert!(
            out["error"].as_str().unwrap().contains("depth cap"),
            "depth-capped invocation must surface a model-readable error: {out}"
        );
    }

    #[tokio::test]
    async fn sub_agent_keeps_delegate_grants_below_depth_cap() {
        let wiring = Arc::new(ZenWiring::new());
        wiring.tools.register(Arc::new(ProbeTool("delegate.fake")));
        let tool = tool_with_depth(
            Arc::clone(&wiring),
            vec!["delegate.*".to_string()],
            Arc::new(AtomicTokenBudget::new(100_000)),
            3,
        );
        let sub = tool.build_sub_agent("Explore", true).unwrap();
        assert!(
            sub.generic.tools().get("delegate.fake").is_ok(),
            "below the depth cap the child must keep delegation grants"
        );
        let capped = tool.build_sub_agent("Explore", false).unwrap();
        assert!(
            capped.generic.tools().get("delegate.fake").is_err(),
            "at the depth cap the name filter is the hard stop"
        );
    }

    #[tokio::test]
    async fn multi_task_fanout_returns_batch_results() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "agent": "Explore", "prompt": "sweep the docs", "description": "sweep" },
                    { "agent": "Librarian", "prompt": "organize the index", "independent": true }
                ]
            }))
            .await
            .unwrap();
        let results = out["results"].as_array().expect("batch shape: {out}");
        assert_eq!(results.len(), 2, "both subtasks return: {out}");
        assert_eq!(results[0]["agent"], "Explore");
        assert_eq!(results[1]["agent"], "Librarian");
        assert!(
            results.iter().all(|r| r["response"].as_str().is_some()),
            "each result carries the sub-agent reply: {out}"
        );
    }

    #[test]
    fn evaluate_gates_flags_unbounded_and_oversized_batches() {
        let small = DelegateRequest {
            agent: "Explore".into(),
            prompt: "compact brief".into(),
            description: None,
            independent: true,
        };
        let gates = DelegateTaskTool::evaluate_gates(&small, 2);
        assert!(gates.bounded && gates.worth_it && gates.independent && gates.consumer_decision);

        let huge = DelegateRequest {
            prompt: "x".repeat(BOUNDED_PROMPT_CHARS + 1),
            ..small.clone()
        };
        assert!(
            !DelegateTaskTool::evaluate_gates(&huge, 2).bounded,
            "over-limit prompts must fail the bounded gate"
        );
        assert!(
            !DelegateTaskTool::evaluate_gates(&small, 6).consumer_decision,
            "batch width above 4 fails the consumer-decision gate"
        );
        let not_worth = DelegateRequest {
            independent: false,
            ..small
        };
        assert!(
            !DelegateTaskTool::evaluate_gates(&not_worth, 2).independent,
            "model-declared dependency must surface in the gate report"
        );
    }

    #[tokio::test]
    async fn invoke_rejects_unbounded_task_with_error_output() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore",
                "prompt": "y".repeat(BOUNDED_PROMPT_CHARS + 1)
            }))
            .await
            .unwrap();
        assert!(
            out["error"].as_str().unwrap().contains("unbounded"),
            "bounded-gate violation must surface a readable error: {out}"
        );
    }

    #[tokio::test]
    async fn mixed_batch_results_stay_in_task_order() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({
                "tasks": [
                    { "agent": "Explore", "prompt": "first, valid" },
                    { "agent": "Nope", "prompt": "second, unknown agent" },
                    { "agent": "Librarian", "prompt": "third, valid" }
                ]
            }))
            .await
            .unwrap();
        let results = out["results"].as_array().expect("batch shape: {out}");
        assert_eq!(results.len(), 3, "{out}");
        assert_eq!(results[0]["agent"], "Explore", "slot 0 = task 0: {out}");
        assert!(
            results[1]["error"]
                .as_str()
                .unwrap()
                .contains("unknown delegate"),
            "slot 1 = task 1 rejection: {out}"
        );
        assert_eq!(results[2]["agent"], "Librarian", "slot 2 = task 2: {out}");
    }

    #[tokio::test]
    async fn build_sub_agent_strips_plan_execute_tool() {
        let wiring = Arc::new(ZenWiring::new());
        wiring
            .tools
            .register(Arc::new(ProbeTool(crate::plan_task::PLAN_TOOL_NAME)));
        let tool = tool_with(
            Arc::clone(&wiring),
            vec![crate::plan_task::PLAN_TOOL_NAME.to_string()],
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let sub = tool.build_sub_agent("Explore", true).unwrap();
        assert!(
            sub.generic
                .tools()
                .get(crate::plan_task::PLAN_TOOL_NAME)
                .is_err(),
            "plan.execute must never ride a grant into a sub-agent, even below the depth cap"
        );
    }

    fn tool_with_concurrency(
        wiring: Arc<ZenWiring>,
        overlay: Vec<String>,
        budget: Arc<AtomicTokenBudget>,
        max_depth: u32,
        max_concurrent: u32,
    ) -> DelegateTaskTool {
        DelegateTaskTool::new(
            wiring,
            mock_router(),
            overlay,
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Public)),
            budget,
            Duration::from_secs(300),
            max_depth,
            max_concurrent,
        )
    }

    #[tokio::test]
    async fn fanout_parse_negatives_surface_readable_errors() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let empty = tool
            .invoke(serde_json::json!({ "tasks": [] }))
            .await
            .unwrap();
        assert!(
            empty["error"].as_str().unwrap().contains("empty"),
            "empty tasks[] must surface an empty diagnostic: {empty}"
        );
        let missing_prompt = tool
            .invoke(serde_json::json!({ "tasks": [{"agent": "Explore"}] }))
            .await
            .unwrap();
        assert!(
            missing_prompt["error"].as_str().unwrap().contains("prompt"),
            "missing prompt must surface a prompt diagnostic: {missing_prompt}"
        );
        let missing_agent = tool
            .invoke(serde_json::json!({ "tasks": [{"prompt": "x"}] }))
            .await
            .unwrap();
        assert!(
            missing_agent["error"].as_str().unwrap().contains("agent"),
            "missing agent must surface an agent diagnostic: {missing_agent}"
        );
    }

    #[tokio::test]
    async fn specialist_parent_cannot_spawn_via_scoped_chain() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = DELEGATE_DEPTH
            .scope(
                Cell::new(0),
                DELEGATE_PARENT.scope(RefCell::new(Some("Explore".to_string())), async {
                    tool.invoke(serde_json::json!({ "agent": "Junior", "prompt": "x" }))
                        .await
                        .unwrap()
                }),
            )
            .await;
        assert!(
            out["error"].as_str().unwrap().contains("spawn hierarchy"),
            "Explore (Specialist) must not spawn Junior (Worker): {out}"
        );
    }

    #[tokio::test]
    async fn max_concurrent_chunks_preserve_slot_order() {
        let tool = tool_with_concurrency(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
            1,
            2,
        );
        let agents = ["Explore", "Librarian", "Explore", "Librarian", "Explore"];
        let prompts: Vec<String> = (0..5).map(|i| format!("task-{i}")).collect();
        let tasks: Vec<serde_json::Value> = agents
            .iter()
            .zip(&prompts)
            .map(|(a, p)| serde_json::json!({ "agent": a, "prompt": p }))
            .collect();
        let out = tool
            .invoke(serde_json::json!({ "tasks": tasks }))
            .await
            .unwrap();
        let results = out["results"].as_array().expect("batch shape: {out}");
        assert_eq!(results.len(), 5, "all five subtasks return: {out}");
        for (i, expected_agent) in agents.iter().enumerate() {
            assert_eq!(
                results[i]["agent"].as_str().unwrap(),
                *expected_agent,
                "slot {i} must match task agent: {out}"
            );
        }
        assert!(
            results
                .iter()
                .all(|r| r["response"].as_str().is_some_and(|s| !s.is_empty())),
            "every result must carry a non-empty response: {out}"
        );
    }
}
