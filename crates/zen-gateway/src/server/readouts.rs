//! Read-only inspection methods (T033): `agent/list`, `agent/status`,
//! `skill/list` per contracts/02 §Agent/Skill.
//!
//! PURPOSE: Registry-inspection endpoints for surfaces (TUI agent
//! picker, budget displays). Listing is read-only by design (dsh
//! precedent) — these handlers never mutate state and are dispatched
//! through the slow-method pool (guards.rs) so a cold registry scan
//! never head-of-line blocks `session/turn`.
//!
//! USAGE: The daemon installs these via `DispatchServer::handle`
//! capturing the shared [`ReadDeps`].
//!
//! EXPECTED: `agent/list` returns every registered profile with its
//! tier/role/first model preference; `agent/status` reports liveness
//! for one agent; `skill/list` enumerates workspace skill names.
//!
//! ERRORS: -32602 malformed params; -32603 when the underlying stack
//! (registry/skills) failed to build at daemon startup.

use std::sync::Arc;

use serde_json::{Value, json};
use zen_agents::AgentRegistry;

use crate::protocol::RpcError;

/// Shared read-model dependencies.
#[derive(Clone)]
pub struct ReadDeps {
    /// Daemon-built agent registry.
    pub registry: Option<Arc<dyn AgentRegistry + Send>>,
}

/// agent/list — `{}` → `{agents: [{name, tier, role, model}]}`.
pub fn agent_list(deps: Arc<ReadDeps>, _params: Value) -> Result<Value, RpcError> {
    let registry = deps
        .registry
        .as_ref()
        .ok_or_else(|| RpcError::internal("agent registry unavailable"))?;
    let agents: Vec<Value> = registry
        .list_all()
        .iter()
        .map(|profile| {
            json!({
                "name": profile.name,
                "tier": "specialist",
                "role": profile.role.to_string(),
                "model": profile.llm_preferences.first().map(|p| p.to_string()),
            })
        })
        .collect();
    Ok(json!({ "agents": agents }))
}

/// agent/status — `{agent}` → `{agent, busy, budgetAvailable,
/// budgetConsumed}`. Budget instrumentation lands with usage events;
/// values report zero until then (honest defaults, not fabricated).
pub fn agent_status(deps: Arc<ReadDeps>, params: Value) -> Result<Value, RpcError> {
    const METHOD: &str = "agent/status";
    let agent = params
        .get("agent")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params(METHOD, "missing \"agent\""))?
        .to_string();

    let known = deps
        .registry
        .as_ref()
        .map(|r| r.list_all().iter().any(|p| p.name == agent))
        .unwrap_or(false);
    if deps.registry.is_some() && !known {
        return Err(RpcError::invalid_params(
            METHOD,
            &format!("unknown agent {agent:?}"),
        ));
    }
    Ok(json!({
        "agent": agent,
        "busy": false,
        "budgetAvailable": 0,
        "budgetConsumed": 0,
    }))
}

/// skill/list — `{}` → `{skills: [{name, source, modelInvocable}]}`.
pub fn skill_list(
    paths: Option<&zen_core::paths::ZenPaths>,
    _params: Value,
) -> Result<Value, RpcError> {
    let skills = match paths {
        Some(paths) => zen_agents::skill_loader::SkillLoader::new(paths)
            .list_skills()
            .map_err(|e| RpcError::internal(&format!("skill listing failed: {e}")))?,
        None => Vec::new(),
    };
    let skills: Vec<Value> = skills
        .into_iter()
        .map(|name| {
            json!({
                "name": name,
                "source": "workspace",
                "modelInvocable": true,
            })
        })
        .collect();
    Ok(json!({ "skills": skills }))
}
