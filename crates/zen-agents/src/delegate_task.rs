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
use zen_core::types::{RetrievedNote, Sensitivity, SessionContext};
use zen_provider::DefaultRouter;

use crate::AgentContext;
use crate::agent_profile::Role;
use crate::delegate_tools;
use crate::executor::AgentExecutor;
use crate::registry::{AgentRegistry, DefaultAgentRegistry};
use crate::skill_hit_router::SkillHitRouter;
use crate::wiring::ZenWiring;
use crate::zen_agent::ZenAgent;
use zen_memory::memvid_store::MemvidStore;

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

/// Shared cell carrying the parent turn's retrieved M1 knowledge block into
/// delegated sub-turns (Phase 31 T215, design F2-D8 — exact
/// [`SharedSensitivity`] pattern): the orchestrator refreshes it where it
/// already calls `propagate_sensitivity`, and `compose_sub_session` copies a
/// depth-budgeted truncation into the sub-agent's `SessionContext`. Same
/// last-writer-wins mirror semantics as the sensitivity cell.
pub type SharedKnowledge = Arc<Mutex<Vec<RetrievedNote>>>;

/// One sub-agent's inherited context (Phase 31 T215, design F3-D1). The
/// single contract carried across the delegation seam — prompt-string
/// concatenation was rejected there: it loses sensitivity tagging and the
/// FR-034 reward bookkeeping that reads `session.knowledge`.
pub(crate) struct SubAgentContext {
    /// Inherited knowledge notes (already depth-budget truncated).
    pub knowledge: Vec<RetrievedNote>,
    /// Skill names whose prompts were injected into the sub-session.
    pub skill_hits: Vec<String>,
    /// Char budget the knowledge block was truncated to (0 = gate closed).
    pub budget_chars: usize,
}

impl SubAgentContext {
    /// Total inherited content chars — the reservation-estimate term
    /// (design F2-D10) and the `inherited_chars` audit field.
    fn knowledge_chars(&self) -> usize {
        self.knowledge
            .iter()
            .map(|n| n.content.chars().count())
            .sum()
    }
}

/// Effective inheritance budget when `[agentic.delegate]
/// context_inheritance` is OPEN — the base is `[agentic.orchestrator]
/// knowledge_budget_chars` (design F2-D8: sub-agents inherit the parent's
/// block under the same budget family). `None` = gate CLOSED (default) or
/// config-load failure: the failure direction is closed (T206 "absent =
/// closed" — a broken config must never silently start copying parent
/// context into sub-agents).
fn inheritance_budget_chars() -> Option<usize> {
    match zen_core::config::load_config() {
        Ok(config) => config
            .agentic
            .delegate
            .context_inheritance_or_default()
            .then(|| {
                config
                    .agentic
                    .orchestrator
                    .knowledge_budget_chars_or_default()
            }),
        Err(e) => {
            warn!(
                error = %e,
                "config load failed; delegate context inheritance stays closed"
            );
            None
        }
    }
}

pub struct DelegateTaskTool {
    wiring: Arc<ZenWiring>,
    executor: AgentExecutor,
    registry: DefaultAgentRegistry,
    overlay: Vec<String>,
    memvid_store: Option<MemvidStore>,
    sensitivity: SharedSensitivity,
    knowledge: SharedKnowledge,
    token_budget: Arc<AtomicTokenBudget>,
    timeout: Duration,
    max_depth: u32,
    max_concurrent: u32,
    skill_router: SkillHitRouter,
}

