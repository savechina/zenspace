//! 18-section tiered prompt assembly with cache boundary.
//!
//! Architecture follows Claude Code's decompiled system prompt:
//! - STATIC ZONE (sections 1-12): Globally cacheable, computed once
//! - DYNAMIC ZONE (sections 13-18): Per-session, recomputed every turn
//! - Cache Boundary: Typed sentinel `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__`
//! - Priority Override Chain: 5-level replacement chain
//! - Section Memoization: cached vs cache-breaking sections
//! - Blast Radius Taxonomy: LOW/MEDIUM/HIGH action classification
//! - System Reminders: User message injection (preserve cache)

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use zen_core::AgentDefinition;
use zen_core::paths::ZenPaths;
use zen_core::types::{Sensitivity, SessionContext};

// ---------------------------------------------------------------------------
// Cache Boundary Sentinel (Claude Code naming)
// ---------------------------------------------------------------------------

/// Sentinel marker between static and dynamic zones.
///
/// Claude Code uses `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` as a typed sentinel
/// to split the prompt into two cache zones:
/// - Everything above: byte-identical across users/sessions (cacheScope: 'global')
/// - Everything below: recomputed per-session or per-turn
pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__";

// ---------------------------------------------------------------------------
// CacheScope — static vs dynamic prompt sections
// ---------------------------------------------------------------------------

/// Cache scope for prompt sections.
///
/// Determines whether a section participates in the LLM provider's prompt
/// caching mechanism. Static sections are placed before the cache boundary
/// marker and cached across turns; dynamic sections are recomputed every turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CacheScope {
    /// Computed once, cached across the session. Maps to pre-dynamic-boundary.
    ///
    /// Covers: Sections 1-12 (Identity, Behavior, Tools, Output, etc.)
    Static,
    /// Recomputed every turn. Maps to post-dynamic-boundary.
    ///
    /// Covers: Sections 13-18 (Context, Memory, MCP, CLAUDE.md)
    Dynamic,
}

// ---------------------------------------------------------------------------
// PromptAssembly — 18-section tiered prompt assembly
// ---------------------------------------------------------------------------

/// 18-section tiered prompt assembly following Claude Code architecture.
///
/// Architecture (Claude Code decompiled source):
/// ```text
/// STATIC ZONE (sections 1-12, globally cacheable)
///   ├── 1. System Prefix      — "You are Zen, a Rust CLI agent"
///   ├── 2. Intro Section      — Role definition + SOUL.md
///   ├── 3. Cyber Risk         — Dual-use security check (optional)
///   ├── 4. URL Safety         — Anti-hallucination rules
///   ├── 5. System Section     — Permission mode, hooks
///   ├── 6. Doing Tasks        — Anti-YAGNI: explicit DON'Ts
///   ├── 7. Actions Section    — LOW/MEDIUM/HIGH blast radius taxonomy
///   ├── 8. Using Tools        — Tool preference hierarchy
///   ├── 9. Agent Tool         — Subagent delegation rules
///   ├── 10. Output Efficiency — Response format constraints
///   ├── 11. Token Budget      — Budget guard (GATED)
///   └── 12. Proactive         — Proactive suggestions (GATED)
///
/// ═══ __SYSTEM_PROMPT_DYNAMIC_BOUNDARY__ ═══
///
/// DYNAMIC ZONE (sections 13-18, per-session)
///   ├── 13. Environment Info  — CWD, git branch, OS, model, date
///   ├── 14. Scratchpad        — Temp directory path
///   ├── 15. Function Result   — Context compaction
///   ├── 16. MCP Instructions  — MCP server instructions (UNCACHED)
///   ├── 17. Memory Section    — Conversation history + knowledge
///   └── 18. CLAUDE.md Content — AGENTS.md + custom instructions
/// ```
///
/// Key Features:
/// - **Cache Boundary**: Typed sentinel splits static/dynamic zones
/// - **Priority Override Chain**: 5-level replacement chain (override → coordinator → agent → custom → default)
/// - **Section Memoization**: cached vs cache-breaking sections
/// - **Blast Radius Taxonomy**: LOW/MEDIUM/HIGH action classification
/// - **System Reminders**: Inject into user messages (preserve cache)
#[derive(Debug, Clone)]
pub struct PromptAssembly {
    // -- Priority Override Chain (5 levels) --
    /// Priority 1: Full override (replaces everything)
    pub override_prompt: Option<String>,

