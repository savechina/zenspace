//! Agent identity file management.
//!
//! Manages `.agent-identity.md` files stored in `~/.zen/identity/` that track
//! agent profiles including role, capabilities, and session history.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use zen_core::paths::ZenPaths;

// ─── Data types ────────────────────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub summary: Option<String>,
}

/// Agent identity file metadata (serialized to YAML frontmatter).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityFile {
    /// Agent name (used as the filename stem, e.g. `.zen/identity/researcher.md`).
    pub agent_name: String,
    /// Role description: what kind of agent this is.
    pub role: String,
    /// List of tool/capability names this agent has access to.
    pub capabilities: Vec<String>,
    /// When this identity was first created.
    pub created_at: DateTime<Utc>,
    /// Timestamp of the last recorded session.
    pub last_session: Option<DateTime<Utc>>,
    /// Total number of sessions recorded.
    pub session_count: u64,
    /// Ordered list of session entries (newest last).
    #[serde(default)]
    pub session_history: Vec<SessionEntry>,
}

impl Default for IdentityFile {
    fn default() -> Self {
        Self {
            agent_name: "default".to_string(),
            role: "general assistant".to_string(),
            capabilities: Vec::new(),
            created_at: Utc::now(),
            last_session: None,
            session_count: 0,
            session_history: Vec::new(),
        }
    }
}

// ─── Error type ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("identity file not found: {path}")]
    NotFound { path: PathBuf },

    #[error("failed to parse identity file: {path}: {reason}")]
    ParseError { path: PathBuf, reason: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("internal error: {0}")]
    Internal(String),
}

impl IdentityError {
    pub fn category(&self) -> &str {
        match self {
            IdentityError::NotFound { .. } => "not_found",
            IdentityError::ParseError { .. } => "parse_error",
            IdentityError::Io(_) => "io_error",
            IdentityError::Internal(_) => "internal",
        }
    }
}

// ─── Trait ─────────────────────────────────────────────────────────────

/// Trait for loading and saving agent identity files.
///
/// Implementations are responsible for the file format on disk;
/// `IdentityFileManager` is the standard markdown-based implementation.
pub trait IdentityManager: Send + Sync {
    /// Load an agent's identity from its workspace directory, or return `None`
    /// when no `.agent-identity.md` file exists.
    fn load(&self, workspace: &Path) -> Result<Option<IdentityFile>, IdentityError>;

    /// Save the identity file to the workspace's identity directory.
    fn save(&self, workspace: &Path, identity: &IdentityFile) -> Result<(), IdentityError>;

    /// Create a default identity for the given role with no capabilities.
    fn create_default(role: &str) -> IdentityFile {
        IdentityFile {
            agent_name: "default".to_string(),
            role: role.to_string(),
            capabilities: Vec::new(),
            created_at: Utc::now(),
            last_session: None,
            session_count: 0,
            session_history: Vec::new(),
        }
    }

    /// Append a new session entry to the identity and bump `session_count`.
    fn record_session(identity: &mut IdentityFile, entry: SessionEntry) {
        identity.last_session = Some(entry.timestamp);
        identity.session_count += 1;
        identity.session_history.push(entry);
    }
}

// ─── Default implementation ────────────────────────────────────────────

/// Reads and writes identity files in markdown format with a YAML-like
/// frontmatter block. File is stored as `~/.zen/identity/<name>.md`.
pub struct IdentityFileManager;

impl IdentityFileManager {
    /// Create a default identity for the given role with no capabilities.
    fn create_default_identity(role: &str) -> IdentityFile {
        IdentityFile {
            agent_name: "default".to_string(),
            role: role.to_string(),
            capabilities: Vec::new(),
            created_at: Utc::now(),
            last_session: None,
            session_count: 0,
            session_history: Vec::new(),
        }
    }

    /// Append a new session entry to the identity and bump `session_count`.
    fn record_session_entry(identity: &mut IdentityFile, entry: SessionEntry) {
        identity.last_session = Some(entry.timestamp);
        identity.session_count += 1;
        identity.session_history.push(entry);
    }

    /// Return the identity directory for a given workspace.
    ///
    /// `~/.zen/identity/`
    fn identity_dir(zen_paths: &ZenPaths) -> PathBuf {
        zen_paths.global_root().join("identity")
    }

    /// Compute the file path for a given agent name.
    fn identity_path(zen_paths: &ZenPaths, agent_name: &str) -> PathBuf {
        Self::identity_dir(zen_paths).join(format!("{agent_name}.md"))
    }
}

impl IdentityManager for IdentityFileManager {
    fn load(&self, workspace: &Path) -> Result<Option<IdentityFile>, IdentityError> {
        let content = match fs::read_to_string(workspace) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(IdentityError::Io(e)),
        };

