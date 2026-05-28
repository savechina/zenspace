use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;

// ── Domain ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Domain {
    Work,
    Personal,
    Learning,
    Finance,
    Health,
}

impl Domain {
    fn as_str(&self) -> &str {
        match self {
            Domain::Work => "work",
            Domain::Personal => "personal",
            Domain::Learning => "learning",
            Domain::Finance => "finance",
            Domain::Health => "health",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "work" => Some(Domain::Work),
            "personal" => Some(Domain::Personal),
            "learning" => Some(Domain::Learning),
            "finance" => Some(Domain::Finance),
            "health" => Some(Domain::Health),
            _ => None,
        }
    }
}

impl Serialize for Domain {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Domain {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid domain: {}", s)))
    }
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ── Note ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub tags: Vec<String>,
    pub source: String,
    pub source_id: Option<String>,
    pub sensitivity: Sensitivity,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub domain: Vec<Domain>,
    pub project: Option<String>,
    pub content: String,
    pub file_path: Option<PathBuf>,
}

impl Default for Note {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            id: String::new(),
            tags: Vec::new(),
            source: String::new(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: now,
            updated_at: now,
            domain: Vec::new(),
            project: None,
            content: String::new(),
            file_path: None,
        }
    }
}

// ── Front matter parsing helpers ────────────────────────────────────

/// Extract YAML front matter between triple-dash delimiters.
/// Returns (yaml_body, content_after_dashes) or an error.
fn extract_front_matter(content: &str) -> Result<(&str, &str), anyhow::Error> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return Err(anyhow::anyhow!("missing leading --- delimiter"));
    }

    // Find the second --- that closes the front matter
    let rest = &content[3..];
    let end_pos = rest
        .find("---")
        .ok_or_else(|| anyhow::anyhow!("missing closing --- delimiter"))?;

    let yaml_body = &rest[..end_pos];
    let after = &rest[end_pos + 3..];

    Ok((yaml_body, after))
}

/// Parse a single front-matter key = value, returning owned String for each.
/// Handles arrays like `[a, b, c]` by keeping the brackets; everything else
/// is treated as a raw value (optionally quoted).
fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let eq_pos = line.find(':')?;
    let key = line[..eq_pos].trim().to_string();
    let raw = line[eq_pos + 1..].trim();

    // Remove optional surrounding quotes for scalars
    let value = if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        raw[1..raw.len() - 1].to_string()
    } else {
        raw.to_string()
    };

    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Parse one or more comma-separated values inside brackets, e.g.
/// `[personal]` → `["personal"]`, `[work, learning]` → `["work", "learning"]`.
fn parse_array(value: &str) -> Vec<String> {
    let value = value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return vec![value.to_string()];
    }
    let inner = &value[1..value.len() - 1];
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|s| {
            s.trim()
                .trim_matches(|c: char| c == '"' || c == '\'')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_sensitivity(value: &str) -> Option<Sensitivity> {
    match value.to_lowercase().as_str() {
        "public" => Some(Sensitivity::Public),
        "private" => Some(Sensitivity::Private),
        "confidential" => Some(Sensitivity::Confidential),
        _ => None,
    }
}

fn parse_domain(value: &str) -> Option<Domain> {
    Domain::from_str(value)
}

fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    // chrono handles ISO 8601 with offset
    value.parse::<DateTime<Utc>>().ok()
}

// ── Public API ──────────────────────────────────────────────────────

/// Parse a markdown document with YAML front matter into a [`Note`].
pub fn parse_frontmatter(content: &str) -> Result<Note, anyhow::Error> {
    let (yaml_body, body_content) = extract_front_matter(content)?;

    let mut note = Note::default();

    for line in yaml_body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = parse_kv_line(line)
            .ok_or_else(|| anyhow::anyhow!("invalid front-matter line: {}", line))?;

        match key.as_str() {
            "id" => note.id = value,
            "source" => note.source = value,
            "source_id" if !value.is_empty() && value != "null" && value != "~" => {
                note.source_id = Some(value);
            },
            "project" if !value.is_empty() && value != "null" && value != "~" => {
                note.project = Some(value);
            },
            "tags" => note.tags = parse_array(&value),
            "sensitivity" => {
                note.sensitivity = parse_sensitivity(&value)
                    .ok_or_else(|| anyhow::anyhow!("invalid sensitivity: {}", value))?;
            },
            "domain" => {
                for item in parse_array(&value) {
                    note.domain.push(
                        parse_domain(&item)
                            .ok_or_else(|| anyhow::anyhow!("invalid domain: {}", item))?,
                    );
                }
            },
            "created_at" => {
                note.created_at = parse_datetime(&value)
                    .ok_or_else(|| anyhow::anyhow!("invalid created_at: {}", value))?;
            },
            "updated_at" => {
                note.updated_at = parse_datetime(&value)
                    .ok_or_else(|| anyhow::anyhow!("invalid updated_at: {}", value))?;
            },
            _ => {
                // Ignore unknown fields
            },
        }
    }

    note.content = body_content.trim_start().to_string();

    Ok(note)
}

