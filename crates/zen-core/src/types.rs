use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Sensitivity (FR-080)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Sensitivity {
    Public,
    Private,
    Confidential,
}

impl fmt::Display for Sensitivity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sensitivity::Public => write!(f, "Public"),
            Sensitivity::Private => write!(f, "Private"),
            Sensitivity::Confidential => write!(f, "Confidential"),
        }
    }
}

impl Sensitivity {
    pub fn max(a: Self, b: Self) -> Self {
        if a >= b { a } else { b }
    }

    pub fn max_of(items: &[Self]) -> Self {
        items
            .iter()
            .copied()
            .reduce(Self::max)
            .unwrap_or(Self::Public)
    }
}

// ---------------------------------------------------------------------------
// SessionStatus (FR-078, unified across zen-cli and zen-memory)
// ---------------------------------------------------------------------------

/// Session lifecycle states per data-model.md §3.9.
///
/// State transitions:
/// - Active → Compacted (context truncated) → Active (reactivated)
/// - Active → Completed (normal end)
/// - Active/Compacted → Failed (error during execution)
/// - Completed/Failed/Compacted → Archived (terminal, preserved for history)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// Session is currently running.
    Active,
    /// Context window was compressed (conversation truncated).
    Compacted,
    /// Session finished normally.
    Completed,
    /// Session ended with an error.
    Failed,
    /// Session ended, preserved for history (terminal state).
    Archived,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStatus::Active => write!(f, "Active"),
            SessionStatus::Compacted => write!(f, "Compacted"),
            SessionStatus::Completed => write!(f, "Completed"),
            SessionStatus::Failed => write!(f, "Failed"),
            SessionStatus::Archived => write!(f, "Archived"),
        }
    }
}

// ---------------------------------------------------------------------------
// ComplexityLevel & Task (FR-AGENT-004)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComplexityLevel {
    Simple,
    Standard,
    Complex,
    Critical,
}

impl fmt::Display for ComplexityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComplexityLevel::Simple => write!(f, "Simple"),
            ComplexityLevel::Standard => write!(f, "Standard"),
            ComplexityLevel::Complex => write!(f, "Complex"),
            ComplexityLevel::Critical => write!(f, "Critical"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskType {
    Code,
    Text,
    Data,
}

impl fmt::Display for TaskType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskType::Code => write!(f, "Code"),
            TaskType::Text => write!(f, "Text"),
            TaskType::Data => write!(f, "Data"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub user_input: String,
    pub semantic_entropy: f64,
    pub complexity: ComplexityLevel,
    pub physical_attribute: TaskType,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub metadata: HashMap<String, String>,
}

impl Task {
    pub fn new(user_input: &str, semantic_entropy: f64, task_type: TaskType) -> Self {
        let complexity = Self::classify_complexity(semantic_entropy, &task_type);
        Self {
            id: Uuid::now_v7(),
            user_input: user_input.to_string(),
            semantic_entropy,
            complexity,
            physical_attribute: task_type,
            created_at: chrono::Utc::now(),
            metadata: HashMap::new(),
        }
    }

    fn classify_complexity(entropy: f64, task_type: &TaskType) -> ComplexityLevel {
        match (entropy, task_type) {
            (e, _) if e > 0.9 => ComplexityLevel::Critical,
            (e, TaskType::Code) if e < 0.3 => ComplexityLevel::Simple,
            (e, TaskType::Code) if e < 0.6 => ComplexityLevel::Standard,
            (e, TaskType::Text) if e > 0.7 => ComplexityLevel::Complex,
            _ => ComplexityLevel::Standard,
        }
    }
}

pub struct SemanticEntropy;

impl SemanticEntropy {
    pub fn calculate(text: &str) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        let freq = Self::char_frequency(text);
        let len = text.len() as f64;
        freq.values()
            .map(|&c| {
                let p = c as f64 / len;
                if p > 0.0 {
                    -p * p.log2()
                } else {
                    0.0
                }
            })
            .sum()
    }

    fn char_frequency(text: &str) -> HashMap<char, usize> {
        let mut freq = HashMap::new();
        for c in text.chars() {
            *freq.entry(c).or_insert(0) += 1;
        }
        freq
    }
}

// ---------------------------------------------------------------------------
// SessionContext, RetrievedNote, ConversationTurn (FR-076, FR-081)
// ---------------------------------------------------------------------------

/// Assembled session context for agent orchestration.
///
/// Composes three independent peer crate outputs:
/// - zen-agents: agent definition (WHO am I?)
/// - zen-memory: internal context (WHAT do I know about myself?)
/// - zen-knowledge: external memory (WHAT do I know about the world?)
///
/// Per FR-076: The orchestrator (zen-agents) is responsible for calling each
/// crate independently and constructing this composed context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContext {
    /// Unique session identifier (UUID v7)
    pub session_id: Uuid,
    /// Agent name (zen-agents owns the full profile)
    pub agent_name: String,
    /// Assembled system prompt (SOUL.md + AGENTS.md + agent instructions)
    pub system_prompt: String,
    /// Retrieved knowledge from zen-knowledge (search results, wiki pages)
    pub knowledge: Vec<RetrievedNote>,
    /// Computed sensitivity policy from retrieved notes
    pub sensitivity_policy: Sensitivity,
    /// Conversation history for this session
    pub conversation: Vec<ConversationTurn>,
    /// Token budget for this session
    pub max_tokens: usize,
}

