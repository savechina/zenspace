use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info};
use uuid::Uuid;

use crate::paths::ZenPaths;
use crate::session_index::SessionIndex;

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
// SessionEvent — single-file event stream (Codex-style)
// ---------------------------------------------------------------------------

/// A single event in a session's `.jsonl` file.
///
/// Each session is a single `<uuid>.jsonl` file containing ordered events:
///   - `session/meta` (first event) — replaces SessionEntity metadata JSON
///   - `chat/turn` — conversation turns (replaces separate chat.jsonl)
///   - future: `tool/call`, `session/status`, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum SessionEvent {
    #[serde(rename = "session/meta")]
    Meta(SessionEntity),
    #[serde(rename = "chat/turn")]
    Turn(ChatTurnEvent),
}

/// Payload for a `chat/turn` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurnEvent {
    pub role: String,
    pub content: String,
    /// Unix timestamp seconds (i64, Codex-compatible via chrono::serde::ts_seconds).
    #[serde(with = "chrono::serde::ts_seconds")]
    pub timestamp: DateTime<Utc>,
}

impl SessionEvent {
    /// Read the first (meta) event from a `.jsonl` file.
    pub fn read_meta(path: &std::path::Path) -> Result<SessionEntity> {
        let line = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read session file: {}", path.display()))?
            .lines()
            .next()
            .ok_or_else(|| anyhow::anyhow!("empty session file: {}", path.display()))?
            .to_string();
        match serde_json::from_str::<SessionEvent>(&line)
            .with_context(|| format!("failed to parse session event: {}", path.display()))?
        {
            SessionEvent::Meta(entity) => Ok(entity),
            _ => anyhow::bail!(
                "expected session/meta event as first line in {}",
                path.display()
            ),
        }
    }

    /// Write a `session/meta` event as the first line of a `.jsonl` file.
    /// If the file already exists, the meta line is overwritten (line 1).
    pub fn write_meta(path: &std::path::Path, entity: &SessionEntity) -> Result<()> {
        let meta_line = serde_json::to_string(&SessionEvent::Meta(entity.clone()))
            .context("failed to serialize session/meta event")?;

        if path.exists() {
            // Overwrite first line only, preserve conversation events
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read: {}", path.display()))?;
            let mut lines: Vec<&str> = content.lines().collect();
            if lines.is_empty() {
                lines.push(&meta_line);
            } else {
                lines[0] = &meta_line;
            }
            let new_content = lines.join("\n") + "\n";
            std::fs::write(path, new_content)
                .with_context(|| format!("failed to write: {}", path.display()))?;
        } else {
            // New file — write meta event
            let mut file = std::fs::File::create(path)
                .with_context(|| format!("failed to create: {}", path.display()))?;
            use std::io::Write;
            writeln!(file, "{}", meta_line)
                .with_context(|| format!("failed to write meta event: {}", path.display()))?;
        }
        Ok(())
    }
}

/// Read a SessionEntity from a file, detecting format by extension.
///
/// - `.jsonl` → parse first line as `session/meta` event
/// - `.json` → legacy format, parse whole file as `SessionEntity`
pub fn load_session_from_file(path: &std::path::Path) -> Result<SessionEntity> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("jsonl") => SessionEvent::read_meta(path),
        _ => {
            let json = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read session file: {}", path.display()))?;
            serde_json::from_str::<SessionEntity>(&json)
                .with_context(|| format!("failed to parse session file: {}", path.display()))
        }
    }
}

// ---------------------------------------------------------------------------
// SessionEntity (FR-078, FR-081) — canonical definition
// ---------------------------------------------------------------------------

/// Session entity persisted as the first `session/meta` event in `<id>.jsonl`.
///
/// Per data-model.md §3.9: JSONL file is primary storage (Tier 2 derived cache).
/// SQLite table is derived from these files for fast queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntity {
    /// Unique session identifier (UUID v7).
    pub id: String,
    /// Agent name from zen-agents registry.
    pub agent_name: String,
    /// User-assigned session title (optional).
    pub title: Option<String>,
    /// Parent session ID (for fork tracking).
    pub parent_id: Option<String>,
    /// Computed max sensitivity across retrieved notes.
    pub sensitivity_policy: Sensitivity,
    /// Session creation time (Unix timestamp seconds).
    #[serde(with = "chrono::serde::ts_seconds")]
    pub created_at: DateTime<Utc>,
    /// Last session activity (Unix timestamp seconds).
    #[serde(with = "chrono::serde::ts_seconds")]
    pub updated_at: DateTime<Utc>,
    /// Session lifecycle state.
    pub status: SessionStatus,
    /// Workspace path for the session.
    pub workspace: String,
}