    /// Priority 2: Coordinator mode (swarm/orchestration)
    pub coordinator_mode: bool,

    /// Priority 3: Agent definition (custom agent prompt)
    pub agent_definition: Option<AgentDefinition>,

    /// Priority 4: Custom prompt (--system-prompt flag)
    pub custom_prompt: Option<String>,

    // -- Static Zone (sections 1-12, globally cacheable) --
    /// Section 1: Zen System Prefix
    pub system_prefix: Option<String>,

    /// Section 2: Intro (Role definition + SOUL.md)
    pub intro: Option<String>,

    /// Section 3: Cyber Risk Instruction (optional)
    pub cyber_risk: Option<String>,

    /// Section 4: URL Safety (Anti-hallucination)
    pub url_safety: Option<String>,

    /// Section 5: System Section (Permission mode, hooks)
    pub system_section: Option<String>,

    /// Section 6: Doing Tasks (Anti-YAGNI + behavior_constraints)
    pub doing_tasks: Option<String>,

    /// Section 7: Actions Section (Blast Radius Taxonomy)
    pub actions: Option<String>,

    /// Section 8: Using Tools (Tool definitions + permissions)
    pub using_tools: Option<String>,

    /// Section 9: Agent Tool Section (Subagent delegation rules)
    pub agent_tool: Option<String>,

    /// Section 10: Output Efficiency (Response format)
    pub output_efficiency: Option<String>,

    /// Section 11: Token Budget (GATED)
    pub token_budget: Option<String>,

    /// Section 12: Proactive/KAIROS (GATED)
    pub proactive: Option<String>,

    // -- Dynamic Zone (sections 13-18, per-session) --
    /// Section 13: Environment Info (CWD, git, OS, model, date)
    pub env_info: Option<String>,

    /// Section 14: Scratchpad (Temp directory)
    pub scratchpad: Option<String>,

    /// Section 15: Function Result Clearing
    pub function_result_clearing: Option<String>,

    /// Section 16: MCP Instructions (Always UNCACHED)
    pub mcp_instructions: Option<String>,

    /// Section 17: Memory Section (Conversation history + knowledge)
    pub memory: Option<String>,

    /// Section 18: CLAUDE.md Content (AGENTS.md + custom_instructions)
    pub claude_md: Option<String>,

    // -- Legacy fields (backward compatibility) --
    /// Soul content (mapped to intro section)
    pub soul_content: String,

    /// Agents content (mapped to claude_md section)
    pub agents_content: String,

    /// Memory file content (mapped to memory section)
    pub memory_content: String,

    /// Retrieved knowledge (mapped to memory section)
    pub retrieved_knowledge: Vec<String>,

    /// Conversation history (mapped to memory section)
    pub conversation_history: Vec<(String, String)>,

    // -- Section Cache State --
    /// Cached sections (computed once per session)
    cached_sections: HashMap<String, String>,

    /// Cache-breaking sections (recomputed every turn)
    /// Format: (section_name, (content, reason))
    cache_breaking_sections: HashMap<String, (String, String)>,

    /// Current sensitivity level for blast radius taxonomy
    pub sensitivity: Sensitivity,
}

impl Default for PromptAssembly {
    fn default() -> Self {
        Self {
            override_prompt: None,
            coordinator_mode: false,
            agent_definition: None,
            custom_prompt: None,
            system_prefix: None,
            intro: None,
            cyber_risk: None,
            url_safety: None,
            system_section: None,
            doing_tasks: None,
            actions: None,
            using_tools: None,
            agent_tool: None,
            output_efficiency: None,
            token_budget: None,
            proactive: None,
            env_info: None,
            scratchpad: None,
            function_result_clearing: None,
            mcp_instructions: None,
            memory: None,
            claude_md: None,
            soul_content: String::new(),
            agents_content: String::new(),
            memory_content: String::new(),
            retrieved_knowledge: Vec::new(),
            conversation_history: Vec::new(),
            cached_sections: HashMap::new(),
            cache_breaking_sections: HashMap::new(),
            sensitivity: Sensitivity::Public,
        }
    }
}

impl PromptAssembly {
    /// Create empty prompt assembly.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create builder for fluent construction.
    pub fn builder() -> PromptAssemblyBuilder {
        PromptAssemblyBuilder::default()
    }

