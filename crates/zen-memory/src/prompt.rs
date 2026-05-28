use std::fs;
use std::path::Path;

use zen_core::AgentDefinition;
use zen_core::paths::ZenPaths;

/// System prompt assembly for agent sessions.
///
/// Assembly order (per FR-TUI-018):
/// 1. AgentDefinition.prompt_template (core behavior)
/// 2. SOUL.md content (personality layer)
/// 3. AGENTS.md content (operating instructions)
/// 4. MEMORY.md content (durable facts)
/// 5. Retrieved knowledge (external context)
/// 6. Conversation history (session context)
pub struct PromptAssembly {
    pub agent_definition: Option<AgentDefinition>,
    pub soul_content: String,
    pub agents_content: String,
    pub memory_content: String,
    pub retrieved_knowledge: Vec<String>,
    pub conversation_history: Vec<(String, String)>,
}

impl Default for PromptAssembly {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptAssembly {
    pub fn new() -> Self {
        Self {
            agent_definition: None,
            soul_content: String::new(),
            agents_content: String::new(),
            memory_content: String::new(),
            retrieved_knowledge: Vec::new(),
            conversation_history: Vec::new(),
        }
    }

    /// Assemble the full system prompt in the defined order.
    pub fn assemble(&self) -> String {
        let mut prompt = String::new();

        // 1. Agent definition prompt template
        if let Some(def) = &self.agent_definition {
            prompt.push_str(&def.prompt_template);
            prompt.push_str("\n\n---\n\n");
        }

        // 2. SOUL.md
        if !self.soul_content.is_empty() {
            prompt.push_str("## Personality & Principles\n\n");
            prompt.push_str(&self.soul_content);
            prompt.push_str("\n\n---\n\n");
        }

        // 3. AGENTS.md
        if !self.agents_content.is_empty() {
            prompt.push_str("## Operating Instructions\n\n");
            prompt.push_str(&self.agents_content);
            prompt.push_str("\n\n---\n\n");
        }

        // 4. MEMORY.md
        if !self.memory_content.is_empty() {
            prompt.push_str("## Durable Facts\n\n");
            prompt.push_str(&self.memory_content);
            prompt.push_str("\n\n---\n\n");
        }

        // 5. Retrieved knowledge
        if !self.retrieved_knowledge.is_empty() {
            prompt.push_str("## Relevant Context\n\n");
            for (i, note) in self.retrieved_knowledge.iter().enumerate() {
                prompt.push_str(&format!("[{}] {}\n", i + 1, note));
            }
            prompt.push_str("\n---\n\n");
        }

        // 6. Conversation history
        if !self.conversation_history.is_empty() {
            prompt.push_str("## Conversation History\n\n");
            for (role, content) in &self.conversation_history {
                prompt.push_str(&format!("{}: {}\n", role, content));
            }
            prompt.push_str("\n---\n\n");
        }

        prompt
    }

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
}

fn read_file_or_empty(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_empty_inputs() {
        let assembly = PromptAssembly::new();
        let result = assembly.assemble();
        assert_eq!(result, "");
    }

    #[test]
    fn assemble_with_agent_definition() {
        let def = AgentDefinition::default_agent();
        let mut assembly = PromptAssembly::new();
        assembly.agent_definition = Some(def);
        let result = assembly.assemble();
        assert!(result.contains("You are Zen"));
        assert!(result.contains("---"));
    }

    #[test]
    fn assemble_full_order() {
        let def = AgentDefinition::default_agent();
        let mut assembly = PromptAssembly::new();
        assembly.agent_definition = Some(def);
        assembly.soul_content = "Be helpful.".to_string();
        assembly.agents_content = "Follow rules.".to_string();
        assembly.memory_content = "User prefers Rust.".to_string();
        assembly.retrieved_knowledge = vec!["Note 1".to_string()];
        assembly.conversation_history = vec![("user".to_string(), "Hi".to_string())];

        let result = assembly.assemble();

        // Verify order: definition → soul → agents → memory → knowledge → conversation
        let def_pos = result.find("You are Zen").unwrap();
        let soul_pos = result.find("Personality").unwrap();
        let agents_pos = result.find("Operating Instructions").unwrap();
        let memory_pos = result.find("Durable Facts").unwrap();
        let knowledge_pos = result.find("Relevant Context").unwrap();
        let conversation_pos = result.find("Conversation History").unwrap();

        assert!(def_pos < soul_pos);
        assert!(soul_pos < agents_pos);
        assert!(agents_pos < memory_pos);
        assert!(memory_pos < knowledge_pos);
        assert!(knowledge_pos < conversation_pos);
    }

    #[test]
    fn read_file_or_empty_returns_empty_for_missing_file() {
        let result = read_file_or_empty(Path::new("/nonexistent/file.md"));
        assert_eq!(result, "");
    }
}