impl SessionEntity {
    /// Create a new session entity with the given agent name and workspace.
    pub fn new(agent_name: &str, workspace: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::now_v7().to_string(),
            agent_name: agent_name.to_string(),
            title: None,
            parent_id: None,
            sensitivity_policy: Sensitivity::Private, // Safe default (FR-071)
            created_at: now,
            updated_at: now,
            status: SessionStatus::Active,
            workspace: workspace.to_string(),
        }
    }

    /// Create a fork of this session with a new ID.
    pub fn fork(&self, title: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::now_v7().to_string(),
            agent_name: self.agent_name.clone(),
            title: title.or_else(|| self.title.as_ref().map(|t| format!("{} (fork)", t))),
            parent_id: Some(self.id.clone()),
            sensitivity_policy: self.sensitivity_policy,
            created_at: now,
            updated_at: now,
            status: SessionStatus::Active,
            workspace: self.workspace.clone(),
        }
    }

    /// Rename this session.
    pub fn rename(&mut self, title: String) -> Result<()> {
        self.title = Some(title);
        self.updated_at = Utc::now();
        self.save()?;
        info!(session_id = %self.id, title = %self.title.as_ref().unwrap(), "session renamed");
        Ok(())
    }

    /// Save this session to `<id>.jsonl` with a `session/meta` event.
    ///
    /// The file is stored at `~/.zen/sessions/YYYY/MM/DD/<id>.jsonl`.
    /// If the file already exists (from a previous save), only the first line
    /// (the `session/meta` event) is updated — conversation events are preserved.
    pub fn save(&self) -> Result<PathBuf> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;
        let date_dir = paths.session_dir_for_date(self.created_at);
        std::fs::create_dir_all(&date_dir).with_context(|| {
            format!(
                "failed to create sessions directory: {}",
                date_dir.display()
            )
        })?;

        let file_path = date_dir.join(format!("{}.jsonl", self.id));
        SessionEvent::write_meta(&file_path, self)?;

        let relative_path = format!(
            "{}/{}/{}/{}.jsonl",
            self.created_at.format("%Y"),
            self.created_at.format("%m"),
            self.created_at.format("%d"),
            self.id,
        );

        if let Ok(index) = SessionIndex::open(&paths.db())
            && let Err(e) = index.upsert(
                &self.id,
                &relative_path,
                &self.agent_name,
                &self.status.to_string(),
                &self.created_at.to_rfc3339(),
                &self.updated_at.to_rfc3339(),
                &self.workspace,
            )
        {
            debug!("failed to upsert session index: {}", e);
        }

        debug!("saved session {} to {}", self.id, file_path.display());
        Ok(file_path)
    }

    pub fn load(id: &str) -> Result<SessionEntity> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;

        // Fast path: SessionIndex → .jsonl
        if let Ok(index) = SessionIndex::open(&paths.db())
            && let Ok(Some(relative_path)) = index.find(id)
        {
            let file_path = paths.sessions().join(&relative_path);
            if file_path.exists() {
                return SessionEvent::read_meta(&file_path);
            }
        }

        // Fallback: brute-force date-dir scan for .jsonl
        let sessions_root = paths.sessions();
        let date_dirs_result: Result<Vec<PathBuf>> =
            Self::scan_date_dirs(&sessions_root, id, "jsonl");
        if let Ok(Some(session)) = date_dirs_result?
            .into_iter()
            .next()
            .map(|p| SessionEvent::read_meta(&p))
            .transpose()
        {
            return Ok(session);
        }

        // Legacy fallback: .json metadata file (pre-JSONL format)
        let date_dirs_result: Result<Vec<PathBuf>> =
            Self::scan_date_dirs(&sessions_root, id, "json");
        if let Ok(Some(session)) = date_dirs_result?
            .into_iter()
            .next()
            .map(|p| {
                let json = std::fs::read_to_string(&p)
                    .with_context(|| format!("failed to read session file: {}", p.display()))?;
                serde_json::from_str::<SessionEntity>(&json)
                    .with_context(|| format!("failed to parse session file: {}", p.display()))
            })
            .transpose()
        {
            return Ok(session);
        }

        let flat_path = sessions_root.join(format!("{}.jsonl", id));
        if flat_path.exists() {
            return SessionEvent::read_meta(&flat_path);
        }

        let flat_path = sessions_root.join(format!("{}.json", id));
        if flat_path.exists() {
            let json = std::fs::read_to_string(&flat_path)
                .with_context(|| format!("failed to read session file: {}", flat_path.display()))?;
            let session: SessionEntity = serde_json::from_str(&json).with_context(|| {
                format!("failed to parse session file: {}", flat_path.display())
            })?;
            return Ok(session);
        }

        anyhow::bail!("session not found: {id}")
    }

    fn scan_date_dirs(sessions_root: &PathBuf, id: &str, ext: &str) -> Result<Vec<PathBuf>> {
        let mut results = Vec::new();

        if !sessions_root.exists() {
            return Ok(results);
        }

        for year_entry in std::fs::read_dir(sessions_root).with_context(|| {
            format!(
                "failed to read sessions directory: {}",
                sessions_root.display()
            )
        })? {
            let year_entry = year_entry?;
            let year_path = year_entry.path();
            if !year_path.is_dir() {
                continue;
            }
            for month_entry in std::fs::read_dir(&year_path)? {
                let month_entry = month_entry?;
                let month_path = month_entry.path();
                if !month_path.is_dir() {
                    continue;
                }
                for day_entry in std::fs::read_dir(&month_path)? {
                    let day_entry = day_entry?;
                    let day_path = day_entry.path();
                    if !day_path.is_dir() {
                        continue;
                    }
                    let candidate = day_path.join(format!("{}.{}", id, ext));
                    if candidate.exists() {
                        results.push(candidate);
                    }
                }
            }
        }

        Ok(results)
    }

    pub fn list() -> Result<Vec<SessionEntity>> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;
        let sessions_root = paths.sessions();
        let mut sessions: Vec<SessionEntity> = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        if let Ok(index) = SessionIndex::open(&paths.db())
            && let Ok(indexed) = index.list_all()
        {
            for row in indexed {
                let file_path = sessions_root.join(&row.file_path);
                if file_path.exists() {
                    let session = match load_session_from_file(&file_path) {
                        Ok(s) => s,
                        Err(e) => {
                            debug!("skipping invalid session {}: {}", row.id, e);
                            continue;
                        }
                    };
                    seen_ids.insert(session.id.clone());
                    sessions.push(session);
                } else if let Some(repaired) =
                    Self::repair_indexed_session(&sessions_root, &row.id, &paths.db())
                {
                    seen_ids.insert(repaired.id.clone());
                    sessions.push(repaired);
                }
            }
        }

        Self::scan_filesystem_sessions(&sessions_root, &mut sessions, &mut seen_ids)?;

        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        debug!("listed {} sessions", sessions.len());
        Ok(sessions)
    }

    fn repair_indexed_session(
        sessions_root: &PathBuf,
        id: &str,
        db_dir: &PathBuf,
    ) -> Option<SessionEntity> {
        let result = Self::scan_date_dirs(sessions_root, id, "jsonl")
            .ok()
            .and_then(|paths| paths.into_iter().next());
        let found_path = result.or_else(|| {
            Self::scan_date_dirs(sessions_root, id, "json")
                .ok()
                .and_then(|paths| paths.into_iter().next())
        })?;
        let json = std::fs::read_to_string(&found_path).ok()?;
        let session = if found_path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            SessionEvent::read_meta(&found_path).ok()?
        } else {
            serde_json::from_str::<SessionEntity>(&json).ok()?
        };
        let relative = found_path
            .strip_prefix(sessions_root)
            .unwrap_or(&found_path)
            .to_string_lossy()
            .to_string();
        if let Ok(idx) = SessionIndex::open(db_dir) {
            let _ = idx.reconcile(&session.id, &relative);
        }
        Some(session)
    }

    fn scan_filesystem_sessions(
        sessions_root: &PathBuf,
        sessions: &mut Vec<SessionEntity>,
        seen_ids: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        Self::walk_sessions_dir(sessions_root, sessions, seen_ids)?;
        Ok(())
    }

    fn walk_sessions_dir(
        dir: &PathBuf,
        sessions: &mut Vec<SessionEntity>,
        seen_ids: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.is_dir() {
                Self::walk_sessions_dir(&path, sessions, seen_ids)?;
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str());
                if ext != Some("jsonl") && ext != Some("json") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if seen_ids.contains(stem) {
                        continue;
                    }
                    if let Ok(session) = load_session_from_file(&path) {
                        seen_ids.insert(session.id.clone());
                        sessions.push(session);
                    }
                }
            }
        }
        Ok(())
    }

    /// List only active sessions.
    pub fn list_active() -> Result<Vec<SessionEntity>> {
        Ok(Self::list()?
            .into_iter()
            .filter(|s| s.status == SessionStatus::Active)
            .collect())
    }

    /// Transition session to Archived state (terminal).
    pub fn archive(&mut self) -> Result<()> {
        self.status = SessionStatus::Archived;
        self.updated_at = Utc::now();
        self.save()?;
        info!(session_id = %self.id, "session archived");
        Ok(())
    }

    /// Transition session to Compacted state (context was truncated).
    pub fn compact(&mut self) -> Result<()> {
        self.status = SessionStatus::Compacted;
        self.updated_at = Utc::now();
        self.save()?;
        info!(session_id = %self.id, "session compacted");
        Ok(())
    }

    /// Reactivate a compacted session.
    pub fn reactivate(&mut self) -> Result<()> {
        if self.status == SessionStatus::Archived {
            anyhow::bail!("cannot reactivate archived session");
        }
        self.status = SessionStatus::Active;
        self.updated_at = Utc::now();
        self.save()?;
        info!(session_id = %self.id, "session reactivated");
        Ok(())
    }
}