    // -----------------------------------------------------------------------
    // Priority Override Chain (5-level replacement chain)
    // -----------------------------------------------------------------------

    /// Assemble system prompt following 5-level priority chain.
    ///
    /// Priority order:
    /// 1. override_prompt → replaces everything
    /// 2. coordinator_mode → swarm prompt
    /// 3. agent_definition → custom agent prompt (replacement, not append)
    /// 4. custom_prompt → CLI flag prompt (replacement, not append)
    /// 5. default 18-section assembly
    ///
    /// Note: Claude Code uses **replacement chain** (only one base prompt wins),
    /// not merge/append chain.
    pub fn assemble(&self) -> String {
        // Priority 1: Override replaces everything
        if let Some(ref override_prompt) = self.override_prompt {
            return override_prompt.clone();
        }

        // Priority 2: Coordinator mode (swarm/orchestration)
        if self.coordinator_mode {
            return self.build_coordinator_prompt();
        }

        // Priority 3: Agent definition (replacement chain)
        if let Some(ref def) = self.agent_definition {
            return self.build_agent_prompt(def);
        }

        // Priority 4: Custom prompt (replacement chain)
        if let Some(ref custom) = self.custom_prompt {
            return self.build_custom_prompt(custom);
        }

        // Priority 5: Default 18-section assembly
        self.build_default_18_sections()
    }