        let identity = parse_identity(workspace, &content)?;
        debug!(
            "loaded identity for agent '{}' with {} sessions",
            identity.agent_name, identity.session_count
        );
        Ok(Some(identity))
    }

    fn save(&self, workspace: &Path, identity: &IdentityFile) -> Result<(), IdentityError> {
        let content = generate_identity_md(identity);
        fs::write(workspace, content).map_err(IdentityError::Io)?;
        info!(
            "saved identity for agent '{}' ({} sessions)",
            identity.agent_name, identity.session_count
        );
        Ok(())
    }
}

/// High-level helpers that resolve `.zen/identity/<name>.md` from ZenPaths.
impl IdentityFileManager {
    /// Load an agent identity by name, returning `None` if the file is missing.
    pub fn load_by_name(
        &self,
        zen_paths: &ZenPaths,
        agent_name: &str,
    ) -> Result<Option<IdentityFile>, IdentityError> {
        let path = Self::identity_path(zen_paths, agent_name);
        self.load(&path)
    }

    /// Save an agent identity by resolving its name to `.zen/identity/<name>.md`.
    pub fn save_by_name(
        &self,
        zen_paths: &ZenPaths,
        identity: &IdentityFile,
    ) -> Result<(), IdentityError> {
        let dir = Self::identity_dir(zen_paths);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create identity dir: {}", dir.display()))
            .map_err(|e| IdentityError::Internal(e.to_string()))?;

        let path = Self::identity_path(zen_paths, &identity.agent_name);
        self.save(&path, identity)
    }

    /// Record a session for a named agent, loading existing identity (or
    /// creating a default) before appending the entry.
    pub fn record_session_for_agent(
        &self,
        zen_paths: &ZenPaths,
        agent_name: &str,
        session_id: &str,
        summary: Option<&str>,
    ) -> Result<IdentityFile, IdentityError> {
        let mut identity = match self.load_by_name(zen_paths, agent_name)? {
            Some(id) => id,
            None => Self::create_default_identity("general assistant"),
        };
        identity.agent_name = agent_name.to_string();

        let entry = SessionEntry {
            session_id: session_id.to_string(),
            timestamp: Utc::now(),
            summary: summary.map(str::to_string),
        };
        Self::record_session_entry(&mut identity, entry);
        self.save_by_name(zen_paths, &identity)?;
        Ok(identity)
    }

    /// List all agent identity names stored in the identity directory.
    pub fn list_agents(zen_paths: &ZenPaths) -> Result<Vec<String>, IdentityError> {
        let dir = Self::identity_dir(zen_paths);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(&dir)
            .with_context(|| format!("failed to read identity directory: {}", dir.display()))
            .map_err(|e| IdentityError::Internal(e.to_string()))?;

        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            })
            .collect();

        names.sort();
        Ok(names)
    }
}

// ─── File format parsing / generation ──────────────────────────────────

/// Parse an agent identity from markdown content with YAML-like frontmatter.
///
/// Supported format:
/// ```markdown
/// ---
/// agent_name: "default"
/// role: "general assistant"
/// capabilities: ["search", "note"]
/// created_at: 2026-05-23T10:00:00Z
/// last_session: 2026-05-23T14:30:00Z
/// session_count: 1
/// ---
/// ```
fn parse_identity(path: &Path, content: &str) -> Result<IdentityFile, IdentityError> {
    if let Ok(identity) = parse_yaml_frontmatter(path, content) {
        return Ok(identity);
    }

    parse_kv_frontmatter(path, content)
}

/// Parse YAML frontmatter (between --- delimiters).
fn parse_yaml_frontmatter(path: &Path, content: &str) -> Result<IdentityFile, IdentityError> {
    let trimmed = content.trim();

    let (yaml_block, _) = if trimmed.starts_with("---") {
        let second_delim = trimmed
            .get(3..)
            .and_then(|s| s.find("---"))
            .ok_or_else(|| IdentityError::ParseError {
                path: path.to_path_buf(),
                reason: "missing closing --- delimiter".into(),
            })?;
        let yaml = trimmed.get(3..3 + second_delim).unwrap_or("").trim();
        let body = trimmed.get(3 + second_delim + 3..).unwrap_or("");
        (yaml, body.trim())
    } else {
        return Err(IdentityError::ParseError {
            path: path.to_path_buf(),
            reason: "no YAML frontmatter found".into(),
        });
    };

    let fields = parse_yaml_block(yaml_block)?;

    let agent_name = extract_string(&fields, "agent_name").unwrap_or_else(|| "default".to_string());
    let role = extract_string(&fields, "role").unwrap_or_else(|| "general assistant".to_string());

    let capabilities = extract_string_array(&fields, "capabilities");

    let created_at = extract_datetime(&fields, "created_at").unwrap_or_else(Utc::now);
    let last_session = extract_datetime(&fields, "last_session");
    let session_count = extract_u64(&fields, "session_count").unwrap_or(0);

    Ok(IdentityFile {
        agent_name,
        role,
        capabilities,
        created_at,
        last_session,
        session_count,
        session_history: Vec::new(),
    })
}

