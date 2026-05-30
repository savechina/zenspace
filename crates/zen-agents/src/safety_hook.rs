use crate::Role;
use rig_core::{
    agent::{HookAction, PromptHook, ToolCallHookAction},
    completion::CompletionModel,
    message::Message,
};
use std::{collections::HashSet, future::Future};
use zen_core::types::Sensitivity;

/// Safety hook that gates tool calls based on agent permissions and data sensitivity.
#[derive(Clone)]
pub struct ZenHook {
    agent_id: String,
    agent_role: Role,
    allowed_tools: HashSet<String>,
    sensitivity: Sensitivity,
}

impl ZenHook {
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            agent_role: Role::Worker,
            allowed_tools: HashSet::new(),
            sensitivity: Sensitivity::Public,
        }
    }

    pub fn with_agent_role(mut self, role: Role) -> Self {
        self.agent_role = role;
        self
    }

    pub fn with_allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools.into_iter().collect();
        self
    }

    pub fn with_sensitivity(mut self, sensitivity: Sensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    fn is_mutation_tool(tool_name: &str) -> bool {
        let lower = tool_name.to_lowercase();
        lower.contains("write")
            || lower.contains("create")
            || lower.contains("delete")
            || lower.contains("update")
            || lower.contains("modify")
            || lower.contains("execute")
    }

    fn is_strategy_tool(tool_name: &str) -> bool {
        let lower = tool_name.to_lowercase();
        lower.contains("plan")
            || lower.contains("design")
            || lower.contains("architect")
            || lower.contains("route")
            || lower.contains("schedule")
    }
}

fn is_cloud_tool(tool_name: &str) -> bool {
    let lower = tool_name.to_lowercase();
    lower.contains("cloud")
        || lower.contains("web")
        || lower.contains("search")
        || lower.contains("http")
        || lower.contains("network")
}

impl<M: CompletionModel> PromptHook<M> for ZenHook {
    fn on_completion_call(
        &self,
        prompt: &Message,
        _history: &[Message],
    ) -> impl Future<Output = HookAction> + Send {
        let prompt_debug = format!("{prompt:?}");
        let agent_id = self.agent_id.clone();
        let sensitivity = self.sensitivity;
        async move {
            tracing::info!(
                agent_id = %agent_id,
                sensitivity = %sensitivity,
                "on_completion_call: prompt={prompt_debug}",
            );
            HookAction::cont()
        }
    }

    fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
    ) -> impl Future<Output = ToolCallHookAction> + Send {
        let agent_id = self.agent_id.clone();
        let agent_role = self.agent_role.clone();
        let allowed = self.allowed_tools.contains(tool_name);
        let confidential = self.sensitivity == Sensitivity::Confidential;
        let cloud = is_cloud_tool(tool_name);
        let tool_name_owned = tool_name.to_owned();
        async move {
            if !allowed {
                return ToolCallHookAction::skip(format!(
                    "Tool '{tool_name_owned}' not permitted for agent '{agent_id}'"
                ));
            }

            if confidential && cloud {
                return ToolCallHookAction::skip(format!(
                    "Cloud tool '{tool_name_owned}' blocked for confidential data"
                ));
            }

            // FR-AGENT-006: Planner tier agents MUST NOT execute mutation tools
            if matches!(agent_role, Role::Planner | Role::Orchestrator)
                && Self::is_mutation_tool(&tool_name_owned)
            {
                return ToolCallHookAction::skip(format!(
                    "Planner/Orchestrator agent '{agent_id}' cannot use mutation tool '{tool_name_owned}'"
                ));
            }

            // FR-AGENT-007: Worker tier agents MUST NOT use strategy tools
            if matches!(agent_role, Role::Worker) && Self::is_strategy_tool(&tool_name_owned) {
                return ToolCallHookAction::skip(format!(
                    "Worker agent '{agent_id}' cannot use strategy tool '{tool_name_owned}'"
                ));
            }

            // TODO: implement prompt injection detection (flag suspicious tool args)

            ToolCallHookAction::cont()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_planner_cannot_use_mutation_tools() {
        let hook = ZenHook::new("Metis")
            .with_agent_role(Role::Planner)
            .with_allowed_tools(vec!["read_file".to_string(), "search".to_string()]);

        assert!(ZenHook::is_mutation_tool("write_file"));
        assert!(ZenHook::is_mutation_tool("delete_file"));
        assert!(ZenHook::is_mutation_tool("execute_command"));
        assert!(!ZenHook::is_mutation_tool("read_file"));
    }

    #[test]
    fn test_worker_cannot_use_strategy_tools() {
        let hook = ZenHook::new("Junior")
            .with_agent_role(Role::Worker)
            .with_allowed_tools(vec!["read_file".to_string(), "write_file".to_string()]);

        assert!(ZenHook::is_strategy_tool("plan_architecture"));
        assert!(ZenHook::is_strategy_tool("design_system"));
        assert!(ZenHook::is_strategy_tool("route_task"));
        assert!(!ZenHook::is_strategy_tool("read_file"));
    }

    #[test]
    fn test_cloud_tool_detection() {
        assert!(is_cloud_tool("web_search"));
        assert!(is_cloud_tool("http_request"));
        assert!(is_cloud_tool("network_call"));
        assert!(!is_cloud_tool("read_file"));
    }

    #[test]
    fn test_hook_builder_defaults() {
        let hook = ZenHook::new("test-agent");
        assert_eq!(hook.agent_id, "test-agent");
        assert_eq!(hook.agent_role, Role::Worker);
        assert_eq!(hook.sensitivity, Sensitivity::Public);
        assert!(hook.allowed_tools.is_empty());
    }
}
