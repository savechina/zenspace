//! `delegate.task` — model-driven sub-agent delegation (006, US1/D1-D3).
//!
//! One tool, OpenCode `task.ts` shape: `{agent, prompt, description?}`.
//! Each invocation runs a REAL LLM sub-turn (identity + prompt assembly +
//! own tool grants + mini tool loop) and returns the sub-agent's final
//! response as the tool output. Depth-1 guard is by construction: sub-agent
//! grants are filtered of every `delegate.*` name, so a delegate can never
//! delegate.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rig_compose::budget::TokenBudget;
use rig_compose::normalizer::dispatch_tool_invocations_with_hooks;
use rig_compose::tool::{Tool, ToolSchema};
use tracing::{info, warn};
use zen_core::config::LlmPreference;
use zen_core::types::{RetrievedNote, Sensitivity, SessionContext};
use zen_provider::DefaultRouter;
use zen_provider::LlmRouter;

use crate::AgentContext;
use crate::agent_profile::{AgentProfile, Role};
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

/// D25 SC-024 (background narrowing): the enumerated grant-reduction list
/// applied to `background: true` admissions — the target agent's grants
/// MINUS these five, so the same agent definition resolves to a smaller
/// toolset behind the flag (Claude Code background filtering precedent).
/// Delegate/plan tools stay stripped at the depth cap as before; this list
/// is the spec, the admission-time grant test is the pin.
pub const BACKGROUND_NARROWED_TOOLS: [&str; 5] = [
    "fs.delete",
    "fs.move",
    "fs.copy",
    "shell.exec",
    "shell.kill",
];

/// One mirrored parent-conversation entry (D25 SC-021 `history` fork).
/// The orchestrator refreshes the shared cell at every turn entry (same
/// last-writer-wins semantics as `propagate_sensitivity`); composition
/// strips tool-call noise per A1 before the sub-session inherits it.
/// D30: the mirrored parent-conversation entry now lives in zen-core so the
/// turn-scoped task-local can carry it (zen-plugin hooks read the same
/// module). Re-exported here for the orchestrator's propagation site.
pub use zen_core::turn_context::ParentHistoryMessage;

pub type SharedHistory = Arc<Mutex<Vec<ParentHistoryMessage>>>;

/// Per-task context fork mode (D25 SC-021, codex `fork_turns` precedent):
/// `none` = empty context (byte-identical to the gate-closed path),
/// `knowledge` = today's gate-ON inheritance path (per-task addressable),
/// `history` = knowledge PLUS the stripped parent conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkMode {
    None,
    Knowledge,
    History,
}

impl ForkMode {
    /// Parse the model-supplied `fork_mode` argument.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Self::None),
            "knowledge" => Ok(Self::Knowledge),
            "history" => Ok(Self::History),
            other => Err(format!(
                "invalid fork_mode \"{other}\": expected none|knowledge|history"
            )),
        }
    }

    /// Lowercase label for audit/payload surfaces.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Knowledge => "knowledge",
            Self::History => "history",
        }
    }
}

/// Per-task model hint resolution outcome (D25 SC-022): additive
/// `model_source` falsifiability on `loop.delegate.outcome` and result
/// payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// AgentProfile.llm_preferences won over the hint (sensitivity >
    /// preference > hint).
    Profile,
    /// The hint steered the sub-agent (profile had no preference).
    Hint,
    /// No hint and no profile preference: routing default.
    Default,
}

impl ModelSource {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Hint => "hint",
            Self::Default => "default",
        }
    }
}

/// D25 SC-022 ladder, single source: `sensitivity > LlmPreference > hint >
/// default`. A profile with ANY explicit preference wins, so its hint never
/// steers. Kept as a free function so the runtime producer and the plan
/// forecast share one implementation (Constitution XI — no second ladder).
pub(crate) fn resolve_model_source(
    hint: Option<&str>,
    profile: Option<&AgentProfile>,
) -> ModelSource {
    let has_explicit_preference = profile.is_some_and(|p| {
        p.llm_preferences
            .iter()
            .any(|pref| !matches!(pref, LlmPreference::Any))
    });
    if has_explicit_preference {
        ModelSource::Profile
    } else if hint.is_some() {
        ModelSource::Hint
    } else {
        ModelSource::Default
    }
}

/// A-02/row13 — the ONE `(agent, model)` history-key resolver shared by the
/// delegate runtime producer (what the workflow checkpoint stores) and the
/// plan forecast consumer (what it looks up), so the two can never drift.
/// A steering hint is the key; otherwise the provider's default model name
/// is (`""` when it cannot be resolved — the forecast then simply omits the
/// pair instead of guessing).
#[must_use]
pub(crate) fn resolved_model_label(
    hint: Option<&str>,
    profile: Option<&AgentProfile>,
    provider_default: Option<&str>,
) -> String {
    match resolve_model_source(hint, profile) {
        ModelSource::Hint => hint.unwrap_or_default().to_string(),
        ModelSource::Profile | ModelSource::Default => {
            provider_default.unwrap_or_default().to_string()
        }
    }
}

/// One background-admission hold (W5.2, spec 006 D20 §8/D11-A). W12.26:
/// the armed token reservation NO LONGER lives here — `TokenReservation`
/// is not `Clone`, so single ownership through the spawned future is the
/// only sound shape: the future's `record_usage` reconcile refunds the
/// remainder (estimate − actual) on completion and its `Drop` refunds the
/// full estimate on early failure / kill / never-spawned rejection. The
/// book is refund-neutral; it carries only what the registry cannot.
pub(crate) struct BackgroundHolding {
    /// Model hint resolution (`None` = no hint given) — surfaced read-only
    /// by the W5.4 notice drain (`peek_model_source`); every other
    /// admission fact (agent/task_index/session/status/result) lives
    /// canonically in the registry entry + meta schema.
    pub(crate) model_source: Option<ModelSource>,
}

/// Outlives-turn bookkeeping for background delegate admissions. Held by
/// the tool (an `Arc` shared across rounds), so a reservation never bills
/// against a turn that already returned and reconciled (D20 §8).
#[derive(Clone, Default)]
pub struct BackgroundBook(Arc<Mutex<HashMap<String, BackgroundHolding>>>);