/// Parse key: value lines from content without YAML delimiters.
fn parse_kv_frontmatter(path: &Path, content: &str) -> Result<IdentityFile, IdentityError> {
    let mut fields: HashMap<String, String> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim();
            if field_name_is_valid(&key) {
                fields.insert(key, value.to_string());
            }
        }
    }

    if fields.is_empty() {
        return Err(IdentityError::ParseError {
            path: path.to_path_buf(),
            reason: "no identity metadata found".into(),
        });
    }

    let agent_name = extract_string(&fields, "agent_name").unwrap_or_else(|| "default".to_string());
    let role = extract_string(&fields, "role").unwrap_or_else(|| "general assistant".to_string());

    Ok(IdentityFile {
        agent_name,
        role,
        capabilities: Vec::new(),
        created_at: extract_datetime(&fields, "created_at").unwrap_or_else(Utc::now),
        last_session: extract_datetime(&fields, "last_session"),
        session_count: extract_u64(&fields, "session_count").unwrap_or(0),
        session_history: Vec::new(),
    })
}

/// Check if a key looks like identity metadata.
fn field_name_is_valid(name: &str) -> bool {
    matches!(
        name,
        "agent_name" | "role" | "capabilities" | "created_at" | "last_session" | "session_count"
    )
}

/// Parse a simple YAML block into a HashMap (handles strings, arrays, and dates).
fn parse_yaml_block(block: &str) -> Result<HashMap<String, String>, IdentityError> {
    let mut map = HashMap::new();

    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim();
            map.insert(key, value.to_string());
        }
    }

    Ok(map)
}

/// Extract a trimmed string value from the parsed fields.
fn extract_string(fields: &HashMap<String, String>, key: &str) -> Option<String> {
    fields
        .get(key)
        .map(|v| v.trim_matches('"').trim_matches('\'').trim().to_string())
}

/// Extract a string array from fields (handles `[a, b, c]` format).
fn extract_string_array(fields: &HashMap<String, String>, key: &str) -> Vec<String> {
    let raw = match fields.get(key) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let inner = raw.strip_prefix('[').unwrap_or(raw);
    let inner = inner.strip_suffix(']').unwrap_or(inner);

    if inner.is_empty() {
        return Vec::new();
    }

    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract a DateTime<Utc> from fields.
fn extract_datetime(fields: &HashMap<String, String>, key: &str) -> Option<DateTime<Utc>> {
    let raw = fields.get(key)?;
    let raw = raw.trim();

    if let Ok(dt) = raw.parse::<DateTime<Utc>>() {
        return Some(dt);
    }

    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Some(date.and_hms_opt(0, 0, 0).unwrap().and_utc());
    }

    None
}

/// Extract a u64 from fields.
fn extract_u64(fields: &HashMap<String, String>, key: &str) -> Option<u64> {
    fields.get(key).and_then(|v| v.parse().ok())
}