/// Parse the creation datetime from a UUID v7 session ID string.
///
/// UUID v7 embeds a Unix millisecond timestamp in its first 48 bits.
/// This extracts it without needing the `SessionEntity.created_at` field.
///
/// Returns `None` if the string is not a valid UUID v7.
pub fn session_created_at_from_id(session_id: &str) -> Option<DateTime<Utc>> {
    let uuid = Uuid::parse_str(session_id).ok()?;
    let (secs, nanos) = uuid.get_timestamp()?.to_unix();
    DateTime::from_timestamp(secs as i64, nanos)
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
                if p > 0.0 { -p * p.log2() } else { 0.0 }
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
/// - zen-vault: external memory (WHAT do I know about the world?)
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
    /// Retrieved knowledge from zen-vault (search results, wiki pages)
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
        tracing::info!(
            session_id = %self.session_id,
            role = role,
            content_len = content.len(),
            conversation_turns_before = self.conversation.len(),
            "SessionContext::add_turn: adding conversation turn"
        );
        self.conversation.push(ConversationTurn {
            role: role.to_string(),
            content: content.to_string(),
        });
        tracing::info!(
            session_id = %self.session_id,
            conversation_turns_after = self.conversation.len(),
            "SessionContext::add_turn: turn added"
        );
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
        let task = Task::new(
            "fn add(a: i32, b: i32) -> i32 { a + b }",
            0.2,
            TaskType::Code,
        );
        assert!(matches!(task.complexity, ComplexityLevel::Simple));
    }

    #[test]
    fn test_task_classification_code_medium_entropy() {
        let task = Task::new(
            "implement a complex async state machine",
            0.5,
            TaskType::Code,
        );
        assert!(matches!(task.complexity, ComplexityLevel::Standard));
    }

    #[test]
    fn test_task_classification_text_high_entropy() {
        let task = Task::new(
            "analyze philosophical implications of consciousness",
            0.8,
            TaskType::Text,
        );
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

    #[test]
    fn test_session_entity_new_has_correct_defaults() {
        let session = SessionEntity::new("test-agent", "/workspace");
        assert_eq!(session.agent_name, "test-agent");
        assert_eq!(session.workspace, "/workspace");
        assert_eq!(session.sensitivity_policy, Sensitivity::Private);
        assert_eq!(session.status, SessionStatus::Active);
        assert!(!session.id.is_empty());
    }

    #[test]
    fn test_session_entity_serialization_roundtrip() {
        let session = SessionEntity::new("Sisyphus-Junior", "/tmp");
        let json = serde_json::to_string(&session).unwrap();
        let loaded: SessionEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.agent_name, "Sisyphus-Junior");
        assert_eq!(loaded.workspace, "/tmp");
        assert_eq!(loaded.status, SessionStatus::Active);
    }

    #[test]
    fn test_session_state_transitions() {
        let mut session = SessionEntity::new("test", "/workspace");
        assert_eq!(session.status, SessionStatus::Active);

        session.compact().unwrap();
        assert_eq!(session.status, SessionStatus::Compacted);

        session.reactivate().unwrap();
        assert_eq!(session.status, SessionStatus::Active);

        session.archive().unwrap();
        assert_eq!(session.status, SessionStatus::Archived);

        // Archived cannot be reactivated
        assert!(session.reactivate().is_err());
    }

    /// Returns the global temp dir used by ALL session store tests.
    /// SAFETY: Sets ZEN_HOME once per test process via atomic flag.
    fn session_test_dir() -> &'static std::path::Path {
        use std::sync::OnceLock;
        static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
        let tmp = DIR.get_or_init(|| {
            let t = tempfile::TempDir::new().expect("failed to create temp dir for session tests");
            std::fs::create_dir_all(t.path().join("data")).unwrap();
            std::fs::create_dir_all(t.path().join("sessions")).unwrap();
            // SAFETY: test environment setup, set once per process
            unsafe {
                std::env::set_var("ZEN_HOME", t.path());
            }
            t
        });
        tmp.path()
    }

    #[test]
    fn test_session_save_to_date_path() {
        let root = session_test_dir();
        let session = SessionEntity::new("agent-x", "/ws");
        let path = session.save().unwrap();

        let year = session.created_at.format("%Y").to_string();
        let month = session.created_at.format("%m").to_string();
        let day = session.created_at.format("%d").to_string();
        let expected = root
            .join("sessions")
            .join(&year)
            .join(&month)
            .join(&day)
            .join(format!("{}.jsonl", session.id));

        assert_eq!(path, expected);
        assert!(expected.exists());
    }

    #[test]
    fn test_session_load_roundtrip() {
        session_test_dir();
        let session = SessionEntity::new("agent-y", "/ws2");
        session.save().unwrap();

        let loaded = SessionEntity::load(&session.id).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.agent_name, "agent-y");
        assert_eq!(loaded.workspace, "/ws2");
    }

    #[test]
    fn test_session_list_with_both_layouts() {
        let root = session_test_dir();
        let sessions_root = root.join("sessions");

        // Legacy flat .json file
        let flat_session = SessionEntity::new("flat-agent", "/flat-ws");
        let flat_path = sessions_root.join(format!("{}.json", flat_session.id));
        std::fs::write(
            &flat_path,
            serde_json::to_string_pretty(&flat_session).unwrap(),
        )
        .unwrap();

        // New .jsonl file via save()
        let date_session = SessionEntity::new("date-agent", "/date-ws");
        date_session.save().unwrap();

        let all = SessionEntity::list().unwrap();
        let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        assert!(
            ids.contains(&flat_session.id.as_str()),
            "flat .json session should be in list"
        );
        assert!(
            ids.contains(&date_session.id.as_str()),
            ".jsonl session should be in list"
        );
    }

    #[test]
    fn test_session_sqlite_index_roundtrip() {
        let root = session_test_dir();
        let db_dir = root.join("data");
        let index = crate::session_index::SessionIndex::open(&db_dir).unwrap();

        let prefix = format!("sqlite-{}", Uuid::now_v7());
        for i in 0..3 {
            index
                .upsert(
                    &format!("{}-{}", prefix, i),
                    &format!("2025/06/{:02}/{}-{}.jsonl", 15 + i, prefix, i),
                    &format!("agent-{}", i),
                    "Active",
                    &format!("2025-06-1{}T10:00:00Z", 5 + i),
                    &format!("2025-06-1{}T12:00:00Z", 5 + i),
                    "/ws",
                )
                .unwrap();
        }

        let found = index.find(&format!("{}-1", prefix)).unwrap();
        let expected_path = format!("2025/06/16/{}-1.jsonl", prefix);
        assert_eq!(found.as_deref(), Some(expected_path.as_str()));

        let missing = index.find(&format!("{}-no-such", prefix)).unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_session_load_legacy_flat() {
        let root = session_test_dir();
        let sessions_root = root.join("sessions");

        let mut session = SessionEntity::new("legacy-agent", "/legacy-ws");
        session.title = Some("Legacy Session".to_string());
        let flat_path = sessions_root.join(format!("{}.json", session.id));
        std::fs::write(&flat_path, serde_json::to_string_pretty(&session).unwrap()).unwrap();

        let loaded = SessionEntity::load(&session.id).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.agent_name, "legacy-agent");
        assert_eq!(loaded.title.as_deref(), Some("Legacy Session"));
    }

    #[test]
    fn test_session_read_repair() {
        let root = session_test_dir();
        let db_dir = root.join("data");

        let session = SessionEntity::new("repair-agent", "/repair-ws");
        session.save().unwrap();

        let index = crate::session_index::SessionIndex::open(&db_dir).unwrap();
        index.reconcile(&session.id, "wrong/path.jsonl").unwrap();

        let all = SessionEntity::list().unwrap();
        assert!(
            all.iter().any(|s| s.id == session.id),
            "repaired session should be in list"
        );
    }
}