impl DelegateTaskTool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        wiring: Arc<ZenWiring>,
        router: DefaultRouter,
        overlay: Vec<String>,
        memvid_store: Option<MemvidStore>,
        sensitivity: SharedSensitivity,
        knowledge: SharedKnowledge,
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
            knowledge,
            token_budget,
            timeout,
            max_depth,
            max_concurrent,
            skill_router: SkillHitRouter::new(),
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
    /// (`tasks: [...]`). Mixed forms prefer `tasks`. Fan-out batches are
    /// hard-capped at [`FANOUT_MAX_TASKS`] (T216).
    fn parse_requests(args: &serde_json::Value) -> Result<Vec<DelegateRequest>, String> {
        if let Some(arr) = args.get("tasks").and_then(|v| v.as_array()) {
            if arr.len() > FANOUT_MAX_TASKS {
                return Err(format!(
                    "fan-out batch of {} tasks exceeds the hard cap of {FANOUT_MAX_TASKS}; split the work into batches of at most {FANOUT_MAX_TASKS} independent tasks",
                    arr.len()
                ));
            }
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
    ///
    /// T215: per-task entries gain ADDITIVE `{inherited_notes,
    /// inherited_chars, skill_hits}` fields (design Feature-2d audit row);
    /// existing consumers keep parsing (all pre-T215 fields unchanged).
    /// Entries for tasks that never composed (rejected/unbounded) carry
    /// zeros; gate-closed compositions also carry zeros — the field shape
    /// is unconditional so consumers never branch on presence.
    fn append_gates_audit(
        &self,
        parent: &str,
        depth: u32,
        tasks: &[DelegateRequest],
        reports: &[GateReport],
        composed: &[Option<(SessionContext, SubAgentContext)>],
    ) {
        let Ok(paths) = zen_core::paths::ZenPaths::detect() else {
            return;
        };
        let entries: Vec<serde_json::Value> = tasks
            .iter()
            .zip(reports)
            .zip(composed)
            .map(|((t, r), slot)| {
                let ctx = slot.as_ref().map(|(_, c)| c);
                serde_json::json!({
                    "agent": t.agent,
                    "prompt_chars": t.prompt.len(),
                    "independent": r.independent,
                    "consumer_decision": r.consumer_decision,
                    "bounded": r.bounded,
                    "worth_it": r.worth_it,
                    "inherited_notes": ctx.map(|c| c.knowledge.len()).unwrap_or(0),
                    "inherited_chars": ctx.map(|c| c.knowledge_chars()).unwrap_or(0),
                    "skill_hits": ctx.map(|c| c.skill_hits.len()).unwrap_or(0),
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

    /// The SINGLE sub-session composition seam (Phase 31 T215, design
    /// F3-D1): used by `run_single`, therefore automatically by
    /// `plan.execute` AND resume (shared `Arc<DelegateTaskTool>`; replayed
    /// checkpoints keep their recorded outputs verbatim — they ran under
    /// their time's context, design F3-D6).
    ///
    /// Contract:
    /// - Gate CLOSED (`[agentic.delegate] context_inheritance=false`, the
    ///   default): output is byte-identical to the pre-T215 inline
    ///   construction — empty `SessionContext` + mirrored sensitivity, no
    ///   skill routing (pinned by
    ///   `compose_sub_session_gate_off_matches_legacy_shape`).
    /// - Gate OPEN: the parent's already-retrieved knowledge block is
    ///   inherited (NO fresh retrieval per subtask, design F2-D8),
    ///   truncated to the depth-halved budget `base >> depth` clamped to
    ///   `KNOWLEDGE_BUDGET_CHARS_MIN` (structural halving; `max_depth ≤ 3`
    ///   bounds the shift, `validate_task` enforces the cap before every
    ///   call — the `min` keeps the shift total anyway), plus skill-hit
    ///   injection on the sub-prompt through the orchestrator's shared
    ///   helper (design F2-D9, no logic duplication).
    fn compose_sub_session(
        &self,
        agent_name: &str,
        prompt: &str,
        depth: u32,
    ) -> (SessionContext, SubAgentContext) {
        let sensitivity = *self
            .sensitivity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut session = SessionContext::new(agent_name.to_string(), String::new());
        session.sensitivity_policy = sensitivity;
        let Some(base_budget) = inheritance_budget_chars() else {
            return (
                session,
                SubAgentContext {
                    knowledge: Vec::new(),
                    skill_hits: Vec::new(),
                    budget_chars: 0,
                },
            );
        };
        let budget_chars =
            (base_budget >> depth.min(31)).max(zen_core::config::KNOWLEDGE_BUDGET_CHARS_MIN);
        let snapshot = self
            .knowledge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let (knowledge, _) = crate::orchestrator::truncate_notes_to_budget(snapshot, budget_chars);
        session.knowledge = knowledge.clone();
        let skill_hits =
            crate::orchestrator::inject_skill_hits_into(&self.skill_router, &mut session, prompt)
                .map(|hit| hit.skill)
                .into_iter()
                .collect();
        (
            session,
            SubAgentContext {
                knowledge,
                skill_hits,
                budget_chars,
            },
        )
    }

    /// Execute one validated delegation request (tier/depth/bounded checks
    /// already passed in `invoke`). Always returns a readable Value.
    pub(crate) async fn run_single(
        &self,
        req: &DelegateRequest,
        depth: u32,
        parent: &str,
    ) -> serde_json::Value {
        let (session, sub) = self.compose_sub_session(&req.agent, &req.prompt, depth);
        self.run_single_composed(req, depth, parent, session, sub)
            .await
    }

    /// `run_single` with a pre-composed sub-session — the `invoke` fan-out
    /// path composes BEFORE `append_gates_audit` so the audit line carries
    /// the real per-task `{inherited_notes, inherited_chars, skill_hits}`
    /// values; composition is deterministic, so pre-composing here and
    /// inside `run_single` (the plan.execute path) cannot drift.
    pub(crate) async fn run_single_composed(
        &self,
        req: &DelegateRequest,
        depth: u32,
        parent: &str,
        session: SessionContext,
        sub: SubAgentContext,
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
        // T215 (design F2-D10): inherited knowledge rides the SessionContext
        // (prompt assembly), never the prompt string — the 32k bounded gate
        // is untouched; the estimate additively includes knowledge_chars/4
        // (same bytes→tokens divisor as the prompt term) so reservations
        // stay honest.
        let inherited_chars = sub.knowledge_chars();
        let estimated = (prompt.len() / 4 + 1024 + inherited_chars / 4) as u64;
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
            knowledge_inherited = !sub.knowledge.is_empty(),
            inherited_chars,
            inherited_budget_chars = sub.budget_chars,
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

/// Fan-out hard cap (T216, owner decision D-H): 8 is the RECORDED spec
/// value (AGENTS.md tool inventory: "≤8/batch") that the design audit found
/// unenforced — enforcing it is reuse, not invention. Rejection is a
/// structured model-readable `Ok(json!({"error": ...}))` via `parse_requests`
/// (this file's existing structured-error style). `plan.execute` keeps its
/// own MAX_PLAN_TASKS=12 cap: plan layers batch through `run_single`
/// directly, never through `parse_requests`.
pub(crate) const FANOUT_MAX_TASKS: usize = 8;

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

        // T215: compose the runnable sub-sessions BEFORE the audit line so
        // `loop.delegate.gates` carries the real per-task inheritance stats;
        // the audit still precedes every sub-turn spawn (T374 contract).
        let mut composed: Vec<Option<(SessionContext, SubAgentContext)>> =
            (0..tasks.len()).map(|_| None).collect();
        for &i in &runnable {
            composed[i] = Some(self.compose_sub_session(&tasks[i].agent, &tasks[i].prompt, depth));
        }
        self.append_gates_audit(&parent, depth, &tasks, &reports, &composed);

        for chunk in runnable.chunks(self.max_concurrent as usize) {
            let futures: Vec<_> = chunk
                .iter()
                .map(|&i| {
                    let (session, sub) = composed[i]
                        .take()
                        .expect("runnable slots are composed above");
                    self.run_single_composed(&tasks[i], depth, &parent, session, sub)
                })
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
            Arc::new(std::sync::Mutex::new(Vec::new())),
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
            Arc::new(std::sync::Mutex::new(Vec::new())),
            budget,
            Duration::from_secs(300),
            max_depth,
            max_concurrent,
        )
    }

    /// T216 (owner decision D-H): 9 → structured rejection naming the
    /// recorded cap; 8 → accepted; the cap does not disturb mixed
    /// reject/run slot ordering.
    #[tokio::test]
    async fn fanout_hard_cap_rejects_nine_and_accepts_eight() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(1_000_000)),
        );
        let batch = |n: usize, agent_at: Option<(usize, &str)>| -> serde_json::Value {
            serde_json::json!({
                "tasks": (0..n)
                    .map(|i| serde_json::json!({
                        "agent": agent_at.filter(|(j, _)| *j == i).map(|(_, a)| a).unwrap_or("Explore"),
                        "prompt": format!("capped task {i}")
                    }))
                    .collect::<Vec<_>>()
            })
        };

        let over = tool
            .invoke(batch(FANOUT_MAX_TASKS + 1, None))
            .await
            .unwrap();
        let error = over["error"].as_str().expect("structured rejection");
        assert!(
            error.contains("hard cap of 8"),
            "the rejection must name the recorded cap: {error}"
        );
        assert!(
            error.contains("9 tasks"),
            "the rejection must state the offending width: {error}"
        );

        let at_cap = tool.invoke(batch(FANOUT_MAX_TASKS, None)).await.unwrap();
        let results = at_cap["results"].as_array().expect("batch shape");
        assert_eq!(
            results.len(),
            FANOUT_MAX_TASKS,
            "exactly at the cap: accepted"
        );
        assert!(
            results.iter().all(|r| r["response"].as_str().is_some()),
            "every capped task ran: {at_cap}"
        );

        // Cap-boundary batch with one rejection inside: slot ordering holds.
        let mixed = tool
            .invoke(batch(FANOUT_MAX_TASKS, Some((7, "Nope"))))
            .await
            .unwrap();
        let results = mixed["results"].as_array().expect("batch shape");
        assert_eq!(results.len(), FANOUT_MAX_TASKS);
        for (i, r) in results.iter().enumerate().take(7) {
            assert_eq!(r["agent"].as_str(), Some("Explore"), "slot {i}");
            assert!(r["response"].as_str().is_some(), "slot {i} ran");
        }
        assert!(
            results[7]["error"]
                .as_str()
                .unwrap()
                .contains("unknown delegate"),
            "slot 7 keeps its rejection: {mixed}"
        );
    }

    /// T217 (design F3-D4 pin, no production change): the early LLM-round
    /// failure path returns BEFORE `record_usage`, dropping the reservation;
    /// rig-compose's `TokenReservation` refunds on `Drop`
    /// (rig-compose-0.5.0/src/budget.rs:253-259) — so a failed delegation
    /// must never consume the parent budget.
    #[tokio::test]
    async fn early_llm_round_failure_refunds_reservation() {
        // Budget-level pin of the Drop-refund property itself: consumed only
        // moves through record_usage; a dropped reservation restores
        // availability exactly.
        let budget = Arc::new(AtomicTokenBudget::new(100_000));
        {
            let reservation = budget.try_reserve_tokens(2025).await.unwrap().unwrap();
            assert_eq!(budget.available(), 100_000 - 2025);
            drop(reservation);
        }
        assert_eq!(budget.available(), 100_000, "Drop refunds the estimate");
        assert_eq!(budget.tokens_consumed().await, 0);

        // Path-level pin: drive run_single's early_error branch with a
        // deterministic non-retryable LLM failure. The 401 responder puts
        // "401" in the error text ⇒ ErrorCategory::ClientError ⇒
        // non-retryable ⇒ execute_round Errs on the FIRST attempt
        // (executor.rs:370-380) — no retry delays, no mock fallback.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // Drain the FULL request before responding: closing mid-body
                // (prompts are ~200KB) would surface as a retryable
                // connection error instead of the intended 401.
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                let mut buf = [0u8; 8192];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 18\r\nconnection: close\r\n\r\n{\"error\":\"401-no\"}",
                );
            }
        });

        // NO_PROXY: reqwest honors proxy env vars, and a machine-level HTTP
        // proxy would swallow the loopback request (returning its OWN status
        // instead of the 401) — the same loopback-no-proxy discipline the
        // OllamaProvider applies.
        unsafe {
            std::env::set_var("FAIL401_API_KEY", "test-key");
            std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
            std::env::set_var("no_proxy", "127.0.0.1,localhost");
        }
        let config = zen_core::config::ZenConfig {
            default_provider: Some("fail401".to_string()),
            providers: std::collections::HashMap::from([(
                "fail401".to_string(),
                zen_core::config::ProviderConfig {
                    provider_type: Some("openai-compatible".to_string()),
                    base_url: Some(format!("http://127.0.0.1:{port}")),
                    default_model: Some("test-model".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let tool = DelegateTaskTool::new(
            Arc::new(ZenWiring::new()),
            DefaultRouter::from_agentic(&config),
            Vec::new(),
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Public)),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::clone(&budget),
            Duration::from_secs(300),
            1,
            4,
        );

        let out = tool
            .invoke(
                serde_json::json!({ "agent": "Explore", "prompt": "trigger the failing round" }),
            )
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .unwrap_or_default()
                .contains("LLM round failed"),
            "the early-error path must surface its structured error: {out}"
        );
        assert_eq!(
            budget.tokens_consumed().await,
            0,
            "the dropped reservation must refund — a failed delegation never consumes budget"
        );
        assert_eq!(budget.available(), budget.capacity());
        unsafe {
            std::env::remove_var("FAIL401_API_KEY");
            std::env::remove_var("NO_PROXY");
            std::env::remove_var("no_proxy");
        }
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

    // ── Phase 31 T215: sub-agent context inheritance ──────────────────

    fn note(content: &str) -> RetrievedNote {
        RetrievedNote {
            path: "vault/wiki/notions/n.md".to_string(),
            content: content.to_string(),
            sensitivity: Sensitivity::Private,
            relevance: 1.0,
        }
    }

    fn tool_with_knowledge(
        budget: Arc<AtomicTokenBudget>,
        knowledge: SharedKnowledge,
    ) -> DelegateTaskTool {
        // Public sensitivity in the async gate-on test: Confidential would
        // trip the PRE-EXISTING sync-route hazard in async contexts
        // (enforce_sensitivity → is_local_llm_available → reqwest-blocking
        // health_check; the D1 hazard class), orthogonal to inheritance.
        // Confidential mirroring is pinned by the sync gate-off test below
        // (compose_sub_session never routes, so no hazard there).
        DelegateTaskTool::new(
            Arc::new(ZenWiring::new()),
            mock_router(),
            Vec::new(),
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Public)),
            knowledge,
            budget,
            Duration::from_secs(300),
            3,
            4,
        )
    }

    fn set_inheritance_env(home: Option<&std::path::Path>, inherit: Option<&str>) {
        match home {
            Some(h) => unsafe { std::env::set_var("ZEN_HOME", h) },
            None => unsafe { std::env::remove_var("ZEN_HOME") },
        }
        match inherit {
            Some(v) => unsafe { std::env::set_var("ZEN_DELEGATE_CONTEXT_INHERITANCE", v) },
            None => unsafe { std::env::remove_var("ZEN_DELEGATE_CONTEXT_INHERITANCE") },
        }
        zen_core::config::invalidate_config_cache();
    }

    /// Gate-CLOSED pin (default config): `compose_sub_session` reproduces
    /// the pre-T215 inline construction — empty knowledge, mirrored
    /// sensitivity, zeroed context — and this test FAILS if the
    /// `context_inheritance` default ever flips open.
    #[test]
    fn compose_sub_session_gate_off_matches_legacy_shape() {
        set_inheritance_env(None, None);
        let knowledge: SharedKnowledge = Arc::new(std::sync::Mutex::new(vec![note(
            "parent knowledge that must NOT leak while the gate is closed",
        )]));
        // Confidential cell: compose_sub_session mirrors sensitivity without
        // routing, so the D1 sync-route hazard cannot fire in this sync test.
        let tool = DelegateTaskTool::new(
            Arc::new(ZenWiring::new()),
            mock_router(),
            Vec::new(),
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Confidential)),
            knowledge,
            Arc::new(AtomicTokenBudget::new(100_000)),
            Duration::from_secs(300),
            3,
            4,
        );
        let (session, sub) = tool.compose_sub_session("Explore", "any subtask prompt", 0);
        assert!(
            session.knowledge.is_empty(),
            "gate-off must keep today's empty SessionContext byte-identical"
        );
        assert_eq!(
            session.sensitivity_policy,
            Sensitivity::Confidential,
            "sensitivity mirroring is unchanged by the gate"
        );
        assert_eq!(session.agent_name, "Explore");
        assert!(sub.knowledge.is_empty());
        assert!(sub.skill_hits.is_empty());
        assert_eq!(sub.budget_chars, 0);
    }

    /// Env-dependent phases in ONE test (skill_hit_contract.rs precedent):
    /// gate ON ⇒ depth-halved inheritance + skill-hit injection + the
    /// knowledge term in the reservation estimate + additive audit fields.
    #[tokio::test]
    async fn context_inheritance_gate_on_halves_budget_and_audits() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        set_inheritance_env(Some(dir.path()), Some("1"));

        // Skill fixture: containment match (score 1.0 ≥ 0.72 contract).
        let skills = dir.path().join("skills");
        std::fs::create_dir_all(&skills).expect("skills dir");
        std::fs::write(
            skills.join("zen-probe.md"),
            "---\nname: zen-probe\ndescription: probe\ntriggers: [zen inheritance probe]\n---\n# zen-probe\nDo the probe.",
        )
        .expect("write skill");

        let knowledge: SharedKnowledge = Arc::new(std::sync::Mutex::new(vec![
            note(&"a".repeat(3000)),
            note(&"b".repeat(3000)),
        ]));

        // Phase 1 — depth-halved budgets (base 4000 = config default).
        let tool = tool_with_knowledge(
            Arc::new(AtomicTokenBudget::new(100_000)),
            Arc::clone(&knowledge),
        );
        let (session_d0, sub_d0) =
            tool.compose_sub_session("Explore", "plain subtask without a match", 0);
        assert_eq!(sub_d0.budget_chars, 4000);
        assert_eq!(sub_d0.knowledge.len(), 2);
        assert_eq!(sub_d0.knowledge_chars(), 4000, "3000 + truncated 1000");
        assert_eq!(session_d0.knowledge.len(), 2);
        assert!(sub_d0.skill_hits.is_empty(), "no trigger ⇒ no skill hit");

        let (_session_d1, sub_d1) =
            tool.compose_sub_session("Explore", "plain subtask without a match", 1);
        assert_eq!(sub_d1.budget_chars, 2000, "budget halves per depth level");
        assert_eq!(sub_d1.knowledge.len(), 1);
        assert_eq!(sub_d1.knowledge_chars(), 2000);

        let (_session_d3, sub_d3) =
            tool.compose_sub_session("Explore", "plain subtask without a match", 3);
        assert_eq!(
            sub_d3.budget_chars,
            zen_core::config::KNOWLEDGE_BUDGET_CHARS_MIN,
            "structural clamp floor (design F2-D8)"
        );

        // Phase 2 — skill-hit injection through the shared orchestrator helper.
        let (session_skill, sub_skill) =
            tool.compose_sub_session("Explore", "please run the zen inheritance probe now", 0);
        assert_eq!(sub_skill.skill_hits, vec!["zen-probe".to_string()]);
        assert_eq!(
            session_skill.knowledge[0].path, "skills/zen-probe",
            "skill prompt rides at slot 0 (parent inject_skill_hits semantics)"
        );

        // Phase 3 — reservation estimate includes knowledge_chars/4: a
        // capacity that admits prompt/4+1024 but not the knowledge term
        // must refuse the delegation.
        // prompt "hello" ⇒ 5/4 + 1024 = 1025; inherited 4000/4 = 1000 ⇒ 2025.
        let tight = tool_with_knowledge(Arc::new(AtomicTokenBudget::new(1500)), knowledge.clone());
        let out = tight
            .invoke(serde_json::json!({ "agent": "Explore", "prompt": "hello" }))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .unwrap_or_default()
                .contains("token budget"),
            "the knowledge term must keep try_reserve_tokens honest: {out}"
        );

        // Phase 4 — loop.delegate.gates carries the additive fields with
        // real values while every pre-T215 field stays present.
        let out = tool
            .invoke(serde_json::json!({ "agent": "Explore", "prompt": "audit probe" }))
            .await
            .unwrap();
        assert!(out["response"].as_str().is_some(), "{out}");
        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl"))
            .expect("audit file under ZEN_HOME");
        let gates = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.delegate.gates")
            .expect("gates line");
        let entry = &gates["gates"][0];
        assert_eq!(entry["inherited_notes"].as_u64(), Some(2));
        assert_eq!(entry["inherited_chars"].as_u64(), Some(4000));
        assert_eq!(entry["skill_hits"].as_u64(), Some(0));
        for legacy in [
            "agent",
            "prompt_chars",
            "independent",
            "consumer_decision",
            "bounded",
            "worth_it",
        ] {
            assert!(
                entry.get(legacy).is_some(),
                "pre-T215 field {legacy} must survive (existing consumers keep parsing)"
            );
        }

        set_inheritance_env(None, None);
    }

    /// Gate CLOSED still emits the additive audit fields as zeros — the
    /// field shape is unconditional so consumers never branch on presence.
    #[tokio::test]
    async fn gates_audit_inheritance_fields_are_zero_when_closed() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let saved_home = std::env::var("ZEN_HOME").ok();
        set_inheritance_env(Some(dir.path()), None);
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({ "agent": "Explore", "prompt": "x" }))
            .await
            .unwrap();
        assert!(out["response"].as_str().is_some(), "{out}");
        let audit = std::fs::read_to_string(dir.path().join("logs/audit.jsonl")).expect("audit");
        let gates = audit
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| e["kind"] == "loop.delegate.gates")
            .expect("gates line");
        let entry = &gates["gates"][0];
        assert_eq!(entry["inherited_notes"].as_u64(), Some(0));
        assert_eq!(entry["inherited_chars"].as_u64(), Some(0));
        assert_eq!(entry["skill_hits"].as_u64(), Some(0));
        match saved_home {
            Some(home) => unsafe { std::env::set_var("ZEN_HOME", home) },
            None => unsafe { std::env::remove_var("ZEN_HOME") },
        }
        zen_core::config::invalidate_config_cache();
    }
}