/// Generate the markdown representation of an IdentityFile.
fn generate_identity_md(identity: &IdentityFile) -> String {
    let capabilities_json: Vec<String> = identity
        .capabilities
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect();
    let capabilities_str = format!("[{}]", capabilities_json.join(", "));

    let last_session_str = identity
        .last_session
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    let mut output = String::new();
    output.push_str("---\n");
    output.push_str(&format!("agent_name: \"{}\"\n", identity.agent_name));
    output.push_str(&format!("role: \"{}\"\n", identity.role));
    output.push_str(&format!("capabilities: {}\n", capabilities_str));
    output.push_str(&format!(
        "created_at: {}\n",
        identity.created_at.to_rfc3339()
    ));
    output.push_str(&format!("last_session: {last_session_str}\n"));
    output.push_str(&format!("session_count: {}\n", identity.session_count));
    output.push_str("---\n");

    if !identity.session_history.is_empty() {
        output.push_str("\n## Session History\n\n");
        for entry in &identity.session_history {
            let summary = entry.summary.as_deref().unwrap_or("");
            output.push_str(&format!(
                "- **{}** | `{}` | {summary}\n",
                entry.timestamp.to_rfc3339(),
                entry.session_id
            ));
        }
        output.push('\n');
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_minimal_frontmatter() {
        let content = r#"---
agent_name: "researcher"
role: "deep research"
capabilities: ["search", "research"]
created_at: 2026-05-23T10:00:00Z
session_count: 0
---
"#;
        let path = PathBuf::from("test.md");
        let identity = parse_identity(&path, content).unwrap();
        assert_eq!(identity.agent_name, "researcher");
        assert_eq!(identity.role, "deep research");
        assert_eq!(identity.capabilities, vec!["search", "research"]);
        assert_eq!(identity.session_count, 0);
    }

    #[test]
    fn test_parse_with_session_history() {
        let content = r#"---
agent_name: "coder"
role: "coding assistant"
capabilities: ["note", "search"]
created_at: 2026-05-20T08:00:00Z
last_session: 2026-05-23T14:30:00Z
session_count: 3
---

## Session History

- **2026-05-20T08:00:00Z** | `sess-001` | Initial setup
"#;
        let path = PathBuf::from("test.md");
        let identity = parse_identity(&path, content).unwrap();
        assert_eq!(identity.agent_name, "coder");
        assert_eq!(identity.session_count, 3);
        assert!(identity.last_session.is_some());
    }

    #[test]
    fn test_generate_minimal_identity() {
        let identity = IdentityFile {
            agent_name: "default".to_string(),
            role: "general assistant".to_string(),
            capabilities: vec!["search".to_string()],
            created_at: DateTime::parse_from_rfc3339("2026-05-23T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            last_session: None,
            session_count: 0,
            session_history: Vec::new(),
        };

        let generated = generate_identity_md(&identity);
        assert!(generated.contains("agent_name: \"default\""));
        assert!(generated.contains("role: \"general assistant\""));
        assert!(generated.contains("capabilities: [\"search\"]"));
        assert!(generated.contains("session_count: 0"));
    }

    #[test]
    fn test_generate_with_sessions() {
        let identity = IdentityFile {
            agent_name: "coder".to_string(),
            role: "coding assistant".to_string(),
            capabilities: Vec::new(),
            created_at: DateTime::parse_from_rfc3339("2026-05-20T08:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            last_session: Some(
                DateTime::parse_from_rfc3339("2026-05-23T14:30:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            session_count: 2,
            session_history: vec![
                SessionEntry {
                    session_id: "abc-001".to_string(),
                    timestamp: DateTime::parse_from_rfc3339("2026-05-20T08:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    summary: Some("Initial session".to_string()),
                },
                SessionEntry {
                    session_id: "abc-002".to_string(),
                    timestamp: DateTime::parse_from_rfc3339("2026-05-23T14:30:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    summary: Some("Fixing bugs".to_string()),
                },
            ],
        };

        let generated = generate_identity_md(&identity);
        assert!(generated.contains("session_count: 2"));
        assert!(generated.contains("abc-001"));
        assert!(generated.contains("abc-002"));
        assert!(generated.contains("Initial session"));

        let path = PathBuf::from("test.md");
        let reparsed = parse_identity(&path, &generated).unwrap();
        assert_eq!(reparsed.agent_name, "coder");
        assert_eq!(reparsed.session_count, 2);
    }

    #[test]
    fn test_record_session() {
        let mut identity = IdentityFileManager::create_default_identity("assistant");
        assert_eq!(identity.session_count, 0);
        assert!(identity.last_session.is_none());

        let entry = SessionEntry {
            session_id: "sess-001".to_string(),
            timestamp: Utc::now(),
            summary: Some("test".to_string()),
        };
        IdentityFileManager::record_session_entry(&mut identity, entry);
        assert_eq!(identity.session_count, 1);
        assert!(identity.last_session.is_some());
        assert_eq!(identity.session_history.len(), 1);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("test-agent.md");

        let identity = IdentityFile {
            agent_name: "tester".to_string(),
            role: "QA agent".to_string(),
            capabilities: vec!["test".to_string(), "debug".to_string()],
            created_at: Utc::now(),
            last_session: None,
            session_count: 0,
            session_history: Vec::new(),
        };

        let manager = IdentityFileManager;
        manager.save(&path, &identity).unwrap();

        let loaded = manager.load(&path).unwrap().unwrap();
        assert_eq!(loaded.agent_name, "tester");
        assert_eq!(loaded.role, "QA agent");
        assert_eq!(loaded.capabilities, vec!["test", "debug"]);
    }

    #[test]
    fn test_load_nonexistent_returns_none() {
        let path = PathBuf::from("/nonexistent/path/agent.md");
        let manager = IdentityFileManager;
        let result = manager.load(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_create_default() {
        let identity = IdentityFileManager::create_default_identity("researcher");
        assert_eq!(identity.role, "researcher");
        assert_eq!(identity.agent_name, "default");
        assert!(identity.capabilities.is_empty());
        assert_eq!(identity.session_count, 0);
    }
}