impl BackgroundBook {
    /// Insert an admission hold. `pub(crate)`: the orchestrator test suite
    /// seeds synthetic holds for notice-parity pins (W5.4).
    pub(crate) fn insert(&self, task_id: String, holding: BackgroundHolding) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(task_id, holding);
    }

    /// Release the hold for `task_id`. W12.26: refund-NEUTRAL — the hold
    /// carries no reservation; the budget refund lives in the spawned
    /// future's reconcile (`record_usage`) or `Drop`. `None` when the id
    /// is not a background delegate admission (e.g. a `shell.exec` task).
    pub(crate) fn release(&self, task_id: &str) -> Option<BackgroundHolding> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(task_id)
    }

    /// A clone sharing the same underlying map — the collect/kill tools
    /// observe and release the SAME holds the admission pass created
    /// (W5.3). Cheap: one `Arc` bump.
    #[must_use]
    pub fn share(&self) -> BackgroundBook {
        self.clone()
    }

    /// Read-only peek of an admission's resolved model source (W5.4 notice
    /// parity) — unlike [`release`] this never mutates the book; the entry
    /// itself is released by collect/kill/sweep (W12.26: refund-neutral).
    #[must_use]
    pub(crate) fn peek_model_source(&self, task_id: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task_id)
            .and_then(|h| h.model_source)
            .map(|m| m.as_str().to_string())
    }

    /// Lazy sweep: release holds whose registry entry reached a terminal
    /// state (expiry/failure paths this tool does not otherwise observe).
    /// Called on every background admission AND at the round-boundary
    /// notice drain (W11.16/T17 — expired holds release promptly, not
    /// lazily at the next admission). Cheap (≤ live-task count).
    pub(crate) fn sweep_terminal(&self) {
        let mut map = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ids: Vec<String> = map
            .keys()
            .filter(|id| {
                zen_plugin::task_registry::registry()
                    .snapshot(id)
                    .is_some_and(|s| s.status.is_terminal())
            })
            .cloned()
            .collect();
        for id in ids {
            map.remove(&id); // W12.26: refund-neutral — the pool refund lives in the future
        }
    }
}

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
    /// D25 SC-021 `history` fork source: parent-conversation mirror
    /// refreshed by the orchestrator at every turn entry (last-writer-wins,
    /// `propagate_knowledge` precedent).
    history: SharedHistory,
    token_budget: Arc<dyn TokenBudget>,
    timeout: Duration,
    max_depth: u32,
    max_concurrent: u32,
    skill_router: SkillHitRouter,
    /// D10 per-agent accuracy weights (empty = cold start ⇒ uniform).
    weights: HashMap<String, f32>,
    /// W5.2 background-admission bookkeeping (outlives turns — the tool is
    /// an Arc shared across rounds): per-task admission metadata keyed by
    /// registry task id. W12.26: refund-neutral — the hold carries NO
    /// reservation; the budget refund lives entirely in the spawned
    /// future's reconcile (`record_usage`) or `Drop`.
    background: BackgroundBook,
    /// Self-Arc cell (set by the constructor site BEFORE registry
    /// insertion): the background future must be 'static for
    /// `tokio::spawn`, and the `Tool::invoke(&self)` signature cannot
    /// clone `Arc<Self>` from `&self` — the registered-by-construction
    /// self reference closes that gap. The cycle is process-lifetime by
    /// design (the tool registry already holds the Arc forever).
    self_arc: std::sync::OnceLock<Arc<DelegateTaskTool>>,
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
    test_wave_delays: std::sync::Mutex<HashMap<String, u64>>,
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
        history: SharedHistory,
        token_budget: Arc<dyn TokenBudget>,
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
            history,
            token_budget,
            timeout,
            max_depth,
            max_concurrent,
            skill_router: SkillHitRouter::new(),
            weights: HashMap::new(),
            background: BackgroundBook::default(),
            self_arc: std::sync::OnceLock::new(),
            #[cfg(test)]
            test_wave_log: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            test_in_flight: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            test_peak_in_flight: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            test_wave_delays: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Constructor-site hook: capture `Arc<Self>` before registry
    /// insertion so `invoke_background` can move an owned clone into the
    /// spawned future (see the `self_arc` field doc).
    pub fn set_self_arc(&self, arc: Arc<DelegateTaskTool>) {
        let _ = self.self_arc.set(arc);
    }

    /// A clone of this tool's background book sharing the same underlying
    /// hold map — the `delegate.collect`/`delegate.kill` tools (W5.3) use
    /// it to observe and release the admission holds. W12.26: the release
    /// is refund-neutral (the hold carries no reservation).
    pub fn background_book(&self) -> BackgroundBook {
        self.background.share()
    }

    /// Inject D10 per-agent aggregation weights. An empty map (the default)
    /// leaves multi-result batch ordering byte-identical (cold start).
    pub fn with_weights(mut self, weights: HashMap<String, f32>) -> Self {
        self.weights = weights;
        self
    }

    /// G-13 wave instrumentation (test-only): append to the ordered
    /// `(agent, event)` log — `enter` / `exit`.
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

    /// G-13 test seam: hold each fan-out run of `agent` for `millis` after
    /// its enter event, so width-2 wave siblings observably overlap
    /// (SC-001). `tokio::time::sleep` always pends on first poll, so the
    /// overlap is structural, not a timing race.
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

    /// G-13 (test-only): peak in-flight sub-task concurrency observed
    /// across the whole fan-out (SC-001 evidence; never derived from time).
    #[cfg(test)]
    pub(crate) fn test_peak_in_flight(&self) -> usize {
        self.test_peak_in_flight
            .load(std::sync::atomic::Ordering::SeqCst)
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
    ///
    /// W12.1 (review A1, SC-024): this is the SINGLE production construction
    /// site, so the background narrowing is wired HERE — the
    /// `BACKGROUND_TASK` task-local is read (unset ⇒ `false`) instead of
    /// hardcoded. Background sub-turn futures run under
    /// `BACKGROUND_TASK.scope(true)` (see `invoke_background`), so their
    /// sub-agents lose the [`BACKGROUND_NARROWED_TOOLS`] five; the blocking
    /// path never scopes the flag and keeps byte-identical grants. The
    /// explicit `narrowed` parameter on [`Self::build_sub_agent_with_grants`]
    /// remains a test seam ONLY — a helper-only test is exactly how the dead
    /// wiring stayed green while production kept destructive tools (review
    /// A1 masked-dead-code lesson).
    fn build_sub_agent(
        &self,
        agent_name: &str,
        allow_child_delegation: bool,
    ) -> anyhow::Result<ZenAgent> {
        let narrowed = zen_plugin::tools::approval_hook::BACKGROUND_TASK
            .try_get()
            .unwrap_or(false);
        self.build_sub_agent_with_grants(agent_name, allow_child_delegation, narrowed)
    }

    /// W5.2 (SC-024): build the sub-agent with the background narrowing
    /// applied — the target agent's grants minus
    /// [`BACKGROUND_NARROWED_TOOLS`]. W12.1 (review A1): production never
    /// calls this with `narrowed: true` directly — [`Self::build_sub_agent`]
    /// derives the flag from the `BACKGROUND_TASK` task-local — so this
    /// explicit parameter is a test seam for the narrowing contract pins;
    /// the blocking path's toolset is untouched (pin).
    fn build_sub_agent_with_grants(
        &self,
        agent_name: &str,
        allow_child_delegation: bool,
        narrowed: bool,
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
        if narrowed {
            tools.retain(|name| !BACKGROUND_NARROWED_TOOLS.contains(&name.as_str()));
        }

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
    /// hard-capped at [`FANOUT_MAX_TASKS`] (T216). The optional D9 contract
    /// fields (`output_format`, `task_boundaries`) ride the top-level args for
    /// the single form and each `tasks[]` item for the fan-out form.
    fn parse_requests(args: &serde_json::Value) -> Result<Vec<ParsedDelegate>, String> {
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
                let fork_mode = match item.get("fork_mode") {
                    None => None,
                    Some(v) => Some(ForkMode::parse(
                        v.as_str()
                            .ok_or("fork_mode must be a string (none|knowledge|history)")?,
                    )?),
                };
                let model_hint = optional_nonempty_string(item, "model")?;
                tasks.push(ParsedDelegate {
                    request: DelegateRequest {
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
                        artifact: optional_nonempty_string(item, "artifact")?,
                        fork_mode,
                        model_hint,
                    },
                    contract: DelegateContract {
                        output_format: optional_nonempty_string(item, "output_format")?,
                        task_boundaries: optional_nonempty_string(item, "task_boundaries")?,
                    },
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
        Ok(vec![ParsedDelegate {
            request: DelegateRequest {
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
                artifact: optional_nonempty_string(args, "artifact")?,
                fork_mode: match args.get("fork_mode") {
                    None => None,
                    Some(v) => Some(ForkMode::parse(
                        v.as_str()
                            .ok_or("fork_mode must be a string (none|knowledge|history)")?,
                    )?),
                },
                model_hint: optional_nonempty_string(args, "model")?,
            },
            contract: DelegateContract {
                output_format: optional_nonempty_string(args, "output_format")?,
                task_boundaries: optional_nonempty_string(args, "task_boundaries")?,
            },
        }])
    }

    /// Advisory pre-spawn gates (Codex SPAWN_AGENT_CONTRACT adapted):
    /// independent = model-declared; consumer_decision = the parent can
    /// digest the batch width; bounded = self-contained prompt under the
    /// hard limit; worth_it = context spend below the advisory line.
    ///
    /// I2 (G-14): the `bounded` field here is INFORMATIONAL ONLY. The single
    /// enforcement site for the bounded-prompt limit is `validate_task` (its
    /// hard `prompt.len() > BOUNDED_PROMPT_CHARS` check) — never re-check
    /// `bounded` to reject a task. The field survives because the
    /// `loop.delegate.gates` audit line reports the advisory truth and
    /// existing consumers keep parsing it.
    ///
    /// I3 (G-15): `consumer_decision` uses an INDEPENDENT advisory threshold
    /// (literal 4), deliberately NOT bound to `max_concurrent` — see the
    /// inline comment at the field.
    fn evaluate_gates(task: &DelegateRequest, batch_width: usize) -> GateReport {
        GateReport {
            independent: task.independent,
            // I3 (G-15): INDEPENDENT advisory threshold. Deliberately NOT
            // bound to `max_concurrent`: waves never exceed `max_concurrent`,
            // so tying this literal to it would make the gate tautological.
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
        if let Some(artifact) = task.artifact.as_deref()
            && let Err(error) = self.validate_artifact_path(&task.agent, artifact)
        {
            return Some(error);
        }
        // D3 (eng-review 2026-11-20): a hint must name a model the
        // SENSITIVITY-ROUTED provider serves — reject at admission rather
        // than at the wire (the batch-level SC-022 check still ran first).
        if let Some(hint) = task.model_hint.as_deref()
            && let Err(error) = self.validate_hint_routed(hint, self.current_sensitivity())
        {
            return Some(error);
        }
        None
    }

    /// D3 (eng-review 2026-11-20, outside #6): validate one model hint
    /// against the SENSITIVITY-ROUTED provider's configured `models` keys —
    /// the admission scope the A-02 contract actually applies the hint in
    /// ("switches the model NAME within the routed provider, never the
    /// provider"). Fail-open when the routed provider cannot be resolved,
    /// is the mock stand-in, or declares no `models` table: the global
    /// catalog check (`validate_model_hint_values`) remains the gate for
    /// those shapes, keeping offline/test paths byte-identical.
    ///
    /// Residual corner (documented): a resumed plan whose recorded policy
    /// differs from the resuming session's sensitivity validates against
    /// the session cell read here; the runtime keeps its honest structured
    /// provider error for that case (W12.15 mismatched-cell resume).
    fn validate_hint_routed(&self, hint: &str, sensitivity: Sensitivity) -> Result<(), String> {
        let requirements = zen_provider::TaskRequirements {
            max_tokens: None,
            sensitivity,
            preferred_model: None,
            budget_limit: None,
        };
        let router = self.executor.router();
        let Ok(provider) = router
            .route_with_preferences(&requirements, &[])
            .or_else(|_| router.route(&requirements))
        else {
            return Ok(());
        };
        let name = provider.to_string();
        if name == "mock" {
            return Ok(());
        }
        let Ok(config) = zen_core::config::load_config() else {
            return Ok(());
        };
        let Some(entry) = config.providers.get(&name) else {
            return Ok(());
        };
        if entry.models.is_empty() {
            return Ok(());
        }
        if !entry.models.contains_key(hint) {
            return Err(format!(
                "model hint \"{hint}\" is not served by the sensitivity-routed provider \"{name}\" \
                 (hints switch the model name within the routed provider, never the provider)"
            ));
        }
        Ok(())
    }

    /// W2.2 (D12): validate a declared artifact output path with the SAME
    /// containment gate `fs.write` enforces. The sandbox mode + workspace
    /// roots are derived exactly as `ZenWiring::new` derives them, so a path
    /// accepted here is the path the sub-agent's `fs.write` will accept.
    /// The permission matrix is unchanged: an agent without the `fs.write`
    /// grant cannot declare an artifact.
    pub(crate) fn validate_artifact_path(&self, agent: &str, path: &str) -> Result<(), String> {
        if !delegate_tools::resolve_agent_tool_grants(agent, &self.overlay, &self.wiring.tools)
            .iter()
            .any(|g| g == "fs.write")
        {
            return Err(format!(
                "agent \"{agent}\" has no fs.write grant; it cannot produce an artifact"
            ));
        }
        let roots = zen_core::paths::ZenPaths::detect()
            .ok()
            .and_then(|p| p.workspace_root().cloned())
            .map(|w| vec![w])
            .unwrap_or_default();
        let validator = zen_core::sandbox::SandboxValidator::new(self.wiring.sandbox_mode(), roots);
        validator
            .validate_path_for_write(std::path::Path::new(path))
            .map_err(|denied| format!("artifact path rejected: {denied}"))
    }

    /// Current session sensitivity mirrored from the orchestrator — the
    /// plan-quality gate reuses it so plan reviews classify under the
    /// same policy as turn reviews.
    pub(crate) fn current_sensitivity(&self) -> Sensitivity {
        self.turn_sensitivity()
    }

    /// D30 turn-scoped sensitivity: the TurnContext task-local FIRST (race-
    /// free under concurrent hosted sessions), shared cell as the documented
    /// non-turn fallback (scheduler workers / direct tool invocation).
    fn turn_sensitivity(&self) -> Sensitivity {
        zen_core::turn_context::current_sensitivity().unwrap_or_else(|| {
            *self
                .sensitivity
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        })
    }

    /// D30 turn-scoped knowledge snapshot (same precedence as
    /// [`Self::turn_sensitivity`]; clone cost matches the cell snapshot).
    fn turn_knowledge(&self) -> Vec<RetrievedNote> {
        zen_core::turn_context::current_knowledge().unwrap_or_else(|| {
            self.knowledge
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
    }

    /// D30 turn-scoped parent-conversation snapshot (same precedence).
    fn turn_history(&self) -> Vec<ParentHistoryMessage> {
        zen_core::turn_context::current_history().unwrap_or_else(|| {
            self.history
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
    }

    /// D30 background-admission capture: snapshot the ambient turn context
    /// SYNCHRONOUSLY (task-locals do not cross `tokio::spawn`). Outside any
    /// turn scope the shared cells are snapshotted instead — the documented
    /// non-turn fallback, captured at admission so later cell overwrites
    /// cannot retroactively change an in-flight background sub-turn.
    pub(crate) fn capture_turn_context(&self) -> zen_core::turn_context::TurnContext {
        zen_core::turn_context::current().unwrap_or_else(|| zen_core::turn_context::TurnContext {
            sensitivity: self.turn_sensitivity(),
            knowledge: self.turn_knowledge(),
            history: self.turn_history(),
        })
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
        tasks: &[ParsedDelegate],
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
                    "agent": t.request.agent,
                    "prompt_chars": t.request.prompt.len(),
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

    /// One `loop.delegate.outcome` line PER executed task, right at its
    /// completion (`ok = error field absent` on the structured result).
    /// Purpose: the accuracy weights in `aggregate_weights` need a post-exec
    /// verdict — the pre-spawn `loop.delegate.gates` line only ever carried
    /// the `bounded` admit/deny (documented deviation there). Writing each
    /// outcome stands alone so a crash between the two appends degrades to
    /// the old gates-only derivation instead of block-mispairing.
    fn append_outcomes_audit(
        &self,
        parent: &str,
        depth: u32,
        tasks: &[ParsedDelegate],
        chunk: &[usize],
        outcomes: &[serde_json::Value],
    ) {
        if chunk.is_empty() {
            return;
        }
        let Ok(paths) = zen_core::paths::ZenPaths::detect() else {
            return;
        };
        let log_path = paths.logs().join("audit.jsonl");
        if let Some(dir) = log_path.parent()
            && std::fs::create_dir_all(dir).is_ok()
        {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log_path) {
                for (&i, value) in chunk.iter().zip(outcomes) {
                    let entry = serde_json::json!({
                        "kind": "loop.delegate.outcome",
                        "parent": parent,
                        "depth": depth,
                        "agent": tasks[i].request.agent,
                        "ok": value.get("error").is_none(),
                        "task_index": i,
                    });
                    let _ = writeln!(f, "{entry}");
                }
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
        let sensitivity = self.turn_sensitivity();
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
        let snapshot = self.turn_knowledge();
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

    /// D25 SC-021 per-task fork composition. `None` (argument absent)
    /// delegates to the session-wide `[agentic.delegate]
    /// context_inheritance` default — byte-identical to the pre-D25 path.
    /// `history` = knowledge fork PLUS the mirrored parent conversation
    /// (user/developer + assistant prose; tool-call noise stripped — A1's
    /// load-bearing detail) injected into the session's conversation.
    fn compose_sub_session_forked(
        &self,
        agent_name: &str,
        prompt: &str,
        depth: u32,
        fork_mode: Option<ForkMode>,
    ) -> (SessionContext, SubAgentContext) {
        let resolved = fork_mode.unwrap_or_else(|| {
            if inheritance_budget_chars().is_some() {
                ForkMode::Knowledge
            } else {
                ForkMode::None
            }
        });
        match resolved {
            ForkMode::None => {
                // Gate-closed path: empty knowledge, no history.
                let sensitivity = self.turn_sensitivity();
                let mut session = SessionContext::new(agent_name.to_string(), String::new());
                session.sensitivity_policy = sensitivity;
                (
                    session,
                    SubAgentContext {
                        knowledge: Vec::new(),
                        skill_hits: Vec::new(),
                        budget_chars: 0,
                    },
                )
            }
            ForkMode::Knowledge => self.compose_sub_session(agent_name, prompt, depth),
            ForkMode::History => {
                let (mut session, sub) = self.compose_sub_session(agent_name, prompt, depth);
                let parent = self.turn_history();
                for entry in parent {
                    // Defensive re-strip: the mirror already strips at
                    // propagation; composition must not trust producers.
                    let content = crate::orchestrator::strip_fenced_tool_blocks(&entry.content);
                    if content.trim().is_empty() {
                        continue;
                    }
                    let role = match entry.role.as_str() {
                        "user" => zen_core::types::MessageRole::User,
                        "assistant" => zen_core::types::MessageRole::Assistant,
                        _ => continue,
                    };
                    session.conversation.push(zen_core::types::Message {
                        role,
                        content,
                        timestamp: None,
                    });
                }
                (session, sub)
            }
        }
    }

    /// D25 SC-022 model-hint resolution (thin wrapper over the shared
    /// [`resolve_model_source`] ladder). Returns the source label for the
    /// additive `model_source` falsifiability field. A-02 (D2b): a
    /// `ModelSource::Hint` outcome ALSO sets
    /// [`AgentContext::model_override`] so the hint switches the runtime
    /// MODEL NAME within the sensitivity-routed provider (never the
    /// provider itself); `Profile`/`Default` stay byte-identical (`None`).
    fn resolve_model_hint(
        &self,
        hint: Option<&str>,
        profile: Option<&AgentProfile>,
    ) -> ModelSource {
        resolve_model_source(hint, profile)
    }

    /// A-02/row13: resolve the provider's configured default model name the
    /// delegate path would use for `agent` under `sensitivity`. Config-only
    /// (`route_with_preferences` probes only for a `LocalOnly` profile);
    /// `None` when no provider resolves — the forecast then omits the pair.
    fn provider_default_model(
        &self,
        profile: Option<&AgentProfile>,
        sensitivity: Sensitivity,
    ) -> Option<String> {
        let requirements = zen_provider::TaskRequirements {
            max_tokens: None,
            sensitivity,
            preferred_model: None,
            budget_limit: None,
        };
        let preferences = profile.map_or(&[][..], |p| p.llm_preferences.as_slice());
        let router = self.executor.router();
        let provider = router
            .route_with_preferences(&requirements, preferences)
            .or_else(|_| router.route(&requirements))
            .ok()?;
        let name = provider.to_string();
        if name == "mock" {
            return Some("mock".to_string());
        }
        router
            .provider_instance(&name)
            .map(|instance| instance.model_name().to_string())
    }

    /// A-02/row13: the forecast-side world of
    /// [`resolved_model_label`] — same ladder, same provider-default
    /// resolution the runtime producer reports, so the `(agent, model)`
    /// history key the checkpoint writes is the one the forecast looks up.
    pub(crate) fn model_history_key(
        &self,
        agent: &str,
        hint: Option<&str>,
        sensitivity: Sensitivity,
    ) -> String {
        let profile = self.registry.find_by_name(agent).ok().cloned();
        let provider_default = match resolve_model_source(hint, profile.as_ref()) {
            ModelSource::Hint => None,
            ModelSource::Profile | ModelSource::Default => {
                self.provider_default_model(profile.as_ref(), sensitivity)
            }
        };
        resolved_model_label(hint, profile.as_ref(), provider_default.as_deref())
    }

    /// D25 SC-022: the configured model catalog (5-layer config,
    /// `[providers.*] models` keys). `None` when config cannot load — the
    /// admission then rejects hints (fail-closed) rather than accepting
    /// unknown names.
    fn model_catalog() -> Option<std::collections::HashSet<String>> {
        let config = zen_core::config::load_config().ok()?;
        let mut names = std::collections::HashSet::new();
        for provider in config.providers.values() {
            for key in provider.models.keys() {
                names.insert(key.clone());
            }
        }
        Some(names)
    }

    /// D25 SC-022 admission validation: catalog-validated hint, ≤5
    /// distinct hints per batch.
    fn validate_model_hints(tasks: &[ParsedDelegate]) -> Result<(), String> {
        let hints: Vec<String> = tasks
            .iter()
            .filter_map(|t| t.request.model_hint.clone())
            .collect();
        Self::validate_model_hint_values(&hints)
    }

    /// Shared SC-022 hint-set validation (W11.5/T5): ≤5 distinct hints per
    /// batch, each in the configured provider catalog. Reused by the
    /// `plan.execute` parse path so plan and delegate batches enforce ONE
    /// rule (Constitution XI — no second validator).
    pub(crate) fn validate_model_hint_values(hints: &[String]) -> Result<(), String> {
        let mut distinct = std::collections::HashSet::new();
        for hint in hints {
            distinct.insert(hint.clone());
        }
        if distinct.len() > MODEL_HINT_MAX_DISTINCT {
            return Err(format!(
                "batch declares {} distinct model hints; at most {MODEL_HINT_MAX_DISTINCT} are allowed",
                distinct.len()
            ));
        }
        let Some(catalog) = Self::model_catalog() else {
            return Err(
                "model hint rejected: provider catalog unavailable (config load failed)"
                    .to_string(),
            );
        };
        for hint in &distinct {
            if !catalog.contains(hint) {
                return Err(format!(
                    "unknown model hint \"{hint}\": not in the configured provider catalog"
                ));
            }
        }
        Ok(())
    }

    /// W5.2 (D20 §1): the `background: true` admission path. Hard
    /// `validate_task` + advisory `evaluate_gates` have ALREADY run (the
    /// same shared pass as the blocking path); this admits each runnable
    /// task to the task registry and returns immediately — the parent
    /// round never waits on a sub-turn.
    ///
    /// Admission gates, in order: model-hint validation (SC-022), the
    /// per-turn token reserve (D11-A — insufficient budget is a structured
    /// rejection), then the registry's atomic concurrency gate. Depth was
    /// resolved by the shared validate pass (parent depth + 1) and the
    /// narrowed grant set is applied at sub-agent construction via the
    /// `BACKGROUND_TASK` task-local this future scopes (SC-024, W12.1);
    /// `SESSION_SCOPE` is read synchronously here because task-locals do
    /// not cross `tokio::spawn` (the APPROVAL_TURN precedent) — and
    /// re-entered inside the spawned future (W12.3) so nested admissions
    /// attribute to the real owning session.
    async fn invoke_background(
        &self,
        tasks: &[ParsedDelegate],
        depth: u32,
        parent: &str,
    ) -> Result<serde_json::Value, rig_compose::registry::KernelError> {
        Self::validate_model_hints(tasks).map_err(|e| {
            rig_compose::registry::KernelError::InvalidArgument(format!(
                "background admission rejected: {e}"
            ))
        })?;

        let owned = Arc::clone(
            self.self_arc
                .get()
                .ok_or_else(|| {
                    rig_compose::registry::KernelError::InvalidArgument(
                        "background admission rejected: delegate tool self-arc not wired                          (constructor must call set_self_arc before registration)"
                            .to_string(),
                    )
                })?,
        );
        let session_id = zen_plugin::task_registry::current_session_scope();
        let limits = self.background_limits();

        let mut admitted: Vec<serde_json::Value> = Vec::with_capacity(tasks.len());
        let mut task_ids: Vec<String> = Vec::with_capacity(tasks.len());

        for (i, task) in tasks.iter().enumerate() {
            // Rejections carry slot order (task_index) exactly like the
            // blocking path — re-attributable by consumers.
            let rejected = self.validate_task(parent, depth, &task.request);
            if let Some(error) = rejected {
                admitted.push(serde_json::json!({
                    "task_index": i,
                    "agent": task.request.agent,
                    "error": error,
                    "background": true,
                }));
                continue;
            }

            let fork_mode = task.request.fork_mode;
            let profile = self
                .registry
                .find_by_name(&task.request.agent)
                .ok()
                .cloned();
            let model_source =
                self.resolve_model_hint(task.request.model_hint.as_deref(), profile.as_ref());

            // Compose BEFORE the reserve (W11.16/T17, OV-6): the composed
            // context moves into the spawned future, and composing first
            // lets the admission estimate include the inherited chars the
            // sub-turn will actually carry (SC-021 additive clause). Depth
            // resolved at admission; grants narrow inside the future via
            // the BACKGROUND_TASK scope.
            let (session, sub) = self.compose_sub_session_forked(
                &task.request.agent,
                &task.request.prompt,
                depth,
                fork_mode,
            );

            // Admission token reserve (D11-A). W12.26: the estimate is the
            // RUNTIME estimate shape — `compose_sub_prompt` bytes (D9
            // contract + artifact block + the always-on REPORT_REQUEST
            // trailer) / 4 + 1024 + inherited_chars/4 — so the admission
            // hold ≡ the runtime hold by construction and the armed
            // reservation can move INTO the spawned future with no second
            // reserve and no latent ~REPORT_REQUEST-sized under-hold.
            let estimate = (compose_sub_prompt(&task.request, &DelegateContract::default()).len()
                / 4
                + 1024
                + sub.knowledge_chars() / 4) as u64;
            let reservation = match self.token_budget.try_reserve_tokens(estimate).await {
                Ok(Some(reservation)) => reservation,
                Ok(None) => {
                    admitted.push(serde_json::json!({
                        "task_index": i,
                        "agent": task.request.agent,
                        "error": "background admission rejected: insufficient token budget for the sub-turn reserve",
                        "background": true,
                    }));
                    continue;
                }
                Err(e) => {
                    admitted.push(serde_json::json!({
                        "task_index": i,
                        "agent": task.request.agent,
                        "error": format!("background admission rejected: token budget reservation failed: {e}"),
                        "background": true,
                    }));
                    continue;
                }
            };

            let task_id = uuid::Uuid::new_v4().simple().to_string();
            // W12.26: the reservation-less holding enters the book BEFORE
            // the spawn so sweep_terminal/release observe it from the first
            // instant; the armed reservation itself moves INTO the future
            // chain (single ownership — the future's `record_usage`
            // reconcile or `Drop` is the only refund path; the book is
            // refund-neutral).
            self.background.insert(
                task_id.clone(),
                BackgroundHolding {
                    model_source: Some(model_source),
                },
            );

            // D30: the captured turn context scopes the spawned future so
            // the background sub-turn's hook gates and any composition read
            // the ADMITTING session's policy, immune to concurrent-session
            // cell overwrites (BACKGROUND_TASK.scope precedent — task-locals
            // survive block_in_place/join_all, NOT tokio::spawn).
            let turn_ctx = self.capture_turn_context();
            // W12.3 (review A3): task-locals do not cross `tokio::spawn` —
            // re-enter SESSION_SCOPE inside the spawned future with the
            // session id captured at admission, so nested admissions
            // (depth ≥ 2) attribute to the REAL owning session instead of
            // falling back to "local".
            let future = zen_core::turn_context::TURN_CONTEXT.scope(
                turn_ctx,
                zen_plugin::tools::approval_hook::BACKGROUND_TASK.scope(
                    true,
                    zen_plugin::task_registry::SESSION_SCOPE.scope(
                        session_id.clone(),
                        Arc::clone(&owned).run_single_background(
                            task.request.clone(),
                            depth,
                            parent.to_string(),
                            (session, sub),
                            model_source,
                            reservation,
                        ),
                    ),
                ),
            );
            let spawned = zen_plugin::task_registry::registry().spawn_async(
                &task_id,
                &session_id,
                &task.request.agent,
                Some(i),
                &limits,
                future,
            );
            match spawned {
                Ok(_join) => {
                    task_ids.push(task_id.clone());
                    admitted.push(serde_json::json!({
                        "task_index": i,
                        "agent": task.request.agent,
                        "task_id": task_id,
                        "background": true,
                        "status": "running",
                        "fork_mode": fork_mode.map(|f| f.as_str()),
                        "model_source": model_source.as_str(),
                    }));
                }
                Err(e) => {
                    // Registry gate (concurrency/drain/id): structured
                    // rejection. W12.26: the release is refund-neutral (the
                    // book carries no reservation); the armed reservation
                    // inside the never-spawned future refunds the full
                    // estimate via `TokenReservation::drop`.
                    self.background.release(&task_id);
                    admitted.push(serde_json::json!({
                        "task_index": i,
                        "agent": task.request.agent,
                        "error": format!("background admission rejected: {e}"),
                        "background": true,
                    }));
                }
            }
        }

        // Lazy release of holds whose tasks already reached terminal states
        // through paths this tool does not observe (expiry, registry loss).
        self.background.sweep_terminal();

        Ok(serde_json::json!({
            "started": task_ids.len(),
            "task_ids": task_ids,
            "background": true,
            "admissions": admitted,
        }))
    }

    /// The background sub-turn future body (W5.2): the SAME delegate loop
    /// as `run_single_composed` — which re-verifies the depth guard exactly
    /// as blocking sub-turns do — with the D25 result-payload seeds added.
    /// W12.26: the admission's ARMED reservation rides in (`reservation`)
    /// and is consumed by the completion reconcile inside the future — the
    /// runtime re-reserve is gone (single armed reservation, no soft-deny
    /// re-check). The narrowed grant set (SC-024) is applied inside
    /// `build_sub_agent` via the `BACKGROUND_TASK` task-local scope this
    /// future runs under, so no loop duplication is needed. `composed`
    /// bundles `(session, sub)` so the future body stays within clippy's
    /// arity gate (no allow-suppression, W12.26).
    async fn run_single_background(
        self: Arc<Self>,
        req: DelegateRequest,
        depth: u32,
        parent: String,
        composed: (SessionContext, SubAgentContext),
        model_source: ModelSource,
        reservation: rig_compose::budget::TokenReservation,
    ) -> anyhow::Result<serde_json::Value> {
        let (session, sub) = composed;
        let mut outcome = self
            .run_single_composed(
                &req,
                depth,
                &parent,
                (session, sub),
                &DelegateContract::default(),
                Some(reservation),
            )
            .await;
        if let Some(obj) = outcome.as_object_mut() {
            obj.insert(
                "model_source".to_string(),
                serde_json::json!(model_source.as_str()),
            );
        }
        Ok(outcome)
    }

    /// Background admission limits: the TaskRegistry's
    /// `[agentic.background] max_concurrent` is the single shared
    /// enforcement point for ALL background work (shell.exec children AND
    /// delegate futures) — the W5.2 union-cap design decision (merge gate,
    /// spec D20 §6). The delegate `max_concurrent` key governs foreground
    /// fan-out width only.
    fn background_limits(&self) -> zen_plugin::task_registry::BackgroundLimits {
        let config = zen_core::config::load_config().ok();
        let max_wall_secs = config
            .as_ref()
            .and_then(|c| c.agentic.background.max_wall_secs)
            .unwrap_or(zen_core::config::BACKGROUND_MAX_WALL_SECS_DEFAULT);
        let max_concurrent = config
            .as_ref()
            .and_then(|c| c.agentic.background.max_concurrent)
            .unwrap_or(zen_core::config::BACKGROUND_MAX_CONCURRENT_DEFAULT);
        zen_plugin::task_registry::BackgroundLimits {
            max_wall_secs,
            max_concurrent: max_concurrent as usize,
            output_cap_bytes: zen_plugin::task_registry::OUTPUT_CAP_BYTES,
        }
    }

    /// Execute one validated delegation request (tier/depth/bounded checks
    /// already passed in `invoke`). Always returns a readable Value. Used by
    /// `plan.execute`, which has no D9 contract (empty ⇒ pre-D9 prompt).
    pub(crate) async fn run_single(
        &self,
        req: &DelegateRequest,
        depth: u32,
        parent: &str,
    ) -> serde_json::Value {
        // W11.5/T5 (SC-021): the plan path honors the per-task fork_mode —
        // `None` resolves to the session-wide default byte-identically.
        let composed =
            self.compose_sub_session_forked(&req.agent, &req.prompt, depth, req.fork_mode);
        self.run_single_composed(
            req,
            depth,
            parent,
            composed,
            &DelegateContract::default(),
            None,
        )
        .await
    }

    /// `run_single` with a pre-composed sub-session — the `invoke` fan-out
    /// path composes BEFORE `append_gates_audit` so the audit line carries
    /// the real per-task `{inherited_notes, inherited_chars, skill_hits}`
    /// values; composition is deterministic, so pre-composing here and
    /// inside `run_single` (the plan.execute path) cannot drift.
    ///
    /// W12.26: `armed` carries a PRE-PAID reservation (the background
    /// admission's, computed against this same composed prompt). When
    /// `Some`, the runtime reserve below is SKIPPED entirely — no soft-deny
    /// re-check (admission already paid; the re-check was the double-hold
    /// bug) — and the armed reservation is consumed by the completion
    /// reconcile. `None` keeps the blocking paths byte-identical.
    /// `composed` bundles `(session, sub)` (W12.26: clippy arity gate —
    /// no allow-suppression).
    pub(crate) async fn run_single_composed(
        &self,
        req: &DelegateRequest,
        depth: u32,
        parent: &str,
        composed: (SessionContext, SubAgentContext),
        contract: &DelegateContract,
        armed: Option<rig_compose::budget::TokenReservation>,
    ) -> serde_json::Value {
        let (session, sub) = composed;
        let started = Instant::now();
        let agent_name = req.agent.as_str();
        let prompt = compose_sub_prompt(req, contract);
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

        let model_source = resolve_model_source(req.model_hint.as_deref(), Some(&profile));
        let mut context = AgentContext::new(profile.clone(), prompt.to_string(), session)
            .with_preferences(profile.llm_preferences.clone());
        if model_source == ModelSource::Hint
            && let Some(hint) = req.model_hint.as_deref()
        {
            context = context.with_model_override(hint);
        }
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
        // W12.26: `armed` (background admission) is consumed as-is — the
        // admission already reserved THIS exact estimate from the composed
        // prompt, so re-reserving would double-hold and re-checking the
        // budget soft-deny would be the bug this wave removes. `None`
        // (blocking fan-out / plan.execute) reserves below byte-identically.
        let inherited_chars = sub.knowledge_chars();
        let estimated = (prompt.len() / 4 + 1024 + inherited_chars / 4) as u64;
        let reservation = match armed {
            Some(reservation) => reservation,
            None => match self.token_budget.try_reserve_tokens(estimated).await {
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
            },
        };

        let (early_error, mut response, rounds, timed_out, runtime_model, usage_cost_usd) =
            DELEGATE_DEPTH.scope(
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
        let mut runtime_model: Option<String> = None;
        let mut usage_cost_usd: Option<f64> = None;

        while rounds < MAX_DELEGATE_ROUNDS {
            if let Some(note) = Self::deadline_exceeded(deadline) {
                timed_out = true;
                warn!(agent = agent_name, rounds, "delegate sub-turn hit deadline");
                response.push_str(&format!("\n\n[{note}]"));
                break;
            }
            let Ok(execution) = crate::orchestrator::run_blocking_llm(|| {
                self.executor
                    .execute_round(&context, &agent, &manifest, &feedback)
            }) else {
                return (
                    Some(serde_json::json!({
                        "error": format!("delegate agent \"{agent_name}\" LLM round failed")
                    })),
                    response,
                    rounds,
                    timed_out,
                    runtime_model,
                    usage_cost_usd,
                );
            };
            rounds += 1;
            if execution.metadata.runtime_model.is_some() {
                runtime_model = execution.metadata.runtime_model.clone();
            }
            if let Some(cost) = execution.metadata.usage_cost_usd {
                *usage_cost_usd.get_or_insert(0.0) += cost;
            }
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
                    feedback = dispatch_error_feedback(&e);
                }
            }
        }
                        (
                            None,
                            response,
                            rounds,
                            timed_out,
                            runtime_model,
                            usage_cost_usd,
                        )
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

        let (clean_response, reasoning, verification) = split_report_markers(&response);
        let response_chars = clean_response.chars().count();
        // W12.4 (review N4): rig-compose `BudgetGuard::record_usage` SUMS
        // its prompt+completion arguments into one consumption debit
        // (`prompt.saturating_add(completion)`, budget.rs record_usage) —
        // the historical `record_usage(actual, actual)` billed every
        // delegate sub-turn at 2× its real size, silently halving the
        // effective budget capacity. Report the REAL split instead (the
        // same bytes→tokens divisor the estimate uses); the arguments sum
        // back to the identical total, billed exactly once.
        let prompt_tokens = (prompt.len() / 4) as u64;
        let completion_tokens = (clean_response.len() / 4) as u64;
        self.token_budget
            .record_usage(reservation, prompt_tokens, completion_tokens)
            .await;

        // FIX-5: an empty or tool-JSON-only sub-agent response must not
        // masquerade as an ok-shaped result. The error is built BEFORE the
        // QE-cascade shadow below so the shadow records `has_error: true`,
        // and before the completion log (which now carries the error flag).
        let honesty_error = sub_response_honesty_error(
            agent_name,
            req.artifact.as_deref(),
            &clean_response,
            rounds,
        );
        let output = if let Some(error) = honesty_error {
            serde_json::json!({ "error": error })
        } else {
            // W2.2 (D12) artifact pattern: surface the declared output pointer
            // and a bounded summary alongside the answer (additive — a request
            // without an artifact keeps the pre-W2 shape byte-identical).
            let artifact_summary = req
                .artifact
                .as_deref()
                .map(|_| bounded_summary(&clean_response));
            let mut output = serde_json::json!({
                "agent": agent_name,
                "response": clean_response,
                "rounds": rounds,
                "duration_ms": started.elapsed().as_millis() as u64,
            });
            if let Some(artifact) = req.artifact.as_deref() {
                output["artifact_path"] = serde_json::json!(artifact);
                if let Some(summary) = artifact_summary {
                    output["summary"] = serde_json::json!(summary);
                }
            }
            if let Some(description) = req.description.as_ref() {
                output["description"] = serde_json::json!(description);
            }
            if let Some(reasoning) = reasoning {
                output["reasoning"] = serde_json::json!(reasoning);
            }
            if let Some(verification) = verification {
                output["verification"] = serde_json::json!(verification);
            }
            // W11.5/T5 (SC-022 falsifiability): the same model_source label
            // the background path seeds — resolution order sensitivity >
            // LlmPreference > hint > default. A-02: the label is display
            // honesty only; the RUNTIME model name (and the
            // forecast-keyed history value) is `model` below.
            output["model_source"] = serde_json::json!(model_source.as_str());
            // A-02/row13: the REAL model name the sub-turn ran under and its
            // metered cost. Both additive/optional — a provider that reports
            // neither leaves the keys absent (honest degradation).
            let model_label = resolved_model_label(
                req.model_hint.as_deref(),
                Some(&profile),
                runtime_model.as_deref(),
            );
            if !model_label.is_empty() {
                output["model"] = serde_json::json!(model_label);
            }
            if let Some(cost) = usage_cost_usd {
                output["cost_usd"] = serde_json::json!(cost);
            }
            output
        };
        // T233 (006 D13/W3.1): post-`run_single` quality estimate — a
        // record-only shadow observation (`loop.decision`, `rung:"shadow"`).
        // It never gates or mutates the returned result.
        if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
            let signal = crate::decision::QualitySignal {
                has_error: output.get("error").is_some(),
                response_chars,
            };
            crate::decision::run_qe_cascade_shadow(
                &paths,
                &context.session.session_id.to_string(),
                &signal,
                None,
            );
        }
        info!(
            agent = agent_name,
            rounds,
            duration_ms = started.elapsed().as_millis() as u64,
            knowledge_inherited = !sub.knowledge.is_empty(),
            inherited_chars,
            inherited_budget_chars = sub.budget_chars,
            has_error = output.get("error").is_some(),
            "delegate.task sub-turn completed"
        );
        output
    }
}

/// FIX-5: honesty check on a sub-agent's final answer. A delegate that
/// produced no prose (or ended on unexecuted tool-call JSON alone) must
/// return a structured `error` instead of an ok-shaped result with an empty
/// response string — consumers (plan.execute status, weight aggregation)
/// read `response`'s presence, and `""` is not a deliverable.
///
/// Contract (Oracle D4): applies regardless of parse-error history. A
/// declared `artifact` short-circuits to `None` — the artifact file is the
/// deliverable, prose is optional there.
pub(crate) fn sub_response_honesty_error(
    agent: &str,
    artifact: Option<&str>,
    clean_response: &str,
    rounds: usize,
) -> Option<String> {
    if artifact.is_some() {
        return None;
    }
    if clean_response.trim().is_empty() {
        return Some(format!(
            "sub-agent \"{agent}\" produced no response ({rounds} round(s))"
        ));
    }
    let stripped = crate::orchestrator::strip_fenced_tool_blocks(clean_response);
    if stripped.trim().is_empty()
        && !crate::orchestrator::AgentOrchestrator::parse_tool_invocations_verbose(clean_response)
            .0
            .is_empty()
    {
        return Some(format!(
            "sub-agent \"{agent}\" ended with unexecuted tool call(s) and no prose answer"
        ));
    }
    None
}

/// One delegation subtask — the fan-out unit (001 A.6 collect mode).
#[derive(Debug, Clone)]
pub struct DelegateRequest {
    pub agent: String,
    pub prompt: String,
    pub description: Option<String>,
    pub independent: bool,
    /// W2.2 (D12): optional artifact output path the sub-agent should write
    /// its deliverable to (sandbox-validated; NO new tool grants — only
    /// agents already holding `fs.write` may declare one).
    pub artifact: Option<String>,
    /// D25 SC-021 per-task fork mode. `None` (argument absent) preserves the
    /// session-wide `[agentic.delegate] context_inheritance` behavior
    /// byte-identically — the per-task argument wins when present.
    pub fork_mode: Option<ForkMode>,
    /// D25 SC-022 per-task model hint (catalog-validated upstream, ≤5
    /// distinct per batch). Resolution: sensitivity > LlmPreference > hint
    /// > default — the hint can never weaken sensitivity routing.
    pub model_hint: Option<String>,
}

/// D9 optional delegation contract (Anthropic 4-part shape). Carried beside
/// each [`DelegateRequest`] so the sub-turn prompt assembly can inject the
/// caller's `output_format` / `task_boundaries` in labeled sections. Both
/// fields are optional; when present they must be non-empty (validated in
/// `parse_requests`). Serde defaults keep older payloads decodable (`{}` ⇒
/// all-`None`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DelegateContract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_boundaries: Option<String>,
}

impl DelegateContract {
    fn is_empty(&self) -> bool {
        self.output_format.is_none() && self.task_boundaries.is_none()
    }

    /// The labeled contract section appended to the sub-agent brief, or
    /// `None` when the contract is empty (⇒ prompt byte-identical to pre-D9).
    fn labeled_section(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut section = String::from("\n\n---\nDelegation contract:");
        if let Some(output_format) = &self.output_format {
            section.push_str("\nOUTPUT FORMAT:\n");
            section.push_str(output_format);
        }
        if let Some(task_boundaries) = &self.task_boundaries {
            section.push_str("\nTASK BOUNDARIES:\n");
            section.push_str(task_boundaries);
        }
        section.push_str("\n---");
        Some(section)
    }
}

/// One parsed request plus its optional D9 contract.
#[derive(Debug)]
struct ParsedDelegate {
    request: DelegateRequest,
    contract: DelegateContract,
}

/// D9 report-schema instruction appended to every sub-agent brief: the model
/// is asked for optional `REASONING:` / `VERIFICATION:` lines that
/// [`split_report_markers`] lifts into the result payload.
const REPORT_REQUEST: &str = "\n\n---\nWhen you have finished, end your answer with two optional lines:\nREASONING: <one sentence on your approach>\nVERIFICATION: <how you checked the result>\n---";

/// Assemble the sub-agent brief: the caller's prompt, the optional D9
/// contract sections, the optional artifact-output instruction, then the
/// report-schema request.
fn compose_sub_prompt(req: &DelegateRequest, contract: &DelegateContract) -> String {
    let mut prompt = req.prompt.clone();
    if let Some(section) = contract.labeled_section() {
        prompt.push_str(&section);
    }
    if let Some(artifact) = req.artifact.as_deref() {
        prompt.push_str(&format!(
            "\n\n---\nARTIFACT OUTPUT: write your full deliverable to the file at this exact path using the fs.write tool, then state the path in your answer: {artifact}\n---"
        ));
    }
    prompt.push_str(REPORT_REQUEST);
    prompt
}

/// W2.2 (D12): cap for the artifact-mode `summary` field (chars) so a
/// downstream plan/parent prompt embedding it stays bounded — mirrors the
/// plan-task upstream-snippet bound.
pub(crate) const ARTIFACT_SUMMARY_MAX_CHARS: usize = 4000;

/// Bounded form of a sub-agent answer for the artifact-mode `summary`.
fn bounded_summary(text: &str) -> String {
    let mut summary: String = text.chars().take(ARTIFACT_SUMMARY_MAX_CHARS).collect();
    if text.chars().count() > ARTIFACT_SUMMARY_MAX_CHARS {
        summary.push_str("…[truncated]");
    }
    summary
}

/// Lift optional `REASONING:` / `VERIFICATION:` report lines out of a
/// sub-agent's final answer (D9). Markers are removed from the returned
/// prose. When neither marker is present the answer is returned unchanged
/// (byte-identical to the pre-D9 result path) with both fields `None`.
fn split_report_markers(response: &str) -> (String, Option<String>, Option<String>) {
    let mut reasoning = None;
    let mut verification = None;
    let mut prose: Vec<&str> = Vec::new();
    let mut found = false;
    for line in response.lines() {
        if let Some(value) = take_marker(line, "reasoning:") {
            reasoning = Some(value);
            found = true;
            continue;
        }
        if let Some(value) = take_marker(line, "verification:") {
            verification = Some(value);
            found = true;
            continue;
        }
        prose.push(line);
    }
    if !found {
        return (response.to_string(), None, None);
    }
    (
        prose.join("\n").trim_end().to_string(),
        reasoning,
        verification,
    )
}

/// If `line` starts with `marker` (ASCII, case-insensitive), return the
/// trimmed remainder.
fn take_marker(line: &str, marker: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed.len() >= marker.len()
        && trimmed.is_char_boundary(marker.len())
        && trimmed[..marker.len()].eq_ignore_ascii_case(marker)
    {
        Some(trimmed[marker.len()..].trim().to_string())
    } else {
        None
    }
}

/// Read an optional string argument; `None`/absent ⇒ `None`, present ⇒
/// non-empty after trimming (D9 validation).
fn optional_nonempty_string(args: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => {
            if value.trim().is_empty() {
                Err(format!("\"{key}\" must not be empty when present"))
            } else {
                Ok(Some(value.clone()))
            }
        }
        Some(_) => Err(format!("\"{key}\" must be a string when present")),
    }
}

/// G1-09: label a dispatch failure accurately. A hook `Terminate`
/// (approval/seatbelt) or a budget failure is a *blocked* dispatch; every
/// other kernel error is a tool-execution failure. The old single label
/// ("blocked by sandbox") mislabeled every `ToolFailed`/`NotFound`.
fn dispatch_error_feedback(error: &rig_compose::registry::KernelError) -> String {
    use rig_compose::registry::KernelError;
    match error {
        KernelError::ToolDispatchTerminated(_) | KernelError::BudgetFailed(_) => format!(
            "tool dispatch blocked (approval/seatbelt/budget): {error}\nAnswer the user with what you have."
        ),
        _ => format!("tool failed: {error}\nAnswer the user with what you have."),
    }
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

/// D25 SC-022: at most this many DISTINCT model hints per admission batch.
pub(crate) const MODEL_HINT_MAX_DISTINCT: usize = 5;
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
                 Effort scaling: for a small task (1-3 tool calls) use ONE focused \
                 sub-agent; for a research sweep (3-10 calls) parallelize the independent \
                 calls with tasks[]; for a large or high-stakes task (>10 calls) escalate \
                 quality by delegating to a specialist rather than widening the batch. \
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
                    "output_format": {
                        "type": "string",
                        "description": "Optional: the exact shape the sub-agent's answer should take (e.g. 'JSON with fields x,y', '3 bullets'). Non-empty when present."
                    },
                    "task_boundaries": {
                        "type": "string",
                        "description": "Optional: what is in and out of scope for this subtask. Non-empty when present."
                    },
                    "artifact": {
                        "type": "string",
                        "description": "Optional: workspace-relative path the sub-agent should write its full deliverable to (via fs.write) and return as a pointer. Only agents with the fs.write grant may declare one; the path is sandbox-validated."
                    },
                    "tasks": {
                        "type": "array",
                        "description": "Parallel fan-out (001 A.6 collect mode): run several independent subtasks concurrently. Each item: {\"agent\", \"prompt\", \"description?\", \"independent?\", \"output_format?\", \"task_boundaries?\", \"artifact?\"}. Use for research sweeps / multi-file audits; results return as a batch (at most 8).",
                        "items": {
                            "type": "object",
                            "properties": {
                                "agent": {"type": "string"},
                                "prompt": {"type": "string"},
                                "description": {"type": "string"},
                                "independent": {"type": "boolean", "description": "true when this subtask needs no other subtask's output (pre-spawn gate)"},
                                "output_format": {"type": "string", "description": "Optional output shape for this subtask"},
                                "task_boundaries": {"type": "string", "description": "Optional scope bounds for this subtask"},
                                "artifact": {"type": "string", "description": "Optional sandbox-validated artifact output path"},
                                "fork_mode": {"type": "string", "enum": ["none", "knowledge", "history"], "description": "Per-task context fork (see top-level)"},
                                "model": {"type": "string", "description": "Per-task catalog-validated model hint (see top-level)"}
                            },
                            "required": ["agent", "prompt"]
                        }
                    },
                    "independent": {
                        "type": "boolean",
                        "description": "Single-task form: true when the parent needs no intermediate state (default true)"
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Admit the task(s) to the background registry and return immediately ({started, task_ids, background:true}); collect later with delegate.collect. Applies to both the single and tasks[] forms. Narrowed tool grants apply (no fs.delete/fs.move/fs.copy/shell.exec/shell.kill)."
                    },
                    "fork_mode": {
                        "type": "string",
                        "enum": ["none", "knowledge", "history"],
                        "description": "Per-task context fork (D25): none = fresh context; knowledge = parent's retrieved knowledge block; history = knowledge PLUS the parent conversation (tool-call noise stripped). Absent = session-wide context_inheritance default."
                    },
                    "model": {
                        "type": "string",
                        "description": "Per-task model hint (must exist in the configured provider catalog; at most 5 distinct per batch). Resolution: sensitivity routing > agent preference > this hint > default."
                    }
                },
                // D25 A6.3: the runtime tolerates BOTH forms (parse_requests
                // early-returns on tasks[]), so the schema must not lie —
                // single-task requires agent+prompt, pure fan-out requires
                // tasks[].
                "anyOf": [
                    { "required": ["agent", "prompt"] },
                    { "required": ["tasks"] }
                ]
            }),
            result_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {"type": "string"},
                    "response": {"type": "string"},
                    "rounds": {"type": "integer"},
                    "duration_ms": {"type": "integer"},
                    "description": {"type": "string"},
                    "reasoning": {"type": "string"},
                    "verification": {"type": "string"},
                    "artifact_path": {"type": "string"},
                    "summary": {"type": "string"}
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

        // W5.2 (D20 §1): the background flag is a TOOL ARGUMENT (no config
        // key). `false`/absent ⇒ the blocking path below byte-identical.
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let (depth, parent) = Self::chain_position();
        let rejected: Vec<Option<String>> = tasks
            .iter()
            .map(|task| self.validate_task(&parent, depth, &task.request))
            .collect();

        // Codex-style pre-spawn gates (T374): the bounded limit is hard and
        // enforced solely by `validate_task` (above); the rest are advisory
        // heuristics logged to the audit trail.
        let reports: Vec<GateReport> = tasks
            .iter()
            .map(|t| Self::evaluate_gates(&t.request, tasks.len()))
            .collect();

        // W5.2: background admissions share the SAME hard validate +
        // advisory gates (both already ran above), then return immediately.
        if background {
            return self.invoke_background(&tasks, depth, &parent).await;
        }

        // W11.5/T5 (SC-022): the BLOCKING path validates the hint set here
        // (catalog + ≤5 distinct per batch). Background admission validates
        // in invoke_background — one validation per path, not two.
        if let Err(e) = Self::validate_model_hints(&tasks) {
            return Ok(serde_json::json!({ "error": e }));
        }

        // Slot-ordered assembly: results[i] always corresponds to tasks[i],
        // whether the task was rejected or executed. Each result
        // also carries `task_index` (0-based) so any post-hoc reordering
        // (D10 weight ranking below) stays re-attributable by consumers —
        // positional pairing is only guaranteed while no weights are set.
        let mut slots: Vec<Option<serde_json::Value>> = Vec::with_capacity(tasks.len());
        let mut runnable: Vec<usize> = Vec::with_capacity(tasks.len());
        for (i, task) in tasks.iter().enumerate() {
            if let Some(error) = &rejected[i] {
                slots.push(Some(serde_json::json!({
                    "agent": task.request.agent,
                    "task_index": i,
                    "error": error
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
            // W11.5/T5 (SC-021): per-task fork_mode wins on the blocking
            // path too; `None` is byte-identical to the session-wide default.
            composed[i] = Some(self.compose_sub_session_forked(
                &tasks[i].request.agent,
                &tasks[i].request.prompt,
                depth,
                tasks[i].request.fork_mode,
            ));
        }
        self.append_gates_audit(&parent, depth, &tasks, &reports, &composed);

        for chunk in runnable.chunks(self.max_concurrent as usize) {
            let futures: Vec<_> = chunk
                .iter()
                .map(|&i| {
                    let composed = composed[i]
                        .take()
                        .expect("runnable slots are composed above");
                    let request = &tasks[i].request;
                    let contract = &tasks[i].contract;
                    let parent: &str = parent.as_str();
                    async move {
                        #[cfg(test)]
                        let _wave = tests::FanOutWaveGuard::enter(self, request.agent.as_str());
                        #[cfg(test)]
                        if let Some(delay) = self.test_wave_delay(request.agent.as_str()) {
                            tokio::time::sleep(delay).await;
                        }
                        self.run_single_composed(request, depth, parent, composed, contract, None)
                            .await
                    }
                })
                .collect();
            let results = futures::future::join_all(futures).await;
            self.append_outcomes_audit(&parent, depth, &tasks, chunk, &results);
            for (&i, outcome) in chunk.iter().zip(results) {
                slots[i] = Some(outcome);
            }
        }
        let mut results: Vec<serde_json::Value> = slots
            .into_iter()
            .enumerate()
            .map(|(i, slot)| {
                let mut value = slot.unwrap_or_else(
                    || serde_json::json!({ "error": "delegation produced no result" }),
                );
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("task_index".to_string(), serde_json::json!(i));
                }
                value
            })
            .collect();

        // D10: rank a multi-result batch by per-agent accuracy weight. A
        // single result is never touched, and an empty weight map (cold
        // start) leaves ordering byte-identical.
        if tasks.len() > 1 {
            crate::aggregate_weights::order_values_by_weight(&mut results, &self.weights);
        }

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
    use rig_compose::budget::AtomicTokenBudget;

    // D25 A6.3: the declared schema must accept BOTH forms the runtime
    // accepts — a top-level required:["agent","prompt"] rejects the pure
    // fan-out {tasks:[...]} payload the parse path early-returns on.
    #[test]
    fn delegate_task_schema_allows_pure_fan_out() {
        let budget = Arc::new(AtomicTokenBudget::new(10_000));
        let schema = tool_with(Arc::new(ZenWiring::new()), Vec::new(), budget).schema();
        let any_of = schema.args_schema["anyOf"]
            .as_array()
            .expect("top-level anyOf replaces bare required");
        let single = any_of
            .iter()
            .find(|arm| arm["required"] == serde_json::json!(["agent", "prompt"]));
        let fanout = any_of
            .iter()
            .find(|arm| arm["required"] == serde_json::json!(["tasks"]));
        assert!(single.is_some(), "single-task arm present: {any_of:?}");
        assert!(fanout.is_some(), "fan-out arm present: {any_of:?}");
        assert!(
            schema.args_schema.get("required").is_none(),
            "no bare top-level required may remain: {:?}",
            schema.args_schema.get("required")
        );
        assert_eq!(
            schema.args_schema["properties"]["tasks"]["items"]["required"],
            serde_json::json!(["agent", "prompt"]),
            "per-item schema keeps agent+prompt mandatory"
        );
    }

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
            artifact: None,
            fork_mode: None,
            model_hint: None,
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
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::clone(&budget) as Arc<dyn rig_compose::budget::TokenBudget>,
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

    /// D1-hazard regression pin (TODOS "D1-class Confidential async routing
    /// hazard"): a Confidential delegate sub-turn routes through
    /// `enforce_sensitivity → is_local_llm_available →
    /// OllamaProvider::health_check` (`reqwest::blocking`) on a LIVE async
    /// runtime — the nested-runtime panic class this test guards against.
    /// The dedicated-thread guard inside `health_check` must turn the probe
    /// into a local-unavailable routing failure, which `execute_round` fails
    /// open from (mock fallback): a served response or a structured error,
    /// never a panic, never a cloud route. Port 1 on loopback is
    /// unassigned — connection-refused is immediate and deterministic, and
    /// the provider pins `no_proxy` for loopback so a machine-level proxy
    /// cannot intercept (T217 lesson). `plan.execute` batches through this
    /// same `run_single` seam, so it inherits the guard by construction.
    #[tokio::test(flavor = "current_thread")]
    async fn confidential_delegate_routing_on_async_runtime_does_not_panic() {
        let config = zen_core::config::ZenConfig {
            default_provider: Some("ollama".to_string()),
            providers: std::collections::HashMap::from([(
                "ollama".to_string(),
                zen_core::config::ProviderConfig {
                    provider_type: Some("ollama".to_string()),
                    base_url: Some("http://127.0.0.1:1".to_string()),
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
            Arc::new(std::sync::Mutex::new(Sensitivity::Confidential)),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::new(AtomicTokenBudget::new(100_000)),
            Duration::from_secs(60),
            1,
            4,
        );
        let out = tool
            .invoke(serde_json::json!({ "agent": "Explore", "prompt": "confidential subtask" }))
            .await
            .unwrap();
        assert!(
            out["response"].as_str().is_some_and(|s| !s.is_empty())
                || out["error"].as_str().is_some(),
            "the Confidential sub-turn must yield a served fallback or a typed error, never panic: {out}"
        );
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

    /// G-13 (SC-001/SC-002) wave-overlap guard: records `enter` and bumps
    /// the in-flight counter at construction, records `exit` and decrements
    /// on drop — i.e. on every settle path (ok / error output), because the
    /// guard lives in the task-body scope. `pub(super)`: the cfg(test) call
    /// site in the fan-out chunk body (parent module) names this type.
    pub(super) struct FanOutWaveGuard<'a> {
        tool: &'a DelegateTaskTool,
        agent: String,
    }

    impl<'a> FanOutWaveGuard<'a> {
        pub(super) fn enter(tool: &'a DelegateTaskTool, agent: &str) -> Self {
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

    impl std::ops::Drop for FanOutWaveGuard<'_> {
        fn drop(&mut self) {
            let _ = self
                .tool
                .test_in_flight
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            self.tool.test_log_wave_event(&self.agent, "exit");
        }
    }

    /// G-13 (SC-001/SC-002): sibling of
    /// `max_concurrent_chunks_preserve_slot_order` asserting the full wave
    /// contract over a 5-task fan-out at width 2. SC-001: peak in-flight
    /// counted from enter/exit events (never elapsed time) is ≥2 and ≤
    /// max_concurrent. SC-002: wave N+1 enters only after every wave-N
    /// task exited. Slot ordering itself stays pinned by the existing
    /// test, untouched.
    #[tokio::test]
    async fn fan_out_waves_barriers_and_peak_in_flight_sc001_sc002() {
        let tool = tool_with_concurrency(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
            1,
            2,
        );
        let agents = ["Explore", "Librarian", "Hephaestus", "Atlas", "Junior"];
        for agent in agents {
            tool.inject_test_wave_delay(agent, 50);
        }
        let tasks: Vec<serde_json::Value> = agents
            .iter()
            .map(|a| serde_json::json!({ "agent": a, "prompt": format!("wave work for {a}") }))
            .collect();
        let out = tool
            .invoke(serde_json::json!({ "tasks": tasks }))
            .await
            .unwrap();
        let results = out["results"].as_array().expect("batch shape: {out}");
        assert_eq!(results.len(), 5, "all five subtasks return: {out}");
        assert!(
            results
                .iter()
                .all(|r| r["response"].as_str().is_some_and(|s| !s.is_empty())),
            "every result carries a response: {out}"
        );

        // SC-001: peak in-flight from enter/exit events, never time.
        let peak = tool.test_peak_in_flight();
        assert!(
            peak >= 2,
            "SC-001: width-2 waves must observably overlap (peak={peak})"
        );
        assert!(
            peak <= 2,
            "SC-001: in-flight must never exceed max_concurrent=2 (peak={peak})"
        );

        // SC-002: waves are barriers — wave N+1 enters only after every
        // wave-N task exited. Waves are the width-2 chunks over the
        // runnable order (all five tasks are valid here).
        let events = tool.test_wave_events();
        let pos_of = |agent: &str, event: &str| {
            events
                .iter()
                .position(|(a, e)| a == agent && *e == event)
                .unwrap_or_else(|| panic!("missing {event} event for {agent}: {events:?}"))
        };
        let waves: Vec<&[&str]> = agents.chunks(2).collect();
        for pair in waves.windows(2) {
            let last_exit_prev = pair[0].iter().map(|a| pos_of(a, "exit")).max().unwrap();
            let first_enter_next = pair[1].iter().map(|a| pos_of(a, "enter")).min().unwrap();
            assert!(
                last_exit_prev < first_enter_next,
                "SC-002: wave barrier violated ({events:?})"
            );
        }
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
        // Public sensitivity keeps this test orthogonal to routing: the
        // D1-class Confidential async-routing hazard is fixed (dedicated-
        // thread guard in OllamaProvider::health_check) and pinned by
        // `confidential_delegate_routing_on_async_runtime_does_not_panic`.
        DelegateTaskTool::new(
            Arc::new(ZenWiring::new()),
            mock_router(),
            Vec::new(),
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Public)),
            knowledge,
            Arc::new(std::sync::Mutex::new(Vec::new())),
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
            Arc::new(std::sync::Mutex::new(Vec::new())),
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

    // ── W1.2 (D9): delegation contract + report schema ────────────────

    #[test]
    fn parse_requests_old_payload_decodes_with_empty_contract() {
        let old = serde_json::json!({ "agent": "Explore", "prompt": "x" });
        let parsed = DelegateTaskTool::parse_requests(&old).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].request.agent, "Explore");
        assert_eq!(parsed[0].request.prompt, "x");
        assert!(parsed[0].contract.output_format.is_none());
        assert!(parsed[0].contract.task_boundaries.is_none());
    }

    #[test]
    fn parse_requests_round_trips_contract_fields() {
        let new = serde_json::json!({
            "agent": "Explore",
            "prompt": "x",
            "output_format": "3 bullets",
            "task_boundaries": "only the search layer"
        });
        let parsed = DelegateTaskTool::parse_requests(&new).unwrap();
        assert_eq!(
            parsed[0].contract.output_format.as_deref(),
            Some("3 bullets")
        );
        assert_eq!(
            parsed[0].contract.task_boundaries.as_deref(),
            Some("only the search layer")
        );
    }

    #[test]
    fn parse_requests_carries_per_item_contract_in_fanout() {
        let fanout = serde_json::json!({
            "tasks": [
                { "agent": "Explore", "prompt": "a", "output_format": "json" },
                { "agent": "Librarian", "prompt": "b" }
            ]
        });
        let parsed = DelegateTaskTool::parse_requests(&fanout).unwrap();
        assert_eq!(parsed[0].contract.output_format.as_deref(), Some("json"));
        assert!(parsed[1].contract.is_empty());
    }

    #[test]
    fn contract_fields_must_be_non_empty_when_present() {
        let empty = serde_json::json!({
            "agent": "Explore", "prompt": "x", "output_format": "   "
        });
        assert!(
            DelegateTaskTool::parse_requests(&empty)
                .unwrap_err()
                .contains("output_format"),
            "blank output_format is rejected"
        );
        let wrong_type =
            serde_json::json!({ "agent": "Explore", "prompt": "x", "task_boundaries": 7 });
        assert!(
            DelegateTaskTool::parse_requests(&wrong_type)
                .unwrap_err()
                .contains("task_boundaries"),
            "non-string task_boundaries is rejected"
        );
    }

    #[test]
    fn delegate_contract_serde_is_additive() {
        let empty: DelegateContract = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, DelegateContract::default());

        let contract = DelegateContract {
            output_format: Some("json".to_string()),
            task_boundaries: None,
        };
        let encoded = serde_json::to_string(&contract).unwrap();
        assert!(
            !encoded.contains("task_boundaries"),
            "None fields are omitted: {encoded}"
        );
        let decoded: DelegateContract = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, contract);
    }

    #[test]
    fn compose_sub_prompt_labels_contract_and_requests_report() {
        let req = DelegateRequest {
            agent: "Explore".into(),
            prompt: "do it".into(),
            description: None,
            independent: true,
            artifact: None,
            fork_mode: None,
            model_hint: None,
        };
        let bare = compose_sub_prompt(&req, &DelegateContract::default());
        assert!(bare.starts_with("do it"));
        assert!(bare.contains("REASONING:"), "{bare}");
        assert!(
            !bare.contains("OUTPUT FORMAT:"),
            "no contract section when empty"
        );

        let contract = DelegateContract {
            output_format: Some("3 bullets".into()),
            task_boundaries: Some("search only".into()),
        };
        let full = compose_sub_prompt(&req, &contract);
        assert!(full.contains("OUTPUT FORMAT:\n3 bullets"), "{full}");
        assert!(full.contains("TASK BOUNDARIES:\nsearch only"), "{full}");
    }

    #[test]
    fn compose_sub_prompt_instructs_artifact_output_only_when_declared() {
        let base = DelegateRequest {
            agent: "Hephaestus".into(),
            prompt: "build it".into(),
            description: None,
            independent: true,
            fork_mode: None,
            model_hint: None,
            artifact: None,
        };
        let bare = compose_sub_prompt(&base, &DelegateContract::default());
        assert!(
            !bare.contains("ARTIFACT OUTPUT"),
            "no artifact ⇒ no artifact section: {bare}"
        );

        let with_artifact = DelegateRequest {
            artifact: Some("vault/resources/report.md".into()),
            ..base
        };
        let prompt = compose_sub_prompt(&with_artifact, &DelegateContract::default());
        assert!(prompt.contains("ARTIFACT OUTPUT"), "{prompt}");
        assert!(
            prompt.contains("vault/resources/report.md"),
            "the declared path must reach the sub-agent brief: {prompt}"
        );
    }

    #[test]
    fn bounded_summary_truncates_at_the_cap() {
        let short = bounded_summary("done");
        assert_eq!(short, "done");
        let long = "x".repeat(ARTIFACT_SUMMARY_MAX_CHARS + 10);
        let summary = bounded_summary(&long);
        assert!(summary.starts_with(&"x".repeat(ARTIFACT_SUMMARY_MAX_CHARS)));
        assert!(summary.ends_with("…[truncated]"));
    }

    #[test]
    fn split_report_markers_lifts_and_removes_report_lines() {
        let (clean, reasoning, verification) =
            split_report_markers("answer body\nREASONING: because\nVERIFICATION: ran tests");
        assert_eq!(clean, "answer body");
        assert_eq!(reasoning.as_deref(), Some("because"));
        assert_eq!(verification.as_deref(), Some("ran tests"));

        let (unchanged, reasoning, verification) = split_report_markers("just prose");
        assert_eq!(unchanged, "just prose", "no markers ⇒ byte-identical");
        assert!(reasoning.is_none() && verification.is_none());
    }

    #[test]
    fn delegate_description_pins_effort_scaling_and_result_contract() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let schema = tool.schema();
        assert!(
            schema.description.contains("Effort scaling"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("parallelize"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("escalate quality"),
            "{}",
            schema.description
        );
        assert_eq!(
            schema.args_schema["properties"]["output_format"]["type"],
            "string"
        );
        assert_eq!(
            schema.args_schema["properties"]["task_boundaries"]["type"],
            "string"
        );
        assert_eq!(
            schema.args_schema["properties"]["tasks"]["items"]["properties"]["output_format"]["type"],
            "string"
        );
    }

    // ── W1.3 (D10): aggregation ordering ──────────────────────────────

    #[tokio::test]
    async fn cold_start_batch_order_is_byte_identical() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({ "tasks": [
                { "agent": "Explore", "prompt": "a" },
                { "agent": "Librarian", "prompt": "b" }
            ]}))
            .await
            .unwrap();
        let results = out["results"].as_array().expect("batch shape");
        assert_eq!(
            results[0]["agent"], "Explore",
            "empty weights ⇒ input order"
        );
        assert_eq!(results[1]["agent"], "Librarian");
    }

    #[tokio::test]
    async fn injected_weights_rank_batch_by_accuracy_without_dropping() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        )
        .with_weights(HashMap::from([
            ("Explore".to_string(), 0.2),
            ("Librarian".to_string(), 0.9),
        ]));
        let out = tool
            .invoke(serde_json::json!({ "tasks": [
                { "agent": "Explore", "prompt": "a" },
                { "agent": "Librarian", "prompt": "b" }
            ]}))
            .await
            .unwrap();
        let results = out["results"].as_array().expect("batch shape");
        assert_eq!(results.len(), 2, "ordering never drops results: {out}");
        assert_eq!(
            results[0]["agent"], "Librarian",
            "higher weight first: {out}"
        );
        assert_eq!(results[1]["agent"], "Explore");
    }

    // ── G1-09: accurate dispatch-failure labels ───────────────────────

    #[test]
    fn dispatch_error_feedback_distinguishes_blocked_from_failed() {
        use rig_compose::registry::KernelError;
        let terminated = dispatch_error_feedback(&KernelError::ToolDispatchTerminated(
            "approval denied".into(),
        ));
        assert!(
            terminated.starts_with("tool dispatch blocked"),
            "{terminated}"
        );
        let budget = dispatch_error_feedback(&KernelError::BudgetFailed("cap".into()));
        assert!(budget.starts_with("tool dispatch blocked"), "{budget}");
        let failed = dispatch_error_feedback(&KernelError::ToolFailed("boom".into()));
        assert!(failed.starts_with("tool failed:"), "{failed}");
        let missing = dispatch_error_feedback(&KernelError::ToolNotFound("nope".into()));
        assert!(missing.starts_with("tool failed:"), "{missing}");
    }

    // ====================================================================
    // W5.2 — background admissions (D20 §1/§6/§7/§8/§10 + D25 SC-021/022/024)
    // ====================================================================

    fn background_tool(
        wiring: Arc<ZenWiring>,
        budget: Arc<AtomicTokenBudget>,
        max_depth: u32,
    ) -> Arc<DelegateTaskTool> {
        let tool = Arc::new(DelegateTaskTool::new(
            wiring,
            mock_router(),
            Vec::new(),
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Public)),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            budget,
            Duration::from_secs(300),
            max_depth,
            4,
        ));
        tool.set_self_arc(Arc::clone(&tool));
        tool
    }

    #[tokio::test]
    async fn background_absent_flag_keeps_blocking_shape_byte_identical() {
        // Flag absent ⇒ the blocking path: single-task result carries
        // `response`, never `task_ids`/`background`.
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({"agent": "Explore", "prompt": "say hi"}))
            .await
            .unwrap();
        assert!(out.get("response").is_some(), "blocking shape: {out}");
        assert!(out.get("background").is_none());
        assert!(out.get("task_ids").is_none());
        // Explicit `background: false` is the same path.
        let out = tool
            .invoke(
                serde_json::json!({"agent": "Explore", "prompt": "say hi", "background": false}),
            )
            .await
            .unwrap();
        assert!(
            out.get("response").is_some(),
            "explicit false = blocking: {out}"
        );
    }

    #[tokio::test]
    async fn background_admission_returns_started_shape_and_registers_task() {
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118);
        // spawn_async writes under user_root, so isolate it.
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();
        let tool = background_tool(
            Arc::new(ZenWiring::new()),
            Arc::new(AtomicTokenBudget::new(100_000)),
            1,
        );
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore",
                "prompt": "research briefly",
                "background": true
            }))
            .await
            .unwrap();
        assert_eq!(out["background"], serde_json::json!(true), "{out}");
        assert_eq!(out["started"], serde_json::json!(1), "{out}");
        let ids = out["task_ids"].as_array().expect("task_ids array");
        assert_eq!(ids.len(), 1);
        let task_id = ids[0].as_str().expect("task_id string").to_string();
        let snap = zen_plugin::task_registry::registry().snapshot(&task_id);
        assert!(snap.is_some(), "task registered in the TaskRegistry");
        zen_plugin::task_registry::registry()
            .kill(&task_id)
            .expect("cleanup");
    }

    #[tokio::test]
    async fn background_over_concurrency_cap_is_structured_rejection() {
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118);
        // spawn_async writes under user_root, so isolate it.
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();
        // The admission limit reads [agentic.background] max_concurrent
        // through the 5-layer config; the documented env override pins it
        // to 1 so one filler occupies the slot.
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_BACKGROUND_MAX_CONCURRENT", "1");
        }
        zen_core::config::invalidate_config_cache();
        // Fill the registry to its [agentic.background] ceiling, then admit.
        let limits = zen_plugin::task_registry::BackgroundLimits {
            max_wall_secs: 60,
            max_concurrent: 1,
            output_cap_bytes: zen_plugin::task_registry::OUTPUT_CAP_BYTES,
        };
        let filler = zen_plugin::task_registry::registry()
            .spawn_async(
                "c0ffee00-0000-4000-8000-000000000001",
                "sess-cap",
                "Explore",
                None,
                &limits,
                async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Ok(serde_json::json!({}))
                },
            )
            .expect("filler admitted");
        let tool = background_tool(
            Arc::new(ZenWiring::new()),
            Arc::new(AtomicTokenBudget::new(100_000)),
            1,
        );
        let lim = tool.background_limits();
        assert_eq!(
            lim.max_concurrent, 1,
            "env override must reach admission limits (got {})",
            lim.max_concurrent
        );
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore",
                "prompt": "second task",
                "background": true
            }))
            .await
            .unwrap();
        assert_eq!(
            out["started"],
            serde_json::json!(0),
            "over-cap admits nothing: {out}"
        );
        let admissions = out["admissions"].as_array().expect("admissions");
        assert!(
            admissions[0]["error"]
                .as_str()
                .unwrap()
                .contains("max_concurrent"),
            "rejection names the limit: {out}"
        );
        drop(filler);
    }

    #[tokio::test]
    async fn background_under_budget_is_structured_rejection() {
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118);
        // spawn_async writes under user_root, so isolate it.
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();
        // A 10-token pool cannot fund the admission reserve (prompt/4 + 1024).
        let tool = background_tool(
            Arc::new(ZenWiring::new()),
            Arc::new(AtomicTokenBudget::new(10)),
            1,
        );
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore",
                "prompt": "a reasonably sized brief that needs a real reserve",
                "background": true
            }))
            .await
            .unwrap();
        assert_eq!(
            out["started"],
            serde_json::json!(0),
            "under-budget admits nothing: {out}"
        );
        let admissions = out["admissions"].as_array().expect("admissions");
        assert!(
            admissions[0]["error"]
                .as_str()
                .unwrap()
                .contains("insufficient token budget"),
            "rejection names the reserve: {out}"
        );
    }

    #[tokio::test]
    async fn background_depth_cap_rejected_before_registry_entry() {
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118);
        // spawn_async writes under user_root, so isolate it.
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();
        // max_depth=0 ⇒ child depth 1 exceeds the cap: validate_task rejects
        // pre-admission; no budget consumed, no registry entry.
        let tool = background_tool(
            Arc::new(ZenWiring::new()),
            Arc::new(AtomicTokenBudget::new(100_000)),
            0,
        );
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore",
                "prompt": "deep brief",
                "background": true
            }))
            .await
            .unwrap();
        assert_eq!(out["started"], serde_json::json!(0), "{out}");
        let admissions = out["admissions"].as_array().expect("admissions");
        assert!(
            admissions[0]["error"]
                .as_str()
                .unwrap()
                .contains("depth cap"),
            "depth-cap rejection surfaces: {out}"
        );
        assert!(
            zen_plugin::task_registry::registry().live_count() == 0,
            "no registry entry for a depth-capped admission"
        );
    }

    #[test]
    fn fork_mode_absent_matches_session_default_byte_identically() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        // knowledge gate closed (empty inheritance budget ⇒ None):
        let (session_absent, sub_absent) = tool.compose_sub_session_forked("Explore", "p", 0, None);
        let (session_none, sub_none) =
            tool.compose_sub_session_forked("Explore", "p", 0, Some(ForkMode::None));
        assert_eq!(session_absent.knowledge.len(), session_none.knowledge.len());
        assert_eq!(sub_absent.budget_chars, sub_none.budget_chars);
    }

    #[test]
    fn history_fork_strips_tool_payloads_from_parent_conversation() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        {
            let mut mirror = tool.history.lock().unwrap();
            mirror.push(crate::delegate_task::ParentHistoryMessage {
                role: "user".into(),
                content: "find the config".into(),
            });
            mirror.push(crate::delegate_task::ParentHistoryMessage {
                role: "assistant".into(),
                content: "reading it.\n```json\n{\"tool\": \"fs.read\", \"args\": {}}\n```\nfound the config key.".into(),
            });
        }
        let (session, _) =
            tool.compose_sub_session_forked("Explore", "p", 0, Some(ForkMode::History));
        let joined: String = session
            .conversation
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("find the config"),
            "user prose forks: {joined}"
        );
        assert!(
            joined.contains("found the config key"),
            "assistant prose forks: {joined}"
        );
        assert!(
            !joined.contains("fs.read") && !joined.contains("```json"),
            "tool-call noise must NOT fork (SC-021 load-bearing): {joined}"
        );
        // none/knowledge forks never see the conversation at all.
        let (session_k, _) =
            tool.compose_sub_session_forked("Explore", "p", 0, Some(ForkMode::Knowledge));
        assert!(session_k.conversation.is_empty());
    }

    #[test]
    fn background_grant_narrowing_drops_enumerated_tools() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        // The Sisyphus profile carries the widest grant surface.
        let widened = tool
            .build_sub_agent("Sisyphus", false)
            .expect("unwidened build");
        let narrowed = tool
            .build_sub_agent_with_grants("Sisyphus", false, true)
            .expect("narrowed build");
        let widened_names: Vec<String> = widened
            .generic
            .tools()
            .schemas()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        let narrowed_names: std::collections::HashSet<String> = narrowed
            .generic
            .tools()
            .schemas()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        for stripped in crate::delegate_task::BACKGROUND_NARROWED_TOOLS {
            if widened_names.iter().any(|n| n == stripped) {
                assert!(
                    !narrowed_names.contains(stripped),
                    "narrowed admissions must drop `{stripped}` (SC-024)"
                );
            }
        }
    }

    /// W12.1 (review A1 E2E pin): the PRODUCTION construction entry must
    /// honor the `BACKGROUND_TASK` task-local. The pre-fix world hardcoded
    /// `narrowed: false` while only the helper seam carried the flag — tests
    /// stayed green while production background sub-agents kept the
    /// destructive five (the masked-dead-code lesson). This pin builds
    /// through `build_sub_agent` under the exact task-local state
    /// `invoke_background` establishes around its spawned future and asserts
    /// the BUILT toolset; flag-absent (foreground) keeps the full surface
    /// byte-identical (W5.2 contract).
    #[tokio::test]
    async fn background_task_scope_narrows_production_entry_toolset() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        // Foreground byte-identity: no BACKGROUND_TASK ⇒ widest surface.
        // Pin precondition: Sisyphus (widest grants) over ZenWiring::new()
        // must actually carry the five, or the narrowing half proves
        // nothing.
        let foreground = tool
            .build_sub_agent("Sisyphus", false)
            .expect("foreground build");
        let foreground_names: std::collections::HashSet<String> = foreground
            .generic
            .tools()
            .schemas()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        for stripped in BACKGROUND_NARROWED_TOOLS {
            assert!(
                foreground_names.contains(stripped),
                "pin precondition: foreground Sisyphus must carry `{stripped}`"
            );
        }

        // Production entry under the background scope — the same task-local
        // state a spawned background future runs under (invoke_background).
        let narrowed = zen_plugin::tools::approval_hook::BACKGROUND_TASK
            .scope(true, async {
                tool.build_sub_agent("Sisyphus", false)
                    .expect("scoped production build")
            })
            .await;
        let narrowed_names: std::collections::HashSet<String> = narrowed
            .generic
            .tools()
            .schemas()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        for stripped in BACKGROUND_NARROWED_TOOLS {
            assert!(
                !narrowed_names.contains(stripped),
                "background sub-agents built through the PRODUCTION entry \
                 must lose `{stripped}` (SC-024/W12.1)"
            );
        }
        // Narrowing removes ONLY the enumerated five — no collateral loss.
        let five: std::collections::HashSet<&str> =
            BACKGROUND_NARROWED_TOOLS.iter().copied().collect();
        let expected: std::collections::HashSet<String> = foreground_names
            .iter()
            .filter(|n| !five.contains(n.as_str()))
            .cloned()
            .collect();
        assert_eq!(
            narrowed_names, expected,
            "narrowing must remove exactly the enumerated five, nothing else"
        );
    }

    /// W12.4 (review N4 pin): consumption must equal the REAL total exactly
    /// once. rig-compose `record_usage` sums its prompt+completion args, so
    /// the historical `(actual, actual)` billed every successful sub-turn at
    /// 2× its size — effective capacity halved. The pin drives the blocking
    /// production path with the deterministic mock and compares
    /// `tokens_consumed()` against the exact composition the production
    /// site bills (`compose_sub_prompt` bytes/4 + final-response bytes/4).
    #[tokio::test]
    async fn record_usage_bills_real_total_exactly_once() {
        let budget = Arc::new(AtomicTokenBudget::new(1_000_000));
        let tool = tool_with(Arc::new(ZenWiring::new()), Vec::new(), Arc::clone(&budget));
        let prompt_text = "measure the billed total exactly once";
        let out = tool
            .invoke(serde_json::json!({ "agent": "Explore", "prompt": prompt_text }))
            .await
            .unwrap();
        let response = out["response"]
            .as_str()
            .expect("ok-shaped result carries the sub-agent reply");
        let req = DelegateRequest {
            agent: "Explore".into(),
            prompt: prompt_text.into(),
            description: None,
            independent: true,
            artifact: None,
            fork_mode: None,
            model_hint: None,
        };
        let composed = compose_sub_prompt(&req, &DelegateContract::default());
        let expected = (composed.len() / 4 + response.len() / 4) as u64;
        assert_eq!(
            budget.tokens_consumed().await,
            expected,
            "consumption must equal the real total exactly once — a \
             (actual, actual) call would bill {expected}×2 (W12.4)"
        );
        assert_eq!(
            budget.available(),
            budget.capacity() - expected,
            "the reconciled reservation must leave availability at \
             capacity minus the real total"
        );
    }

    /// W12.3 (review A3 pin): a nested background admission (depth ≥ 2)
    /// must record the REAL owning session id, never "local". Task-locals
    /// do not cross `tokio::spawn` — pre-W12.3, the spawned future lost
    /// `SESSION_SCOPE`, so any `delegate.task background` admitted inside a
    /// background sub-turn attributed "local" and the owning session's
    /// completion push never saw the task. The pin drives BOTH admissions
    /// through the production `invoke` entry: depth-1 exactly as the
    /// orchestrator dispatches (inside `SESSION_SCOPE`), then the depth-2
    /// admission inside the scope shape `invoke_background` wraps around
    /// its spawned future (BACKGROUND_TASK × SESSION_SCOPE × the
    /// DELEGATE_DEPTH/PARENT chain state `run_single_composed` establishes
    /// around its loop). Planner→Specialist keeps the spawn-hierarchy gate
    /// open (Prometheus parent), and `max_depth = 2` admits child_depth 2.
    /// Honest seam note: the mock LLM cannot emit a nested delegate JSON,
    /// so the depth-2 admission is entered at the seam the production
    /// future provides rather than through a second LLM round — the raw-
    /// spawn contrast below pins the failure mode the re-entry closes.
    #[tokio::test]
    async fn nested_background_admission_inside_spawned_future_attributes_real_owning_session() {
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118);
        // spawn_async writes under user_root, so isolate it.
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();

        let tool = background_tool(
            Arc::new(ZenWiring::new()),
            Arc::new(AtomicTokenBudget::new(1_000_000)),
            2,
        );

        // Depth-1 admission through the production entry, exactly as the
        // orchestrator runs it: dispatch inside the owning session's
        // SESSION_SCOPE. invoke_background captures the session id here.
        let d1 = zen_plugin::task_registry::SESSION_SCOPE
            .scope("sess-owner".to_string(), async {
                tool.invoke(serde_json::json!({
                    "tasks": [{ "agent": "Explore", "prompt": "depth-1 wave" }],
                    "background": true
                }))
                .await
                .unwrap()
            })
            .await;
        assert_eq!(
            d1["started"],
            serde_json::json!(1),
            "depth-1 admitted: {d1}"
        );
        let d1_id = d1["task_ids"].as_array().expect("task_ids")[0]
            .as_str()
            .expect("task id string")
            .to_string();
        let d1_snap = zen_plugin::task_registry::registry()
            .snapshot(&d1_id)
            .expect("depth-1 task registered");
        assert_eq!(d1_snap.session_id, "sess-owner", "depth-1 attribution");

        // Contrast — the raw failure mode: a plain tokio::spawn drops the
        // task-local, which is precisely why the fix re-enters the scope.
        let seen = Arc::new(Mutex::new(None::<String>));
        {
            let seen = Arc::clone(&seen);
            zen_plugin::task_registry::SESSION_SCOPE
                .scope("sess-owner".to_string(), async move {
                    tokio::spawn(async move {
                        *seen.lock().unwrap() =
                            Some(zen_plugin::task_registry::current_session_scope());
                    })
                    .await
                    .unwrap();
                })
                .await;
        }
        assert_eq!(
            *seen.lock().unwrap(),
            Some("local".to_string()),
            "task-locals do NOT cross tokio::spawn — the state W12.3 re-enters"
        );

        // Depth-2 admission inside the production future-body scope shape:
        // BACKGROUND_TASK(true) × SESSION_SCOPE(owner) × depth chain, the
        // exact nesting invoke_background establishes around the spawned
        // run_single_background future (Planner parent keeps spawn_allowed
        // true; child_depth 2 ≤ max_depth 2).
        let d2 = zen_plugin::tools::approval_hook::BACKGROUND_TASK
            .scope(
                true,
                zen_plugin::task_registry::SESSION_SCOPE.scope(
                    "sess-owner".to_string(),
                    DELEGATE_DEPTH.scope(Cell::new(1), {
                        DELEGATE_PARENT.scope(RefCell::new(Some("Prometheus".to_string())), async {
                            tool.invoke(serde_json::json!({
                                "tasks": [{ "agent": "Explore",
                                            "prompt": "nested depth-2 wave" }],
                                "background": true
                            }))
                            .await
                            .unwrap()
                        })
                    }),
                ),
            )
            .await;
        assert_eq!(
            d2["started"],
            serde_json::json!(1),
            "depth-2 admitted inside the future scope shape: {d2}"
        );
        let d2_id = d2["task_ids"].as_array().expect("task_ids")[0]
            .as_str()
            .expect("task id string")
            .to_string();
        let d2_snap = zen_plugin::task_registry::registry()
            .snapshot(&d2_id)
            .expect("depth-2 task registered");
        assert_eq!(
            d2_snap.session_id, "sess-owner",
            "nested depth-2 admission must attribute the REAL owning \
             session, never \"local\" (W12.3)"
        );

        // Consumer contract (D4): the owning session's completion push
        // surfaces BOTH tasks once they reach terminal states.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let done = |id: &str| {
                zen_plugin::task_registry::registry()
                    .snapshot(id)
                    .map(|s| s.status.is_terminal())
                    .unwrap_or(false)
            };
            if done(&d1_id) && done(&d2_id) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "background futures must complete within the pin deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut notified: Vec<String> = zen_plugin::task_registry::registry()
            .take_status_changes("sess-owner")
            .iter()
            .map(|s| s.task_id.clone())
            .collect();
        notified.sort();
        let mut expected_ids = vec![d1_id.clone(), d2_id.clone()];
        expected_ids.sort();
        assert_eq!(
            notified, expected_ids,
            "the owning session's push cursor must surface both admissions"
        );
    }

    #[test]
    fn model_hint_validation_rejects_unknown_catalog_and_sixth_distinct() {
        let tasks_unknown = |hint: &str| {
            vec![ParsedDelegate {
                request: DelegateRequest {
                    agent: "Explore".into(),
                    prompt: "p".into(),
                    description: None,
                    independent: true,
                    artifact: None,
                    fork_mode: None,
                    model_hint: Some(hint.to_string()),
                },
                contract: DelegateContract::default(),
            }]
        };
        let six: Vec<ParsedDelegate> = (0..6)
            .map(|i| ParsedDelegate {
                request: DelegateRequest {
                    agent: "Explore".into(),
                    prompt: "p".into(),
                    description: None,
                    independent: true,
                    artifact: None,
                    fork_mode: None,
                    model_hint: Some(format!("model-{i}")),
                },
                contract: DelegateContract::default(),
            })
            .collect();
        if let Some(catalog) = DelegateTaskTool::model_catalog() {
            // A hint guaranteed absent from any real catalog.
            let unknown = "definitely-not-a-model-xyz";
            assert!(!catalog.contains(unknown));
            let err = DelegateTaskTool::validate_model_hints(&tasks_unknown(unknown)).unwrap_err();
            assert!(
                err.contains("not in the configured provider catalog"),
                "{err}"
            );
        } else {
            // Catalog unavailable ⇒ fail-closed rejection.
            assert!(DelegateTaskTool::validate_model_hints(&tasks_unknown("x")).is_err());
        }
        let err = DelegateTaskTool::validate_model_hints(&six).unwrap_err();
        assert!(
            err.contains("at most 5"),
            "sixth distinct override rejected: {err}"
        );
    }

    #[test]
    fn model_hint_resolution_order_prefers_profile_over_hint() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let mut profile = crate::registry::DefaultAgentRegistry::new()
            .find_by_name("Explore")
            .cloned()
            .expect("Explore profile");
        // Explicit preference ⇒ hint suppressed (SC-022 order).
        profile.llm_preferences = vec![LlmPreference::LocalOnly];
        assert_eq!(
            tool.resolve_model_hint(Some("model-a"), Some(&profile)),
            ModelSource::Profile
        );
        // Any-only ⇒ hint wins.
        profile.llm_preferences = vec![LlmPreference::Any];
        assert_eq!(
            tool.resolve_model_hint(Some("model-a"), Some(&profile)),
            ModelSource::Hint
        );
        // No hint ⇒ default.
        assert_eq!(
            tool.resolve_model_hint(None, Some(&profile)),
            ModelSource::Default
        );
    }

    /// W11.10/T10 (G3a): `fork_mode: knowledge` POSITIVELY inherits the
    /// parent knowledge block content (the gate-ON path, per-task addressable).
    #[test]
    fn fork_mode_knowledge_inherits_parent_knowledge_content() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        set_inheritance_env(Some(dir.path()), Some("1"));
        let knowledge: SharedKnowledge =
            Arc::new(std::sync::Mutex::new(vec![note("parent knowledge alpha")]));
        let tool = tool_with_knowledge(Arc::new(AtomicTokenBudget::new(100_000)), knowledge);
        let (session, sub) =
            tool.compose_sub_session_forked("Explore", "p", 0, Some(ForkMode::Knowledge));
        assert!(
            session
                .knowledge
                .iter()
                .any(|n| n.content.contains("parent knowledge alpha")),
            "knowledge fork must inherit the parent block: {:?}",
            session.knowledge
        );
        assert!(sub.knowledge_chars() > 0, "inherited chars are nonzero");
        set_inheritance_env(None, None);
    }

    /// W11.10/T10 (G3b): an invalid fork_mode string is a structured
    /// rejection, never a silent default.
    #[test]
    fn fork_mode_parse_rejects_invalid_string() {
        assert!(ForkMode::parse("bogus").is_err());
        assert!(ForkMode::parse("").is_err());
        assert!(ForkMode::parse("none").is_ok());
        assert!(ForkMode::parse("knowledge").is_ok());
        assert!(ForkMode::parse("history").is_ok());
    }

    #[tokio::test]
    async fn delegate_rejects_invalid_fork_mode_string() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore", "prompt": "p", "fork_mode": "bogus"
            }))
            .await
            .unwrap();
        assert!(
            out["error"].as_str().unwrap().contains("fork_mode"),
            "invalid fork_mode surfaces a diagnostic: {out}"
        );
    }

    /// W11.1/T1 (D30, F1+OV-1) race pin: a Confidential TurnContext wins over
    /// a Public shared cell (a concurrent session's last write) — the
    /// composition classifies under the TURN's policy, so no cloud leak.
    #[tokio::test]
    async fn turn_context_sensitivity_overrides_shared_cell() {
        let tool = tool_with(
            Arc::new(ZenWiring::new()),
            Vec::new(),
            Arc::new(AtomicTokenBudget::new(100_000)),
        );
        // The shared cell holds Public (the racy last-writer-wins value).
        let ctx = zen_core::turn_context::TurnContext {
            sensitivity: Sensitivity::Confidential,
            knowledge: Vec::new(),
            history: Vec::new(),
        };
        let (session, _sub) = zen_core::turn_context::TURN_CONTEXT
            .scope(ctx, async {
                tool.compose_sub_session_forked("Explore", "p", 0, Some(ForkMode::None))
            })
            .await;
        assert_eq!(
            session.sensitivity_policy,
            Sensitivity::Confidential,
            "the turn's policy must win over the concurrent-session cell"
        );
        // Outside any turn scope the cell fallback still applies.
        let (fallback, _) =
            tool.compose_sub_session_forked("Explore", "p", 0, Some(ForkMode::None));
        assert_eq!(fallback.sensitivity_policy, Sensitivity::Public);
    }

    /// W11.1/T1 (D30) race pin: turn-scoped knowledge wins over a foreign
    /// shared-cell block — no cross-session knowledge injection.
    #[tokio::test]
    async fn turn_context_knowledge_overrides_foreign_cell() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        set_inheritance_env(Some(dir.path()), Some("1"));
        let foreign: SharedKnowledge = Arc::new(std::sync::Mutex::new(vec![note(
            "foreign session knowledge",
        )]));
        let tool = tool_with_knowledge(Arc::new(AtomicTokenBudget::new(100_000)), foreign);
        let ctx = zen_core::turn_context::TurnContext {
            sensitivity: Sensitivity::Private,
            knowledge: vec![note("turn-scoped knowledge")],
            history: Vec::new(),
        };
        let (session, _) = zen_core::turn_context::TURN_CONTEXT
            .scope(ctx, async {
                tool.compose_sub_session_forked("Explore", "p", 0, Some(ForkMode::Knowledge))
            })
            .await;
        assert!(
            session
                .knowledge
                .iter()
                .any(|n| n.content.contains("turn-scoped knowledge")),
            "the turn's knowledge must be inherited"
        );
        assert!(
            !session
                .knowledge
                .iter()
                .any(|n| n.content.contains("foreign session knowledge")),
            "foreign cell knowledge must NOT leak into the turn"
        );
        set_inheritance_env(None, None);
    }

    /// W12.26 (replaces `background_hold_release_refunds_the_reservation`):
    /// the book hold carries NO reservation — insert/release never move the
    /// budget. The refund lives in the spawned future's `record_usage`
    /// reconcile (remainder) or `Drop` (full estimate); the pin reconciles
    /// a reservation the way the future does, then proves the book path is
    /// refund-neutral.
    #[tokio::test]
    async fn release_is_refund_neutral_after_reconcile() {
        use rig_compose::budget::TokenBudget;
        let budget = Arc::new(AtomicTokenBudget::new(100_000));
        let reservation = budget
            .try_reserve_tokens(5_000)
            .await
            .unwrap()
            .expect("reserve");
        assert_eq!(
            budget.available(),
            95_000,
            "the armed hold pins the estimate"
        );
        // Reconcile the way the future does: the remainder (5000 − 4000)
        // is refunded HERE, never at collect/release.
        budget.record_usage(reservation, 3_000, 1_000).await;
        assert_eq!(
            budget.available(),
            96_000,
            "reconcile refunds the remainder (estimate − actual)"
        );
        let book = BackgroundBook::default();
        book.insert(
            "t-neutral".to_string(),
            BackgroundHolding { model_source: None },
        );
        book.release("t-neutral");
        assert_eq!(
            budget.available(),
            96_000,
            "insert/release are refund-neutral — the hold carries no reservation"
        );
        assert_eq!(budget.tokens_consumed().await, 4_000, "billed exactly once");
    }

    /// W11.16/T17 (OV-6, SC-021) resynced for W12.26: the background
    /// admission estimate is computed from the COMPOSED prompt (D9 default
    /// contract + artifact + the always-on REPORT_REQUEST trailer) plus the
    /// inherited chars — a budget one token short of that estimate must
    /// refuse the admission. The pre-W12.26 shape (raw prompt/4 + 1024)
    /// was strictly smaller, so this pin also fails if the estimate
    /// regresses to the raw prompt.
    #[tokio::test]
    async fn background_admission_estimate_includes_inherited_chars() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        set_inheritance_env(Some(dir.path()), Some("1"));
        let knowledge: SharedKnowledge =
            Arc::new(std::sync::Mutex::new(vec![note(&"a".repeat(4000))]));
        // prompt "hello" ⇒ composed (5 + REPORT_REQUEST + contract-empty
        // trailer bytes)/4 + 1024 + inherited 4000/4 = 1000. Compute the
        // estimate the admission computes so the pin never drifts on
        // trailer-text edits, then fund the budget exactly one token short.
        let req = DelegateRequest {
            agent: "Explore".into(),
            prompt: "hello".into(),
            description: None,
            independent: true,
            artifact: None,
            fork_mode: None,
            model_hint: None,
        };
        let composed = compose_sub_prompt(&req, &DelegateContract::default());
        let estimate = (composed.len() / 4 + 1024 + 4000 / 4) as u64;
        let raw_estimate = (req.prompt.len() / 4 + 1024 + 4000 / 4) as u64;
        assert!(
            estimate > raw_estimate,
            "the REPORT_REQUEST trailer must add to the estimate (W12.26)"
        );
        let tool = Arc::new(DelegateTaskTool::new(
            Arc::new(ZenWiring::new()),
            mock_router(),
            Vec::new(),
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Public)),
            knowledge,
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::new(AtomicTokenBudget::new(estimate - 1)),
            Duration::from_secs(300),
            1,
            4,
        ));
        tool.set_self_arc(Arc::clone(&tool));
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore", "prompt": "hello", "background": true
            }))
            .await
            .unwrap();
        assert_eq!(out["started"], serde_json::json!(0), "{out}");
        assert!(
            out["admissions"][0]["error"]
                .as_str()
                .unwrap()
                .contains("insufficient token budget"),
            "the composed estimate must push the admission over the budget: {out}"
        );
        set_inheritance_env(None, None);
    }

    /// Bounded terminal wait (reap-wait discipline): poll the registry
    /// snapshot until the task is terminal, or panic past `secs`.
    async fn wait_terminal(task_id: &str, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if zen_plugin::task_registry::registry()
                .snapshot(task_id)
                .is_some_and(|s| s.status.is_terminal())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "background task {task_id} must reach a terminal state within {secs}s"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// W12.26 pin ①: ONE armed reservation end-to-end. The budget is funded
    /// with EXACTLY the admission estimate (computed from the composed
    /// prompt), so a single hold consumes the whole pool — admission
    /// succeeds and the sub-turn completes ok-shaped. Under the pre-fix
    /// world the runtime re-reserve of the same estimate would soft-deny
    /// (available == 0 < estimate) and the task would complete with the
    /// "token budget exhausted" error instead of a response. This is also
    /// the behavioral composed-estimate equality pin (④): the admission
    /// estimate must equal the runtime estimate for the default contract.
    #[tokio::test]
    async fn background_single_reserve_no_double_hold() {
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118);
        // spawn_async writes under user_root, so isolate it.
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();

        let prompt_text = "single armed reservation wave";
        let req = DelegateRequest {
            agent: "Explore".into(),
            prompt: prompt_text.into(),
            description: None,
            independent: true,
            artifact: None,
            fork_mode: None,
            model_hint: None,
        };
        let composed = compose_sub_prompt(&req, &DelegateContract::default());
        let estimate = (composed.len() / 4 + 1024) as u64;
        let budget = Arc::new(AtomicTokenBudget::new(estimate));
        let tool = background_tool(Arc::new(ZenWiring::new()), Arc::clone(&budget), 1);
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore", "prompt": prompt_text, "background": true
            }))
            .await
            .unwrap();
        assert_eq!(out["started"], serde_json::json!(1), "{out}");
        let id = out["task_ids"].as_array().expect("task_ids")[0]
            .as_str()
            .expect("task id")
            .to_string();
        wait_terminal(&id, 10).await;

        let collected = zen_plugin::task_registry::registry()
            .collect_delegate(&id, 5_000)
            .await
            .expect("collect");
        assert_eq!(collected.status, "completed", "{:?}", collected.error);
        let response = collected.response.expect("ok-shaped result");
        let sub_response = response["response"].as_str().expect("sub-agent reply");
        let real = (composed.len() / 4 + sub_response.len() / 4) as u64;
        assert_eq!(
            budget.tokens_consumed().await,
            real,
            "billed exactly the real total once (single hold end-to-end)"
        );
        assert_eq!(
            budget.available(),
            estimate - real,
            "availability = capacity − real_total: the remainder was refunded \
             at reconcile, and no second hold ever existed"
        );
    }

    /// W12.26 pin ②: after the future completes, the pool is ALREADY at
    /// capacity − real_total (remainder refunded at the reconcile INSIDE
    /// the future) — collecting afterwards (running → terminal → idempotent)
    /// moves the budget by exactly zero.
    #[tokio::test]
    async fn remainder_refunded_at_reconcile_not_collect() {
        use crate::delegate_background_tools::DelegateCollectTool;
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118).
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();

        let prompt_text = "remainder refund probe";
        let req = DelegateRequest {
            agent: "Explore".into(),
            prompt: prompt_text.into(),
            description: None,
            independent: true,
            artifact: None,
            fork_mode: None,
            model_hint: None,
        };
        let composed = compose_sub_prompt(&req, &DelegateContract::default());
        let budget = Arc::new(AtomicTokenBudget::new(100_000));
        let tool = background_tool(Arc::new(ZenWiring::new()), Arc::clone(&budget), 1);
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore", "prompt": prompt_text, "background": true
            }))
            .await
            .unwrap();
        assert_eq!(out["started"], serde_json::json!(1), "{out}");
        let id = out["task_ids"].as_array().expect("task_ids")[0]
            .as_str()
            .expect("task id")
            .to_string();
        wait_terminal(&id, 10).await;

        let real = {
            let collected = zen_plugin::task_registry::registry()
                .collect_delegate(&id, 0)
                .await
                .expect("peek result");
            let response = collected.response.expect("ok-shaped result");
            let sub_response = response["response"].as_str().expect("sub-agent reply");
            (composed.len() / 4 + sub_response.len() / 4) as u64
        };
        assert_eq!(
            budget.tokens_consumed().await,
            real,
            "the future's reconcile billed the real total BEFORE any collect"
        );
        assert_eq!(
            budget.available(),
            100_000 - real,
            "the remainder (estimate − actual) was refunded at reconcile, \
             not at collect"
        );

        // Collect through the real tool: running/terminal/idempotent passes
        // must not move the budget (the book release is refund-neutral).
        let collect = DelegateCollectTool::new(tool.background_book());
        for wait_ms in [0u64, 5_000, 0] {
            let receipt = collect
                .invoke(serde_json::json!({ "task_id": id, "wait_ms": wait_ms }))
                .await
                .expect("collect invoke");
            assert_eq!(
                receipt["results"][0]["status"],
                serde_json::json!("completed")
            );
            assert_eq!(
                budget.tokens_consumed().await,
                real,
                "collect wait_ms={wait_ms} must not bill"
            );
            assert_eq!(
                budget.available(),
                100_000 - real,
                "collect wait_ms={wait_ms} must not refund"
            );
        }
    }

    /// W12.26 pin ③: a spawn rejection (registry concurrency gate) refunds
    /// the armed reservation — the reservation moved into the never-spawned
    /// future is dropped synchronously, so availability is fully restored
    /// the moment the structured rejection returns.
    #[tokio::test]
    async fn spawn_rejection_refunds_armed_reservation() {
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118).
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_BACKGROUND_MAX_CONCURRENT", "1");
        }
        zen_core::config::invalidate_config_cache();
        // Fill the registry to its [agentic.background] ceiling.
        let limits = zen_plugin::task_registry::BackgroundLimits {
            max_wall_secs: 60,
            max_concurrent: 1,
            output_cap_bytes: zen_plugin::task_registry::OUTPUT_CAP_BYTES,
        };
        let filler = zen_plugin::task_registry::registry()
            .spawn_async(
                "c0ffee00-0000-4000-8000-000000000002",
                "sess-reject",
                "Explore",
                None,
                &limits,
                async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Ok(serde_json::json!({}))
                },
            )
            .expect("filler admitted");

        let budget = Arc::new(AtomicTokenBudget::new(100_000));
        let tool = background_tool(Arc::new(ZenWiring::new()), Arc::clone(&budget), 1);
        let out = tool
            .invoke(serde_json::json!({
                "agent": "Explore", "prompt": "rejected at spawn", "background": true
            }))
            .await
            .unwrap();
        assert_eq!(out["started"], serde_json::json!(0), "{out}");
        assert!(
            out["admissions"][0]["error"]
                .as_str()
                .unwrap()
                .contains("max_concurrent"),
            "rejection names the registry gate: {out}"
        );
        assert_eq!(
            budget.tokens_consumed().await,
            0,
            "a rejected admission never bills"
        );
        assert_eq!(
            budget.available(),
            100_000,
            "the armed reservation in the never-spawned future refunded via Drop"
        );
        drop(filler);
        zen_core::config::invalidate_config_cache();
    }

    /// W12.26 pin ④ (expression-level twin of the behavioral differential in
    /// `background_single_reserve_no_double_hold`): the admission estimate
    /// expression and the runtime estimate expression are the same value
    /// for the default contract, and both strictly exceed the pre-W12.26
    /// raw-prompt shape (the latent ~REPORT_REQUEST-sized under-hold).
    #[test]
    fn composed_admission_estimate_equals_runtime_estimate_for_default_contract() {
        let req = DelegateRequest {
            agent: "Explore".into(),
            prompt: "estimate parity".into(),
            description: None,
            independent: true,
            artifact: None,
            fork_mode: None,
            model_hint: None,
        };
        let composed = compose_sub_prompt(&req, &DelegateContract::default());
        // Fixture knowledge terms (non-const so clippy's const-folding
        // lints stay quiet): both sites add `<chars>/4`, 0 for this fixture.
        let admission_knowledge_chars = 0usize;
        let runtime_inherited_chars = 0usize;
        // Admission site (invoke_background): composed bytes/4 + 1024 +
        // sub.knowledge_chars()/4.
        let admission = (composed.len() / 4 + 1024 + admission_knowledge_chars / 4) as u64;
        // Runtime site (run_single_composed): prompt.len()/4 + 1024 +
        // inherited_chars/4 — `prompt` there IS compose_sub_prompt's output.
        let runtime = (composed.len() / 4 + 1024 + runtime_inherited_chars / 4) as u64;
        assert_eq!(admission, runtime, "one estimate shape, both sites");
        let raw = (req.prompt.len() / 4 + 1024) as u64;
        assert!(
            admission > raw,
            "the always-on REPORT_REQUEST trailer must make the composed \
             estimate strictly larger than the pre-W12.26 raw shape"
        );
    }

    /// W12.26: background-path twin of
    /// `early_llm_round_failure_refunds_reservation` — the ARMED reservation
    /// rides inside the spawned future; an early LLM round failure returns
    /// BEFORE the completion reconcile, so the future drops the armed
    /// reservation and the full estimate refunds (the blocking path keeps
    /// its own pin unchanged).
    #[tokio::test]
    async fn background_armed_future_early_error_refunds_full_estimate() {
        // nextest process-per-test ⇒ the ZEN_HOME mutation is safe (T118).
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();

        // Deterministic non-retryable LLM failure: the 401 responder puts
        // "401" in the error text ⇒ ErrorCategory::ClientError ⇒
        // non-retryable ⇒ execute_round Errs on the FIRST attempt (same
        // fixture as the blocking twin pin).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
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
        // proxy would swallow the loopback request (T217 lesson).
        // SAFETY: single-threaded test process (nextest discipline).
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
        let budget = Arc::new(AtomicTokenBudget::new(100_000));
        let tool = Arc::new(DelegateTaskTool::new(
            Arc::new(ZenWiring::new()),
            DefaultRouter::from_agentic(&config),
            Vec::new(),
            None,
            Arc::new(std::sync::Mutex::new(Sensitivity::Public)),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::new(std::sync::Mutex::new(Vec::new())),
            Arc::clone(&budget) as Arc<dyn rig_compose::budget::TokenBudget>,
            Duration::from_secs(300),
            1,
            4,
        ));
        tool.set_self_arc(Arc::clone(&tool));

        let out = tool
            .invoke(serde_json::json!({
                "tasks": [{ "agent": "Explore", "prompt": "trigger the failing round" }],
                "background": true
            }))
            .await
            .unwrap();
        assert_eq!(out["started"], serde_json::json!(1), "admitted: {out}");
        let id = out["task_ids"].as_array().expect("task_ids")[0]
            .as_str()
            .expect("task id")
            .to_string();
        wait_terminal(&id, 10).await;

        let collected = zen_plugin::task_registry::registry()
            .collect_delegate(&id, 5_000)
            .await
            .expect("collect");
        assert_eq!(collected.status, "completed");
        let response = collected.response.expect("error-shaped result");
        assert!(
            response["error"]
                .as_str()
                .unwrap_or_default()
                .contains("LLM round failed"),
            "the early-error path surfaces its structured error: {response}"
        );
        assert_eq!(
            budget.tokens_consumed().await,
            0,
            "the armed reservation in the future refunded on its early return — \
             a failed background delegation never consumes budget"
        );
        assert_eq!(budget.available(), budget.capacity());
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::remove_var("FAIL401_API_KEY");
            std::env::remove_var("NO_PROXY");
            std::env::remove_var("no_proxy");
        }
    }
}
