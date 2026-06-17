//! **DEPRECATED**: This module has been merged into `zen_memory::PromptAssembly`.
//!
//! Use `zen_memory::PromptAssembly` for 18-section tiered prompt assembly with:
//! - Cache boundary (`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__`)
//! - Priority override chain (5-level)
//! - Section memoization (cached vs cache-breaking)
//! - Blast radius taxonomy (LOW/MEDIUM/HIGH)
//! - System reminders injection (user messages)
//!
//! This module will be removed in Zen 0.2.0.

use std::fmt;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use zen_core::types::{ConversationTurn, Sensitivity, SessionContext};

use crate::zen_agent::IdentityContext;

// ---------------------------------------------------------------------------
// PromptTier — ordered tiers for system prompt assembly
// ---------------------------------------------------------------------------

/// Ordered tiers for system prompt assembly.
///
/// Lower ordinal = injected first (primacy effect). Maps to Claude Code's
/// 18-section prompt architecture, condensed into 7 tiers for Zen's domain.
///
/// | Tier | Cache Scope | Claude Code Mapping |
/// |------|-------------|---------------------|
/// | Identity | Static | Intro + System Prefix |
/// | Behavior | Static | Doing Tasks + System |
/// | OutputFormat | Static | Output Efficiency |
/// | Tools | Static | Using Tools + Agent Tool |
/// | Context | Dynamic | Environment Info + Memory |
/// | Memory | Dynamic | Conversation History |
/// | UserRules | Dynamic | CLAUDE.md Content |
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PromptTier {
    /// Agent identity: name, role, SOUL.md/AGENTS.md/MEMORY.md content
    Identity = 0,
    /// Behavioral instructions: tone, rules, constraints
    Behavior = 1,
    /// Output format: response schema, style
    OutputFormat = 2,
    /// Tool definitions & usage policies
    Tools = 3,
    /// Dynamic context: session state, environment, date
    Context = 4,
    /// Memory: conversation history, retrieved knowledge
    Memory = 5,
    /// User instructions: project rules, custom overrides
    UserRules = 6,
}

impl fmt::Display for PromptTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PromptTier::Identity => write!(f, "Identity"),
            PromptTier::Behavior => write!(f, "Behavior"),
            PromptTier::OutputFormat => write!(f, "OutputFormat"),
            PromptTier::Tools => write!(f, "Tools"),
            PromptTier::Context => write!(f, "Context"),
            PromptTier::Memory => write!(f, "Memory"),
            PromptTier::UserRules => write!(f, "UserRules"),
        }
    }
}

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
    /// Covers: Identity, Behavior, OutputFormat, Tools tiers.
    Static,
    /// Recomputed every turn. Maps to post-dynamic-boundary.
    ///
    /// Covers: Context, Memory, UserRules tiers.
    Dynamic,
}

impl fmt::Display for CacheScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheScope::Static => write!(f, "Static"),
            CacheScope::Dynamic => write!(f, "Dynamic"),
        }
    }
}

// ---------------------------------------------------------------------------
// PROMPT_CACHE_BOUNDARY — sentinel between static and dynamic zones
// ---------------------------------------------------------------------------

/// Sentinel inserted between static and dynamic zones.
///
/// LLM providers (Claude, OpenAI) cache prompt prefixes up to this marker.
/// Content before the boundary is stable across turns; content after changes
/// every turn (conversation history, fresh context).
pub const PROMPT_CACHE_BOUNDARY: &str = "=== ZEN_STATIC_PROMPT_BOUNDARY ===";

// ---------------------------------------------------------------------------
// PromptSection — a single named section with tier ordering
// ---------------------------------------------------------------------------