/// A note retrieved from the knowledge base during session context assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedNote {
    /// Note file path
    pub path: String,
    /// Note content (truncated if necessary)
    pub content: String,
    /// Note sensitivity level
    pub sensitivity: Sensitivity,
    /// Relevance score (0.0-1.0)
    pub relevance: f64,
}

/// A single turn in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    /// Role: "user" or "assistant"
    pub role: String,
    /// Message content
    pub content: String,
}

impl SessionContext {
    pub fn new(agent_name: String, system_prompt: String) -> Self {
        Self {
            session_id: Uuid::now_v7(),
            agent_name,
            system_prompt,
            knowledge: Vec::new(),
            sensitivity_policy: Sensitivity::Public,
            conversation: Vec::new(),
            max_tokens: 4096,
        }
    }

    /// Add retrieved knowledge to the session context.
    pub fn add_knowledge(&mut self, notes: Vec<RetrievedNote>) {
        self.knowledge.extend(notes);
    }

    /// Add a conversation turn.
    pub fn add_turn(&mut self, role: &str, content: &str) {
        self.conversation.push(ConversationTurn {
            role: role.to_string(),
            content: content.to_string(),
        });
    }

    /// Build the full prompt for LLM submission.
    pub fn build_prompt(&self, user_query: &str) -> String {
        let mut prompt = self.system_prompt.clone();

        if !self.knowledge.is_empty() {
            prompt.push_str("\n\nRelevant context:\n");
            for (i, note) in self.knowledge.iter().enumerate() {
                prompt.push_str(&format!("[{}] {}\n", i + 1, note.content));
            }
        }

        if !self.conversation.is_empty() {
            prompt.push_str("\n\nConversation history:\n");
            for turn in &self.conversation {
                prompt.push_str(&format!("{}: {}\n", turn.role, turn.content));
            }
        }

        prompt.push_str(&format!("\nUser: {}\n\nAssistant:", user_query));
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_text_has_zero_entropy() {
        let entropy = SemanticEntropy::calculate("");
        assert_eq!(entropy, 0.0);
    }

    #[test]
    fn test_uniform_text_has_high_entropy() {
        let entropy = SemanticEntropy::calculate("abcdefghij");
        assert!(entropy > 3.0);
    }

    #[test]
    fn test_repeated_text_has_low_entropy() {
        let entropy = SemanticEntropy::calculate("aaaaaaaaaa");
        assert!(entropy < 1.0);
    }

    #[test]
    fn test_task_classification_code_low_entropy() {
        let task = Task::new("fn add(a: i32, b: i32) -> i32 { a + b }", 0.2, TaskType::Code);
        assert!(matches!(task.complexity, ComplexityLevel::Simple));
    }

    #[test]
    fn test_task_classification_code_medium_entropy() {
        let task = Task::new("implement a complex async state machine", 0.5, TaskType::Code);
        assert!(matches!(task.complexity, ComplexityLevel::Standard));
    }

    #[test]
    fn test_task_classification_text_high_entropy() {
        let task = Task::new("analyze philosophical implications of consciousness", 0.8, TaskType::Text);
        assert!(matches!(task.complexity, ComplexityLevel::Complex));
    }

    #[test]
    fn test_task_classification_very_high_entropy() {
        // Use explicit high entropy value to trigger Critical classification
        let task = Task::new("some input", 0.95, TaskType::Text);
        assert!(matches!(task.complexity, ComplexityLevel::Critical));
    }

    #[test]
    fn test_session_context_creation() {
        let ctx = SessionContext::new("Sisyphus".to_string(), "system prompt".to_string());
        assert_eq!(ctx.agent_name, "Sisyphus");
        assert_eq!(ctx.system_prompt, "system prompt");
        assert!(ctx.knowledge.is_empty());
        assert!(ctx.conversation.is_empty());
        assert_eq!(ctx.max_tokens, 4096);
    }

    #[test]
    fn test_session_context_add_turn() {
        let mut ctx = SessionContext::new("test".to_string(), "prompt".to_string());
        ctx.add_turn("user", "hello");
        ctx.add_turn("assistant", "hi");
        assert_eq!(ctx.conversation.len(), 2);
    }

    #[test]
    fn test_session_context_build_prompt() {
        let mut ctx = SessionContext::new("test".to_string(), "system".to_string());
        ctx.add_turn("user", "question");
        let prompt = ctx.build_prompt("follow-up");
        assert!(prompt.contains("system"));
        assert!(prompt.contains("question"));
        assert!(prompt.contains("follow-up"));
    }

    #[test]
    fn test_retrieved_note_serialization() {
        let note = RetrievedNote {
            path: "/test.md".to_string(),
            content: "content".to_string(),
            sensitivity: Sensitivity::Private,
            relevance: 0.9,
        };
        let json = serde_json::to_string(&note).unwrap();
        let decoded: RetrievedNote = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.path, "/test.md");
    }
}