/// Write a [`Note`] to a markdown file under `{base_dir}/inbox/`,
/// creating the directory if missing.
///
/// Filename format: `{created_at:%Y-%m-%d-%H%M%S}.md`
pub fn write_note(note: &Note, base_dir: &Path) -> Result<PathBuf, anyhow::Error> {
    let inbox_dir = base_dir.join("inbox");
    std::fs::create_dir_all(&inbox_dir)?;

    let filename = note.created_at.format("%Y-%m-%d-%H%M%S.md").to_string();
    let file_path = inbox_dir.join(filename);

    // Build front matter manually to keep full control over formatting
    let mut front_matter = String::new();
    front_matter.push_str(&format!("id: \"{}\"\n", note.id));

    // Tags (inline array)
    if note.tags.is_empty() {
        front_matter.push_str("tags: []\n");
    } else {
        let joined = note
            .tags
            .iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", ");
        front_matter.push_str(&format!("tags: [{}]\n", joined));
    }

    front_matter.push_str(&format!("source: \"{}\"\n", note.source));

    if let Some(ref source_id) = note.source_id {
        front_matter.push_str(&format!("source_id: \"{}\"\n", source_id));
    } else {
        front_matter.push_str("source_id: null\n");
    }

    // Sensitivity — lowercase to match convention
    let sens_str = match note.sensitivity {
        Sensitivity::Public => "public",
        Sensitivity::Private => "private",
        Sensitivity::Confidential => "confidential",
    };
    front_matter.push_str(&format!("sensitivity: {}\n", sens_str));

    front_matter.push_str(&format!(
        "created_at: \"{}\"\n",
        note.created_at.to_rfc3339()
    ));
    front_matter.push_str(&format!(
        "updated_at: \"{}\"\n",
        note.updated_at.to_rfc3339()
    ));

    // Domain (inline array)
    if note.domain.is_empty() {
        front_matter.push_str("domain: []\n");
    } else {
        let joined = note
            .domain
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        front_matter.push_str(&format!("domain: [{}]\n", joined));
    }

    if let Some(ref project) = note.project {
        front_matter.push_str(&format!("project: \"{}\"\n", project));
    } else {
        front_matter.push_str("project: null\n");
    }

    let full_content = format!("---\n{}---\n\n{}", front_matter, note.content);

    std::fs::write(&file_path, full_content)?;

    Ok(file_path)
}

// ── Note Service ──────────────────────────────────────────────────────

/// Service for creating and managing notes.
pub struct NoteService;

impl NoteService {
    /// Create a new [`NoteService`].
    pub fn new() -> Self {
        Self
    }

    /// Create a new note with the given content, tags, and source.
    ///
    /// Writes the note to `~/.zen/knowledge/inbox/` and returns the created [`Note`].
    pub fn create_note(
        &self,
        content: &str,
        tags: Vec<String>,
        source: &str,
    ) -> Result<Note, anyhow::Error> {
        let now = Utc::now();
        let note = Note {
            id: uuid::Uuid::now_v7().to_string(),
            tags,
            source: source.to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: now,
            updated_at: now,
            domain: Vec::new(),
            project: None,
            content: content.to_string(),
            file_path: None,
        };

        let paths = ZenPaths::detect()?;
        let knowledge_dir = paths.user_data("knowledge");
        let file_path = write_note(&note, &knowledge_dir)?;

        let mut note = note;
        note.file_path = Some(file_path.clone());

        tracing::info!("note created: id={} path={}", note.id, file_path.display());

        Ok(note)
    }
}