/// A single named section in the assembled prompt.
///
/// Each section carries a `tier` for ordering, a `cache_scope` for cache
/// boundary placement, and optional `content` (None = empty section, skipped
/// during assembly).
#[derive(Debug, Clone)]
pub struct PromptSection {
    /// Section name used for logging / debugging.
    pub name: &'static str,
    /// Tier classification for sort ordering.
    pub tier: PromptTier,
    /// Section content. `None` sections are omitted from output.
    pub content: Option<String>,
    /// Cache scope determines placement relative to `PROMPT_CACHE_BOUNDARY`.
    pub cache_scope: CacheScope,
}

// ---------------------------------------------------------------------------
// PromptTemplate — assembles sections into a cached prompt string
// ---------------------------------------------------------------------------

/// Assembles the complete system prompt from tiered sections.
///
/// Assembly algorithm:
/// 1. Sort sections by tier ordinal (primacy effect).
/// 2. Partition into static vs dynamic by cache scope.
/// 3. Concatenate static sections, insert `PROMPT_CACHE_BOUNDARY`, then
///    concatenate dynamic sections.
///
/// Empty sections (`content: None`) are filtered out before assembly.
#[derive(Debug, Clone, Default)]
pub struct PromptTemplate {
    sections: Vec<PromptSection>,
}

impl PromptTemplate {
    /// Create an empty template.
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
        }
    }

    /// Add a section to the template (builder pattern).
    pub fn section(mut self, section: PromptSection) -> Self {
        self.sections.push(section);
        self
    }

    /// Assemble all sections into a single prompt string.
    ///
    /// Returns an empty string if no sections have content.
    pub fn assemble(&self) -> String {
        let mut sorted = self.sections.clone();
        sorted.sort_by_key(|s| s.tier);

        let mut static_parts: Vec<String> = Vec::new();
        let mut dynamic_parts: Vec<String> = Vec::new();

        for section in sorted {
            let Some(content) = section.content else {
                continue; // skip empty sections
            };
            match section.cache_scope {
                CacheScope::Static => static_parts.push(content),
                CacheScope::Dynamic => dynamic_parts.push(content),
            }
        }

        let mut result = static_parts.join("\n\n");
        if !dynamic_parts.is_empty() {
            if !result.is_empty() {
                result.push_str("\n\n");
            }
            result.push_str(PROMPT_CACHE_BOUNDARY);
            result.push_str("\n\n");
            result.push_str(&dynamic_parts.join("\n\n"));
        }
        result
    }

    /// Return the number of registered sections.
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

// ---------------------------------------------------------------------------
// ChatMessage — lightweight turn representation for prompt assembly
// ---------------------------------------------------------------------------

/// Lightweight chat message used in prompt builder for conversation rendering.
///
/// Wraps `ConversationTurn` from zen-core for display formatting.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Role identifier: "user", "assistant", or "system".
    pub role: String,
    /// Message text.
    pub content: String,
}

impl ChatMessage {
    /// Create a new chat message.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }

    /// Create an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new("assistant", content)
    }
}

impl From<ConversationTurn> for ChatMessage {
    fn from(turn: ConversationTurn) -> Self {
        Self::new(turn.role, turn.content)
    }
}

// ---------------------------------------------------------------------------
// PromptBuilder — priority-chain prompt assembly
// ---------------------------------------------------------------------------

/// Builder with priority chain (Claude Code pattern).
///
/// Five priority levels, checked in order:
/// 1. **Override**: replaces everything (used for emergency/system overrides)
/// 2. **Coordinator mode**: swarm/orchestration mode prompt
/// 3. **Agent definition**: custom agent prompt appended to default
/// 4. **Custom prompt**: CLI `--system-prompt` flag appended to default
/// 5. **Default tiered assembly**: full 6-tier assembly (the happy path)
///
/// # Example
///
/// ```ignore
/// let prompt = PromptBuilder::default()
///     .identity(identity_ctx)
///     .behavior(vec!["Be concise".into()])
///     .tools(vec!["read", "write"])
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct PromptBuilder {
    // -- Priority chain fields --
    override_prompt: Option<String>,
    coordinator_mode: bool,
    agent_definition: Option<String>,
    custom_prompt: Option<String>,
    identity: Option<IdentityContext>,
    behavior_constraints: Vec<String>,
    output_format: Option<String>,
    tools: Vec<String>,
    session: Option<SessionContext>,
    custom_instructions: Vec<String>,
    sensitivity: Sensitivity,
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self {
            override_prompt: None,
            coordinator_mode: false,
            agent_definition: None,
            custom_prompt: None,
            identity: None,
            behavior_constraints: Vec::new(),
            output_format: None,
            tools: Vec::new(),
            session: None,
            custom_instructions: Vec::new(),
            sensitivity: Sensitivity::Public,
        }
    }
}