    /// Build coordinator (swarm) mode prompt.
    fn build_coordinator_prompt(&self) -> String {
        let mut static_parts = Vec::new();
        let mut dynamic_parts = Vec::new();

        // Static sections (coordinator-specific)
        if let Some(ref prefix) = self.system_prefix
            && !prefix.is_empty()
        {
            static_parts.push(prefix.clone());
        }
        static_parts.push(Self::build_coordinator_intro());
        static_parts.push(Self::build_blast_radius_section(self.sensitivity));

        // Dynamic sections
        // Section 13: Environment Info
        if let Some(ref env) = self.env_info
            && !env.is_empty()
        {
            dynamic_parts.push(env.clone());
        }

        // Section 17: Memory (use build_memory_section())
        let memory_parts = self.build_memory_section();
        if !memory_parts.is_empty() {
            dynamic_parts.push(memory_parts);
        }

        let mut result = static_parts.join("\n\n");
        result.push_str("\n\n");
        result.push_str(SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
        result.push_str("\n\n");
        result.push_str(&dynamic_parts.join("\n\n"));

        result
    }

    /// Build agent definition prompt (replacement chain).
    fn build_agent_prompt(&self, def: &AgentDefinition) -> String {
        let mut static_parts = Vec::new();
        let mut dynamic_parts = Vec::new();

        // Section 1: System prefix (from definition)
        if !def.prompt_template.is_empty() {
            static_parts.push(def.prompt_template.clone());
        }

        // Section 2: Intro (SOUL.md)
        if !self.soul_content.is_empty() {
            static_parts.push(format!(
                "## Personality & Principles\n{}",
                self.soul_content
            ));
        }

        // Section 6: Doing Tasks (behavior_constraints)
        if !def.behavior_constraints.is_empty() {
            static_parts.push(format!(
                "## Behavior Constraints\n{}\n{}",
                def.behavior_constraints.join("\n"),
                Self::build_blast_radius_section(self.sensitivity)
            ));
        }

        // Section 10: Output Efficiency
        if let Some(ref format) = def.output_format {
            static_parts.push(format!("## Output Format\n{}", format));
        }

        // Section 17: Memory (conversation + knowledge)
        let memory_parts = self.build_memory_section();
        if !memory_parts.is_empty() {
            dynamic_parts.push(memory_parts);
        }

        // Section 18: CLAUDE.md (AGENTS.md + custom_instructions)
        if !self.agents_content.is_empty() || !def.custom_instructions.is_empty() {
            let claude_parts = self.build_claude_md_section(def);
            dynamic_parts.push(claude_parts);
        }

        let mut result = static_parts.join("\n\n");
        if !dynamic_parts.is_empty() {
            result.push_str("\n\n");
            result.push_str(SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
            result.push_str("\n\n");
            result.push_str(&dynamic_parts.join("\n\n"));
        }

        result
    }

    /// Build custom prompt (replacement chain).
    fn build_custom_prompt(&self, custom: &str) -> String {
        let mut static_parts = Vec::new();
        let mut dynamic_parts = Vec::new();

        // Default static sections
        static_parts.push(Self::build_default_intro());
        static_parts.push(Self::build_blast_radius_section(self.sensitivity));

        // Append custom prompt (Claude Code pattern: append at end)
        static_parts.push(custom.to_string());

        // Dynamic sections
        // Section 13: Environment Info
        if let Some(ref env) = self.env_info
            && !env.is_empty()
        {
            dynamic_parts.push(env.clone());
        }

        // Section 17: Memory (use build_memory_section())
        let memory_parts = self.build_memory_section();
        if !memory_parts.is_empty() {
            dynamic_parts.push(memory_parts);
        }

        let mut result = static_parts.join("\n\n");
        if !dynamic_parts.is_empty() {
            result.push_str("\n\n");
            result.push_str(SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
            result.push_str("\n\n");
            result.push_str(&dynamic_parts.join("\n\n"));
        }

        result
    }

    /// Build default 18-section prompt with cache boundary.
    fn build_default_18_sections(&self) -> String {
        let mut static_parts = Vec::new();
        let mut dynamic_parts = Vec::new();

        // Static Zone (sections 1-12)
        for content in [
            &self.system_prefix,
            &self.intro,
            &self.cyber_risk,
            &self.url_safety,
            &self.system_section,
            &self.doing_tasks,
            &self.actions,
            &self.using_tools,
            &self.agent_tool,
            &self.output_efficiency,
            &self.token_budget,
            &self.proactive,
        ]
        .into_iter()
        .flatten()
        {
            if !content.is_empty() {
                static_parts.push(content.clone());
            }
        }

        // If static sections empty, use default intro + blast radius
        if static_parts.is_empty() {
            static_parts.push(Self::build_default_intro());
            static_parts.push(Self::build_blast_radius_section(self.sensitivity));
        }

        // Dynamic Zone (sections 13-18)
        // Section 13: Environment Info
        if let Some(ref env) = self.env_info
            && !env.is_empty()
        {
            dynamic_parts.push(env.clone());
        }

        // Section 14: Scratchpad
        if let Some(ref scratch) = self.scratchpad
            && !scratch.is_empty()
        {
            dynamic_parts.push(scratch.clone());
        }

        // Section 15: Function Result Clearing
        if let Some(ref func_result) = self.function_result_clearing
            && !func_result.is_empty()
        {
            dynamic_parts.push(func_result.clone());
        }

        // Section 16: MCP Instructions
        if let Some(ref mcp) = self.mcp_instructions
            && !mcp.is_empty()
        {
            dynamic_parts.push(mcp.clone());
        }

        // Section 17: Memory (use build_memory_section())
        let memory_parts = self.build_memory_section();
        if !memory_parts.is_empty() {
            dynamic_parts.push(memory_parts);
        }

        // Section 18: CLAUDE.md
        if let Some(ref claude_md) = self.claude_md
            && !claude_md.is_empty()
        {
            dynamic_parts.push(claude_md.clone());
        }

        // Build with cache boundary
        let mut result = static_parts.join("\n\n");
        if !dynamic_parts.is_empty() {
            result.push_str("\n\n");
            result.push_str(SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
            result.push_str("\n\n");
            result.push_str(&dynamic_parts.join("\n\n"));
        }

        result
    }

    // -----------------------------------------------------------------------
    // Section Builders
    // -----------------------------------------------------------------------

    /// Build default intro section.
    fn build_default_intro() -> String {
        "You are Zen, a helpful Rust CLI agent. Answer concisely and directly.".to_string()
    }

    /// Build coordinator intro section.
    fn build_coordinator_intro() -> String {
        r#"## Coordinator Behavior

You are a Zen coordinator agent. Your role is to:

1. Receive complex, multi-step tasks from the user.
2. Decompose tasks into delegable sub-tasks.
3. Route sub-tasks to the appropriate specialist agents.
4. Collect results, validate quality, and synthesize a final response.

Guidelines:
- Prefer parallel delegation when sub-tasks are independent.
- Validate outputs against the original acceptance criteria.
- Escalate to the user when a sub-task fails repeatedly or ambiguity arises.
- Maintain audit trails for all delegated work.

## Quality Gatekeeping

After each agent returns a result, verify:
- Does the output satisfy the sub-task requirements?
- Are there any safety or sensitivity violations?
- Should the result be escalated for human review?

Reject and re-delegate if quality gates fail."#
            .to_string()
    }

    /// Build blast radius taxonomy section (Section 7).
    pub fn build_blast_radius_section(sensitivity: Sensitivity) -> String {
        format!(
            r#"## Blast Radius Taxonomy

When executing actions, classify the risk level:

- **LOW**: Read files, search knowledge base, format output
  Action: Execute without asking

- **MEDIUM**: Create notes, modify wiki pages, run searches
  Action: Execute with caution

- **HIGH**: Delete knowledge base data, run consolidation, execute SQL
  Action: Require explicit user confirmation

Current sensitivity level: {}"#,
            sensitivity
        )
    }

    /// Build memory section (Section 17).
    fn build_memory_section(&self) -> String {
        let mut parts = Vec::new();

        // Retrieved knowledge
        if !self.retrieved_knowledge.is_empty() {
            parts.push("## Retrieved Knowledge".to_string());
            for (i, note) in self.retrieved_knowledge.iter().enumerate() {
                parts.push(format!("[{}] {}", i + 1, note));
            }
        }

        // Conversation history
        if !self.conversation_history.is_empty() {
            parts.push("## Conversation History".to_string());
            for (role, content) in &self.conversation_history {
                parts.push(format!("{}: {}", role, content));
            }
        }

        if parts.is_empty() {
            String::new()
        } else {
            parts.join("\n")
        }
    }

    /// Build CLAUDE.md section (Section 18).
    fn build_claude_md_section(&self, def: &AgentDefinition) -> String {
        let mut parts = Vec::new();

        if !self.agents_content.is_empty() {
            parts.push("## Operating Instructions".to_string());
            parts.push(self.agents_content.clone());
        }

        if !def.custom_instructions.is_empty() {
            parts.push("## Custom Instructions".to_string());
            parts.push(def.custom_instructions.join("\n"));
        }

        if parts.is_empty() {
            String::new()
        } else {
            parts.join("\n\n")
        }
    }

    // -----------------------------------------------------------------------
    // Section Memoization (cached vs cache-breaking)
    // -----------------------------------------------------------------------

    /// Cached section: computed once per session.
    ///
    /// Stores result in cached_sections map. Cleared on /clear or /compact.
    pub fn cached_section(&mut self, name: &str, compute: impl FnOnce() -> Option<String>) {
        if let Some(content) = compute() {
            self.cached_sections.insert(name.to_string(), content);
        }
    }

    /// Cache-breaking section: recomputed every turn (must declare reason).
    ///
    /// Forces developer to justify cache-breaking behavior.
    pub fn cache_breaking_section(
        &mut self,
        name: &str,
        compute: impl FnOnce() -> Option<String>,
        reason: &str,
    ) {
        if let Some(content) = compute() {
            self.cache_breaking_sections
                .insert(name.to_string(), (content, reason.to_string()));
        }
    }

    /// Retrieve cached section (from memoization cache).
    pub fn get_cached(&self, name: &str) -> Option<&String> {
        self.cached_sections.get(name)
    }

    /// Retrieve cache-breaking section.
    pub fn get_cache_breaking(&self, name: &str) -> Option<&(String, String)> {
        self.cache_breaking_sections.get(name)
    }

    /// Clear cache (on /clear or /compact).
    pub fn clear_cache(&mut self) {
        self.cached_sections.clear();
        // Note: cache_breaking_sections recomputed next turn
    }

    // -----------------------------------------------------------------------
    // System Reminders (user message injection)
    // -----------------------------------------------------------------------

    /// Build system reminders for user message injection.
    ///
    /// Claude Code pattern: Inject dynamic info into user messages
    /// (not system prompt) to preserve cache.
    pub fn build_user_message_reminders(&self) -> String {
        let mut reminders = Vec::new();

        // Git status (dynamic)
        if let Some(ref env) = self.env_info
            && !env.is_empty()
        {
            reminders.push(format!("<antml:ref type='git_status'>{}</antml:ref>", env));
        }

        // Cache breaker (dynamic)
        if let Some(ref scratchpad) = self.scratchpad
            && !scratchpad.is_empty()
        {
            reminders.push(format!(
                "<antml:ref type='scratchpad'>{}</antml:ref>",
                scratchpad
            ));
        }

        // MCP instructions (dynamic, cache-breaking)
        if let Some(ref mcp) = self.mcp_instructions
            && !mcp.is_empty()
        {
            reminders.push(format!("<antml:ref type='mcp'>{}</antml:ref>", mcp));
        }

        // Memory (dynamic) - use build_memory_section()
        let memory_parts = self.build_memory_section();
        if !memory_parts.is_empty() {
            reminders.push(format!(
                "<antml:ref type='memory'>{}</antml:ref>",
                memory_parts
            ));
        }

        if reminders.is_empty() {
            String::new()
        } else {
            reminders.join("\n")
        }
    }

    // -----------------------------------------------------------------------
    // File loaders (legacy API compatibility)
    // -----------------------------------------------------------------------

    /// Load SOUL.md content from the workspace.
    pub fn load_soul_content(paths: &ZenPaths) -> String {
        let path = paths.global_root().join("SOUL.md");
        read_file_or_empty(&path)
    }

    /// Load AGENTS.md content from the workspace.
    pub fn load_agents_content(paths: &ZenPaths) -> String {
        let path = paths.global_root().join("AGENTS.md");
        read_file_or_empty(&path)
    }

    /// Load MEMORY.md content from the workspace.
    pub fn load_memory_content(paths: &ZenPaths) -> String {
        let path = paths.global_root().join("MEMORY.md");
        read_file_or_empty(&path)
    }

    /// Build environment info section (Section 13).
    pub fn build_env_info(session: &SessionContext) -> String {
        format!(
            r#"## Environment Info
- CWD: {}
- Git branch: {}
- Model: {}
- Date: {}"#,
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            session.session_id,
            session.agent_name,
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        )
    }
}

fn read_file_or_empty(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// PromptAssemblyBuilder — fluent construction
// ---------------------------------------------------------------------------

/// Builder for [`PromptAssembly`] with ergonomic defaults.
#[derive(Debug, Clone, Default)]
pub struct PromptAssemblyBuilder {
    assembly: PromptAssembly,
}

impl PromptAssemblyBuilder {
    /// Priority 1: Set override prompt (replaces everything).
    pub fn override_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.assembly.override_prompt = Some(prompt.into());
        self
    }

    /// Priority 2: Enable coordinator mode.
    pub fn coordinator_mode(mut self, enabled: bool) -> Self {
        self.assembly.coordinator_mode = enabled;
        self
    }

    /// Priority 3: Set agent definition.
    pub fn agent_definition(mut self, def: AgentDefinition) -> Self {
        self.assembly.agent_definition = Some(def);
        self
    }

    /// Priority 4: Set custom prompt.
    pub fn custom_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.assembly.custom_prompt = Some(prompt.into());
        self
    }

    /// Section 1: Set system prefix.
    pub fn system_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.assembly.system_prefix = Some(prefix.into());
        self
    }

    /// Section 2: Set intro (SOUL.md).
    pub fn intro(mut self, soul: impl Into<String>) -> Self {
        let soul_str = soul.into();
        self.assembly.intro = Some(soul_str.clone());
        self.assembly.soul_content = soul_str;
        self
    }

    /// Section 6: Set doing tasks (behavior_constraints).
    pub fn doing_tasks(mut self, constraints: Vec<String>) -> Self {
        self.assembly.doing_tasks = Some(constraints.join("\n"));
        self
    }

    /// Section 7: Set blast radius (from sensitivity).
    pub fn blast_radius(mut self, sensitivity: Sensitivity) -> Self {
        self.assembly.sensitivity = sensitivity;
        self.assembly.actions = Some(PromptAssembly::build_blast_radius_section(sensitivity));
        self
    }

    /// Section 8: Set using tools.
    pub fn using_tools(mut self, tools: Vec<String>) -> Self {
        self.assembly.using_tools = Some(format!("## Available Tools\n{}", tools.join("\n")));
        self
    }

    /// Section 10: Set output efficiency.
    pub fn output_efficiency(mut self, format: impl Into<String>) -> Self {
        self.assembly.output_efficiency = Some(format.into());
        self
    }

    /// Section 13: Set env info (dynamic).
    pub fn env_info(mut self, env: impl Into<String>) -> Self {
        self.assembly.env_info = Some(env.into());
        self
    }

    /// Section 14: Set scratchpad (dynamic).
    pub fn scratchpad(mut self, scratch: impl Into<String>) -> Self {
        self.assembly.scratchpad = Some(scratch.into());
        self
    }

    /// Section 16: Set MCP instructions (dynamic, cache-breaking).
    pub fn mcp_instructions(mut self, mcp: impl Into<String>) -> Self {
        self.assembly.mcp_instructions = Some(mcp.into());
        self
    }

    /// Section 17: Set memory section (retrieved knowledge + conversation history).
    ///
    /// Merges knowledge retrieval and conversation history into Section 17.
    /// Both components are stored in independent fields and assembled via build_memory_section().
    pub fn memory_section(
        mut self,
        knowledge: Vec<String>,
        history: Vec<(String, String)>,
    ) -> Self {
        self.assembly.retrieved_knowledge = knowledge;
        self.assembly.conversation_history = history;
        // Note: memory field will be set by build_memory_section() during assemble()
        self
    }

    /// Section 18: Set CLAUDE.md (AGENTS.md).
    pub fn claude_md(mut self, agents: impl Into<String>) -> Self {
        let agents_str = agents.into();
        self.assembly.claude_md = Some(agents_str.clone());
        self.assembly.agents_content = agents_str;
        self
    }

    /// Set sensitivity level.
    pub fn sensitivity(mut self, level: Sensitivity) -> Self {
        self.assembly.sensitivity = level;
        self
    }

    /// Build final PromptAssembly.
    pub fn build(self) -> PromptAssembly {
        self.assembly
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_empty_inputs() {
        let assembly = PromptAssembly::new();
        let result = assembly.assemble();
        assert!(result.contains("You are Zen"));
        assert!(result.contains("Blast Radius"));
    }

    #[test]
    fn assemble_with_agent_definition() {
        let def = AgentDefinition::default_agent();
        let assembly = PromptAssembly::builder()
            .agent_definition(def)
            .sensitivity(Sensitivity::Public)
            .build();
        let result = assembly.assemble();
        assert!(result.contains("You are Zen"));
        assert!(result.contains("Blast Radius"));
        assert!(result.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"));
    }

    #[test]
    fn assemble_full_18_sections() {
        let assembly = PromptAssembly::builder()
            .system_prefix("System Prefix")
            .intro("SOUL.md content")
            .doing_tasks(vec!["Constraint 1".to_string()])
            .blast_radius(Sensitivity::Confidential)
            .using_tools(vec!["read".to_string()])
            .output_efficiency("Return JSON")
            .env_info("CWD: /test")
            .memory_section(
                vec!["Knowledge 1".to_string()],
                vec![("user".to_string(), "Hello".to_string())],
            )
            .claude_md("AGENTS.md content")
            .build();

        let result = assembly.assemble();

        // Verify static sections
        assert!(result.contains("System Prefix"));
        assert!(result.contains("SOUL.md"));
        assert!(result.contains("Constraint 1"));
        assert!(result.contains("Blast Radius"));
        assert!(result.contains("Available Tools"));
        assert!(result.contains("Return JSON"));

        // Verify cache boundary
        assert!(result.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"));

        // Verify dynamic sections
        assert!(result.contains("CWD:"));
        assert!(result.contains("Conversation History"));
        assert!(result.contains("AGENTS.md"));

        // Verify static precedes dynamic
        let boundary_pos = result.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
        assert!(result[..boundary_pos].contains("System Prefix"));
        assert!(result[boundary_pos..].contains("CWD:"));
    }

    #[test]
    fn priority_override_replaces_everything() {
        let assembly = PromptAssembly::builder()
            .override_prompt("FULL OVERRIDE")
            .intro("This should not appear")
            .build();

        let result = assembly.assemble();
        assert_eq!(result, "FULL OVERRIDE");
        assert!(!result.contains("This should not appear"));
    }

    #[test]
    fn priority_coordinator_mode() {
        let assembly = PromptAssembly::builder()
            .coordinator_mode(true)
            .blast_radius(Sensitivity::Public)
            .build();

        let result = assembly.assemble();
        assert!(result.contains("Coordinator Behavior"));
        assert!(result.contains("Quality Gatekeeping"));
        assert!(result.contains("Blast Radius"));
    }

    #[test]
    fn priority_agent_definition_replacement() {
        let def = AgentDefinition {
            name: "test".to_string(),
            prompt_template: "CUSTOM AGENT TEMPLATE".to_string(),
            tool_permissions: vec![],
            context_injection: vec!["knowledge".to_string()],
            category_routing: None,
            behavior_constraints: vec!["Rule 1".to_string()],
            output_format: Some("JSON".to_string()),
            custom_instructions: vec!["Instruction 1".to_string()],
        };

        let assembly = PromptAssembly::builder()
            .agent_definition(def)
            .intro("SOUL")
            .blast_radius(Sensitivity::Public)
            .build();

        let result = assembly.assemble();
        assert!(result.contains("CUSTOM AGENT TEMPLATE"));
        assert!(result.contains("SOUL"));
        assert!(result.contains("Rule 1"));
        assert!(result.contains("JSON"));
        assert!(result.contains("Instruction 1"));
    }

    #[test]
    fn cache_boundary_presence() {
        let assembly = PromptAssembly::builder()
            .intro("Static content")
            .env_info("Dynamic content")
            .blast_radius(Sensitivity::Public)
            .build();

        let result = assembly.assemble();
        assert!(result.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"));

        // Static before boundary
        let boundary_pos = result.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
        assert!(result[..boundary_pos].contains("Static content"));

        // Dynamic after boundary
        assert!(result[boundary_pos..].contains("Dynamic content"));
    }

    #[test]
    fn section_memoization_cached() {
        let mut assembly = PromptAssembly::new();
        assembly.cached_section("env_info", || Some("Cached env".to_string()));

        let cached = assembly.get_cached("env_info");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap(), "Cached env");
    }

    #[test]
    fn section_memoization_cache_breaking() {
        let mut assembly = PromptAssembly::new();
        assembly.cache_breaking_section(
            "mcp_instructions",
            || Some("Dynamic MCP".to_string()),
            "MCP servers connect/disconnect between turns",
        );

        let cache_breaking = assembly.get_cache_breaking("mcp_instructions");
        assert!(cache_breaking.is_some());
        assert_eq!(cache_breaking.unwrap().0, "Dynamic MCP");
        assert_eq!(
            cache_breaking.unwrap().1,
            "MCP servers connect/disconnect between turns"
        );
    }

    #[test]
    fn clear_cache() {
        let mut assembly = PromptAssembly::new();
        assembly.cached_section("test", || Some("content".to_string()));

        assembly.clear_cache();

        assert!(assembly.get_cached("test").is_none());
    }

    #[test]
    fn user_message_reminders() {
        let assembly = PromptAssembly::builder()
            .env_info("git status")
            .scratchpad("/tmp/scratch")
            .mcp_instructions("MCP server instructions")
            .memory_section(
                vec!["knowledge content".to_string()],
                vec![("user".to_string(), "conversation history".to_string())],
            )
            .build();

        let reminders = assembly.build_user_message_reminders();
        assert!(reminders.contains("<antml:ref type='git_status'>"));
        assert!(reminders.contains("<antml:ref type='scratchpad'>"));
        assert!(reminders.contains("<antml:ref type='mcp'>"));
        assert!(reminders.contains("<antml:ref type='memory'>"));
    }

    #[test]
    fn backward_compatibility_legacy_fields() {
        let mut assembly = PromptAssembly::new();
        assembly.agent_definition = Some(AgentDefinition::default_agent());
        assembly.soul_content = "SOUL content".to_string();
        assembly.agents_content = "AGENTS content".to_string();
        assembly.memory_content = "MEMORY content".to_string();
        assembly.retrieved_knowledge = vec!["Knowledge 1".to_string()];
        assembly.conversation_history = vec![("user".to_string(), "Hi".to_string())];
        assembly.sensitivity = Sensitivity::Public;

        let result = assembly.assemble();

        // Legacy fields should still work
        assert!(result.contains("SOUL content"));
        assert!(result.contains("Knowledge 1"));
        assert!(result.contains("user: Hi"));
    }

    #[test]
    fn read_file_or_empty_returns_empty_for_missing() {
        let result = read_file_or_empty(Path::new("/nonexistent/file.md"));
        assert_eq!(result, "");
    }
}