impl Default for NoteService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_note() {
        let note = Note::default();
        assert!(note.tags.is_empty());
        assert!(note.domain.is_empty());
        assert!(note.source_id.is_none());
        assert!(note.project.is_none());
        assert_eq!(note.sensitivity, Sensitivity::Private);
    }

    #[test]
    fn test_domain_roundtrip() {
        for d in [
            Domain::Work,
            Domain::Personal,
            Domain::Learning,
            Domain::Finance,
            Domain::Health,
        ] {
            let s = d.to_string();
            let parsed = Domain::from_str(&s).unwrap();
            assert_eq!(d, parsed);
        }
    }

    #[test]
    fn test_parse_full_frontmatter() {
        let input = r#"---
id: "0193b8f2-7a1e-7c4a-b5d6-8f0e3a2c1b4d"
tags: [reminder, meeting]
source: "qq_private"
sensitivity: private
created_at: "2026-05-23T15:00:00+08:00"
updated_at: "2026-05-23T15:00:00+08:00"
domain: [personal]
project: "my-project"
---

This is the note content.
"#;

        let note = parse_frontmatter(input).unwrap();
        assert_eq!(note.id, "0193b8f2-7a1e-7c4a-b5d6-8f0e3a2c1b4d");
        assert_eq!(note.tags, vec!["reminder", "meeting"]);
        assert_eq!(note.source, "qq_private");
        assert_eq!(note.sensitivity, Sensitivity::Private);
        assert_eq!(note.domain, vec![Domain::Personal]);
        assert_eq!(note.project, Some("my-project".to_string()));
        assert_eq!(note.content, "This is the note content.\n");
    }

    #[test]
    fn test_missing_sensitivity_defaults_private() {
        let input = r#"---
id: "abc"
source: "cli"
created_at: "2026-01-01T00:00:00+00:00"
updated_at: "2026-01-01T00:00:00+00:00"
---

Body
"#;

        let note = parse_frontmatter(input).unwrap();
        assert_eq!(note.sensitivity, Sensitivity::Private);
    }

    #[test]
    fn test_missing_tags_defaults_empty() {
        let input = r#"---
id: "abc"
source: "cli"
created_at: "2026-01-01T00:00:00+00:00"
updated_at: "2026-01-01T00:00:00+00:00"
---

Body
"#;

        let note = parse_frontmatter(input).unwrap();
        assert!(note.tags.is_empty());
    }

    #[test]
    fn test_roundtrip() {
        let note = Note {
            id: "0193b8f2-7a1e-7c4a-b5d6-8f0e3a2c1b4d".to_string(),
            tags: vec!["reminder".to_string()],
            source: "qq_private".to_string(),
            source_id: Some("msg123".to_string()),
            sensitivity: Sensitivity::Private,
            created_at: "2026-05-23T15:00:00+08:00".parse().unwrap(),
            updated_at: "2026-05-23T15:00:00+08:00".parse().unwrap(),
            domain: vec![Domain::Personal],
            project: Some("proj".to_string()),
            content: "Hello world".to_string(),
            file_path: None,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = write_note(&note, dir.path()).unwrap();

        assert!(path.exists());
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed = parse_frontmatter(&raw).unwrap();

        assert_eq!(parsed.id, note.id);
        assert_eq!(parsed.tags, note.tags);
        assert_eq!(parsed.source, note.source);
        assert_eq!(parsed.sensitivity, note.sensitivity);
        assert_eq!(parsed.domain, note.domain);
        assert_eq!(parsed.content, note.content);
    }

    #[test]
    fn test_inbox_directory_created() {
        let note = Note::default();
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        assert!(!inbox.exists());

        write_note(&note, dir.path()).unwrap();
        assert!(inbox.exists());
    }

    #[test]
    fn test_create_note_populates_file_path() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: Single-threaded test, no concurrent env access
        unsafe { std::env::set_var("ZEN_ROOT_DIR", dir.path()) };

        let service = NoteService::new();
        let note = service
            .create_note("test content", vec!["tag1".to_string()], "test")
            .unwrap();

        // SAFETY: Single-threaded test, no concurrent env access
        unsafe { std::env::remove_var("ZEN_ROOT_DIR") };

        assert!(
            note.file_path.is_some(),
            "file_path should be set after write"
        );
        assert_eq!(note.file_path.as_ref().unwrap().extension().unwrap(), "md");
    }

    #[test]
    fn test_create_note_returns_correct_source() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: Single-threaded test, no concurrent env access
        unsafe { std::env::set_var("ZEN_ROOT_DIR", dir.path()) };

        let service = NoteService::new();
        let note = service
            .create_note(
                "content",
                vec!["a".to_string(), "b".to_string()],
                "my-source",
            )
            .unwrap();

        // SAFETY: Single-threaded test, no concurrent env access
        unsafe { std::env::remove_var("ZEN_ROOT_DIR") };

        assert_eq!(note.source, "my-source");
        assert_eq!(note.tags, vec!["a", "b"]);
        assert_eq!(note.content, "content");
        assert!(!note.id.is_empty());
        assert_eq!(note.sensitivity, Sensitivity::Private);
    }
}