impl PromptBuilder {
    // -----------------------------------------------------------------------
    // Builder API
    // -----------------------------------------------------------------------

    /// Create a new builder with default (empty) fields.
    pub fn new() -> Self {
        Self::default()
    }

    /// Priority 1: Set full override prompt (bypasses all tiered assembly).
    pub fn override_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.override_prompt = Some(prompt.into());
        self
    }

    /// Priority 2: Enable coordinator (swarm) mode.
    pub fn coordinator_mode(mut self, enabled: bool) -> Self {
        self.coordinator_mode = enabled;
        self
    }

    /// Priority 3: Append a custom agent definition to the default prompt.
    pub fn agent_definition(mut self, def: impl Into<String>) -> Self {
        self.agent_definition = Some(def.into());
        self
    }

    /// Priority 4: Append a custom prompt snippet to the default prompt.
    pub fn custom_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.custom_prompt = Some(prompt.into());
        self
    }

    /// Tier 0: Set the agent identity context.
    pub fn identity(mut self, identity: IdentityContext) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Tier 1: Add behavior constraint rules.
    pub fn behavior(mut self, rules: Vec<String>) -> Self {
        self.behavior_constraints = rules;
        self
    }

    /// Tier 2: Set output format specification.
    pub fn output_format(mut self, format: impl Into<String>) -> Self {
        self.output_format = Some(format.into());
        self
    }

    /// Tier 3: Add tool definitions.
    pub fn tools(mut self, tools: Vec<String>) -> Self {
        self.tools = tools;
        self
    }

    /// Tiers 4-5: Set the session context.
    pub fn session(mut self, session: SessionContext) -> Self {
        self.session = Some(session);
        self
    }

    /// Tier 6: Add custom / project-level instructions.
    pub fn custom_instructions(mut self, instructions: Vec<String>) -> Self {
        self.custom_instructions = instructions;
        self
    }

    /// Set the sensitivity level used in blast radius taxonomy.
    pub fn sensitivity(mut self, level: Sensitivity) -> Self {
        self.sensitivity = level;
        self
    }

    // -----------------------------------------------------------------------
    // Build entry point — priority chain
    // -----------------------------------------------------------------------

    /// Build the final prompt string following the 5-level priority chain.
    ///
    /// Priority 1 > Priority 2 > Priority 3 > Priority 4 > Priority 5 (default tiered).
    pub fn build(&self) -> String {
        // Priority 1: Override replaces everything
        if let Some(ref override_prompt) = self.override_prompt {
            return override_prompt.clone();
        }

        // Priority 2: Coordinator mode
        if self.coordinator_mode {
            return self.build_coordinator_prompt();
        }

        // Priority 3: Custom agent definition appended to default
        if let Some(ref def) = self.agent_definition {
            return format!("{}\n\n{}", self.build_default_prompt(), def);
        }

        // Priority 4: Custom prompt appended to default
        if let Some(ref custom) = self.custom_prompt {
            return format!("{}\n\n{}", self.build_default_prompt(), custom);
        }

        // Priority 5: Default tiered assembly
        self.build_default_prompt()
    }

    // -----------------------------------------------------------------------
    // Default tiered assembly (Priority 5)
    // -----------------------------------------------------------------------

    /// Assemble the full 6-tier prompt (the standard / happy path).
    fn build_default_prompt(&self) -> String {
        let mut template = PromptTemplate::new();

        // Tier 0: Identity (STATIC)
        if let Some(ref identity) = self.identity {
            if !identity.soul_content.is_empty() {
                template = template.section(PromptSection {
                    name: "soul",
                    tier: PromptTier::Identity,
                    content: Some(identity.soul_content.clone()),
                    cache_scope: CacheScope::Static,
                });
            }
            if !identity.agents_content.is_empty() {
                template = template.section(PromptSection {
                    name: "agents",
                    tier: PromptTier::Identity,
                    content: Some(identity.agents_content.clone()),
                    cache_scope: CacheScope::Static,
                });
            }
            if !identity.memory_content.is_empty() {
                template = template.section(PromptSection {
                    name: "memory_file",
                    tier: PromptTier::Identity,
                    content: Some(identity.memory_content.clone()),
                    cache_scope: CacheScope::Static,
                });
            }
        }

        // Tier 1: Behavior (STATIC)
        template = template.section(PromptSection {
            name: "behavior",
            tier: PromptTier::Behavior,
            content: Some(self.build_behavior_section()),
            cache_scope: CacheScope::Static,
        });

        // Tier 2: Output Format (STATIC)
        if let Some(ref format) = self.output_format {
            template = template.section(PromptSection {
                name: "output_format",
                tier: PromptTier::OutputFormat,
                content: Some(format.clone()),
                cache_scope: CacheScope::Static,
            });
        }

        // Tier 3: Tools (STATIC)
        if !self.tools.is_empty() {
            template = template.section(PromptSection {
                name: "tools",
                tier: PromptTier::Tools,
                content: Some(self.build_tools_section()),
                cache_scope: CacheScope::Static,
            });
        }

        // Tier 4: Context (DYNAMIC)
        if let Some(ref session) = self.session {
            template = template.section(PromptSection {
                name: "context",
                tier: PromptTier::Context,
                content: Some(build_context_section(session)),
                cache_scope: CacheScope::Dynamic,
            });
        }

        // Tier 5: Memory (DYNAMIC)
        if let Some(ref session) = self.session {
            template = template.section(PromptSection {
                name: "conversation_history",
                tier: PromptTier::Memory,
                content: Some(build_conversation_section(session)),
                cache_scope: CacheScope::Dynamic,
            });
        }

        // Tier 6: User Rules (DYNAMIC)
        if !self.custom_instructions.is_empty() {
            template = template.section(PromptSection {
                name: "custom_instructions",
                tier: PromptTier::UserRules,
                content: Some(self.custom_instructions.join("\n")),
                cache_scope: CacheScope::Dynamic,
            });
        }

        template.assemble()
    }

    // -----------------------------------------------------------------------
    // Priority 2: Coordinator prompt
    // -----------------------------------------------------------------------

    /// Build the coordinator (swarm/orchestration) mode prompt.
    ///
    /// Coordinator agents supervise other agents; the prompt is focused on
    /// delegation, routing, and quality gates.
    fn build_coordinator_prompt(&self) -> String {
        let identity_block = self.identity.as_ref().map_or_else(String::new, |id| {
            let mut block = String::new();
            if !id.soul_content.is_empty() {
                block.push_str(&id.soul_content);
                block.push('\n');
            }
            if !id.agents_content.is_empty() {
                block.push_str(&id.agents_content);
                block.push('\n');
            }
            if !id.memory_content.is_empty() {
                block.push_str(&id.memory_content);
            }
            block
        });

        let mut parts = Vec::new();
        if !identity_block.is_empty() {
            parts.push(identity_block);
        }
        parts.push(COORDINATOR_BEHAVIOR.to_string());
        parts.push(self.build_blast_radius_section());
        if !self.tools.is_empty() {
            parts.push(self.build_tools_section());
        }
        parts.push(COORDINATOR_GATEKEEPING.to_string());

        // Append dynamic context if available
        if let Some(ref session) = self.session {
            parts.push(PROMPT_CACHE_BOUNDARY.to_string());
            parts.push(build_context_section(session));
            parts.push(build_conversation_section(session));
        }

        parts.join("\n\n")
    }

    // -----------------------------------------------------------------------
    // Tier builders (static)
    // -----------------------------------------------------------------------

    /// Build the behavior constraints section (Tier 1).
    fn build_behavior_section(&self) -> String {
        let mut rules = vec![
            "You are a helpful Zen assistant.".to_string(),
            "Answer concisely and directly.".to_string(),
        ];

        // Anti-YAGNI: explicit negative instructions (Claude Code pattern)
        rules.push("Do not add features beyond what was asked.".to_string());
        rules.push("Do not create unnecessary abstractions.".to_string());
        rules.push("Do not add error handling for impossible scenarios.".to_string());

        // Append custom constraints
        rules.extend(self.behavior_constraints.clone());

        // Blast radius taxonomy
        rules.push(self.build_blast_radius_section());

        rules.join("\n")
    }

    /// Build the blast radius taxonomy section.
    ///
    /// Classifies actions by risk level to guide the agent's confidence in
    /// executing them without explicit user confirmation.
    fn build_blast_radius_section(&self) -> String {
        format!(
            r#"## Blast Radius Taxonomy

When executing actions, classify the risk level:

- **LOW**: Read files, search the knowledge base, format output
  Action: Execute without asking

- **MEDIUM**: Create notes, modify wiki pages, run searches
  Action: Execute with caution

- **HIGH**: Delete knowledge base data, run consolidation, execute SQL
  Action: Require explicit user confirmation

Current sensitivity level: {}"#,
            self.sensitivity
        )
    }

    /// Build the tools section (Tier 3).
    fn build_tools_section(&self) -> String {
        format!("## Available Tools\n\n{}", self.tools.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// Standalone session section builders
// ---------------------------------------------------------------------------

/// Build the dynamic session context section (Tier 4).
/// Active context is budget-managed via ContextPack; full history is preserved in .mv2 archive.
///
/// Includes agent name, session ID, turn count, timestamp.
pub fn build_context_section(session: &SessionContext) -> String {
    format!(
        r#"## Session Context
- Agent: {}
- Session ID: {}
- Message count: {}
- Current date: {}"#,
        session.agent_name,
        session.session_id,
        session.conversation.len(),
        Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
    )
}

/// Build the conversation history section (Tier 5).
///
/// Formats the `SessionContext.conversation` vector into a readable log.
/// Returns an empty string if no conversation turns exist.
pub fn build_conversation_section(session: &SessionContext) -> String {
    if session.conversation.is_empty() {
        return String::new();
    }
    let history: Vec<String> = session
        .conversation
        .iter()
        .map(|turn| format!("{}: {}", turn.role, turn.content))
        .collect();
    format!("## Conversation History\n{}", history.join("\n"))
}

// ---------------------------------------------------------------------------
// Coordinator-specific prompt blocks (static, cached)
// ---------------------------------------------------------------------------

/// Fixed behavior block for coordinator-mode agents.
const COORDINATOR_BEHAVIOR: &str = "\
## Coordinator Behavior

You are a Zen coordinator agent. Your role is to:

1. Receive complex, multi-step tasks from the user.
2. Decompose tasks into delegable sub-tasks.
3. Route sub-tasks to the appropriate specialist agents.
4. Collect results, validate quality, and synthesize a final response.

Guidelines:
- Prefer parallel delegation when sub-tasks are independent.
- Validate outputs against the original acceptance criteria.
- Escalate to the user when a sub-task fails repeatedly or ambiguity arises.
- Maintain audit trails for all delegated work.";

/// Fixed gatekeeping block appended to coordinator prompts.
const COORDINATOR_GATEKEEPING: &str = "\
## Quality Gatekeeping

After each agent returns a result, verify:
- Does the output satisfy the sub-task requirements?
- Are there any safety or sensitivity violations?
- Should the result be escalated for human review?

Reject and re-delegate if quality gates fail.";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::types::ConversationTurn;

    fn make_test_session() -> SessionContext {
        let mut session = SessionContext::new("test-agent".into(), "You are a test agent.".into());
        session.conversation.push(ConversationTurn {
            role: "user".into(),
            content: "Hello".into(),
        });
        session.conversation.push(ConversationTurn {
            role: "assistant".into(),
            content: "Hi there!".into(),
        });
        session
    }

    #[test]
    fn prompt_tier_ordering() {
        assert!(PromptTier::Identity < PromptTier::Behavior);
        assert!(PromptTier::Behavior < PromptTier::OutputFormat);
        assert!(PromptTier::OutputFormat < PromptTier::Tools);
        assert!(PromptTier::Tools < PromptTier::Context);
        assert!(PromptTier::Context < PromptTier::Memory);
        assert!(PromptTier::Memory < PromptTier::UserRules);
    }

    #[test]
    fn cache_scope_display() {
        assert_eq!(CacheScope::Static.to_string(), "Static");
        assert_eq!(CacheScope::Dynamic.to_string(), "Dynamic");
    }

    #[test]
    fn template_assemble_static_only() {
        let template = PromptTemplate::new()
            .section(PromptSection {
                name: "first",
                tier: PromptTier::Identity,
                content: Some("Hello".into()),
                cache_scope: CacheScope::Static,
            })
            .section(PromptSection {
                name: "second",
                tier: PromptTier::Behavior,
                content: Some("World".into()),
                cache_scope: CacheScope::Static,
            });

        let result = template.assemble();
        assert!(result.contains("Hello"));
        assert!(result.contains("World"));
        assert!(!result.contains(PROMPT_CACHE_BOUNDARY));
    }

    #[test]
    fn template_assemble_with_boundary() {
        let template = PromptTemplate::new()
            .section(PromptSection {
                name: "static",
                tier: PromptTier::Identity,
                content: Some("STATIC_CONTENT".into()),
                cache_scope: CacheScope::Static,
            })
            .section(PromptSection {
                name: "dynamic",
                tier: PromptTier::Context,
                content: Some("DYNAMIC_CONTENT".into()),
                cache_scope: CacheScope::Dynamic,
            });

        let result = template.assemble();
        let boundary_pos = result
            .find(PROMPT_CACHE_BOUNDARY)
            .expect("should have boundary");
        assert!(
            result[..boundary_pos].contains("STATIC_CONTENT"),
            "static content should precede boundary"
        );
        assert!(
            result[boundary_pos + PROMPT_CACHE_BOUNDARY.len()..].contains("DYNAMIC_CONTENT"),
            "dynamic content should follow boundary"
        );
    }

    #[test]
    fn template_assemble_skips_empty_sections() {
        let template = PromptTemplate::new()
            .section(PromptSection {
                name: "empty",
                tier: PromptTier::Identity,
                content: None,
                cache_scope: CacheScope::Static,
            })
            .section(PromptSection {
                name: "filled",
                tier: PromptTier::Behavior,
                content: Some("CONTENT".into()),
                cache_scope: CacheScope::Static,
            });

        let result = template.assemble();
        assert!(!result.contains("empty"));
        assert!(result.contains("CONTENT"));
    }

    #[test]
    fn builder_priority_override() {
        let prompt = PromptBuilder::new()
            .override_prompt("FULL OVERRIDE")
            .identity(IdentityContext::default())
            .build();
        assert_eq!(prompt, "FULL OVERRIDE");
    }

    #[test]
    fn builder_priority_coordinator() {
        let prompt = PromptBuilder::new()
            .coordinator_mode(true)
            .sensitivity(Sensitivity::Public)
            .build();
        assert!(prompt.contains("Coordinator Behavior"));
        assert!(prompt.contains("Quality Gatekeeping"));
    }

    #[test]
    fn builder_priority_agent_definition() {
        let prompt = PromptBuilder::new()
            .agent_definition("CUSTOM AGENT RULES")
            .sensitivity(Sensitivity::Public)
            .build();
        assert!(prompt.contains("CUSTOM AGENT RULES"));
        assert!(prompt.contains("Zen assistant"));
    }

    #[test]
    fn builder_priority_custom_prompt() {
        let prompt = PromptBuilder::new()
            .custom_prompt("EXTRA INSTRUCTIONS")
            .sensitivity(Sensitivity::Public)
            .build();
        assert!(prompt.contains("EXTRA INSTRUCTIONS"));
        assert!(prompt.contains("Zen assistant"));
    }

    #[test]
    fn builder_default_assembly() {
        let identity = IdentityContext {
            soul_content: "SOUL_CONTENT".into(),
            agents_content: "AGENTS_CONTENT".into(),
            ..Default::default()
        };

        let prompt = PromptBuilder::new()
            .identity(identity)
            .behavior(vec!["Be kind".into()])
            .output_format("Return JSON.")
            .tools(vec!["read".into(), "write".into()])
            .session(make_test_session())
            .custom_instructions(vec!["Follow conventions".into()])
            .sensitivity(Sensitivity::Confidential)
            .build();

        // Static content present
        assert!(prompt.contains("SOUL_CONTENT"));
        assert!(prompt.contains("AGENTS_CONTENT"));
        assert!(prompt.contains("Be kind"));
        assert!(prompt.contains("Return JSON."));
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("read"));
        assert!(prompt.contains("write"));

        // Dynamic content present
        assert!(prompt.contains("Session Context"));
        assert!(prompt.contains("Conversation History"));
        assert!(prompt.contains("Follow conventions"));

        // Cache boundary present
        assert!(prompt.contains(PROMPT_CACHE_BOUNDARY));

        // Static precedes dynamic
        let boundary_pos = prompt.find(PROMPT_CACHE_BOUNDARY).unwrap();
        assert!(prompt[..boundary_pos].contains("SOUL_CONTENT"));
        assert!(prompt[boundary_pos..].contains("Session Context"));
    }

    #[test]
    fn builder_sensitivity_in_blast_radius() {
        let prompt = PromptBuilder::new()
            .sensitivity(Sensitivity::Confidential)
            .build();
        assert!(prompt.contains("Current sensitivity level: Confidential"));
    }

    #[test]
    fn build_context_section_format() {
        let session = make_test_session();
        let ctx = build_context_section(&session);
        assert!(ctx.contains("test-agent"));
        assert!(ctx.contains("Message count: 2"));
        assert!(ctx.contains("Session Context"));
    }

    #[test]
    fn build_conversation_section_empty() {
        let session = SessionContext::new("empty".into(), String::new());
        let hist = build_conversation_section(&session);
        assert!(hist.is_empty());
    }

    #[test]
    fn build_conversation_section_populated() {
        let session = make_test_session();
        let hist = build_conversation_section(&session);
        assert!(hist.contains("user: Hello"));
        assert!(hist.contains("assistant: Hi there!"));
    }

    #[test]
    fn chat_message_from_conversation_turn() {
        let turn = ConversationTurn {
            role: "user".into(),
            content: "test".into(),
        };
        let msg: ChatMessage = turn.into();
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "test");
    }

    #[test]
    fn section_count() {
        let template = PromptTemplate::new()
            .section(PromptSection {
                name: "a",
                tier: PromptTier::Identity,
                content: None,
                cache_scope: CacheScope::Static,
            })
            .section(PromptSection {
                name: "b",
                tier: PromptTier::Behavior,
                content: Some("x".into()),
                cache_scope: CacheScope::Static,
            });
        assert_eq!(template.section_count(), 2);
    }
}
