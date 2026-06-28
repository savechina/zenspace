use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use tracing::info;

use crate::entity::EntityData;
use crate::note::Note;
use crate::wiki::{WikiIndex, WikiLog, WikiPage, WikiStructure};

/// Known technology keywords for entity classification.
const TECH_KEYWORDS: &[&str] = &[
    "rust",
    "python",
    "javascript",
    "typescript",
    "go",
    "sqlite",
    "postgresql",
    "redis",
    "docker",
    "kubernetes",
    "react",
    "vue",
    "tokio",
    "async",
    "llm",
    "ai",
    "mcp",
    "wasm",
    "rig-core",
    "ratatui",
    "grpc",
    "http",
    "tcp",
    "udp",
    "nginx",
    "apache",
    "git",
    "linux",
    "macos",
    "windows",
    "aws",
    "gcp",
    "azure",
    "kafka",
    "rabbitmq",
    "graphql",
    "rest",
];

/// Categorization result for a note's entity type.
#[derive(Debug, Clone, PartialEq)]
enum NoteCategory {
    /// Technology entity → goes under `entities/`
    Technology,
    /// Concept/page → goes under `concepts/`
    Concept,
}

impl NoteCategory {
    fn directory_name(&self) -> &str {
        match self {
            NoteCategory::Technology => "entities",
            NoteCategory::Concept => "concepts",
        }
    }
}

/// WikiCompiler converts [`Note`] objects into [`WikiPage`] objects,
/// writes them to disk, and maintains the wiki index and log.
pub struct WikiCompiler;

impl WikiCompiler {
    /// Create a new `WikiCompiler`.
    pub fn new() -> Self {
        Self
    }

    /// Compile a batch of notes into wiki pages.
    ///
    /// For each note:
    /// 1. Extract title (first `# Heading` or fallback to note id)
    /// 2. Extract content (strip frontmatter if present)
    /// 3. Extract wikilinks using `[[...]]` pattern
    /// 4. Categorize (Technology → `entities/`, Concept → `concepts/`)
    /// 5. Write to disk under `wiki_dir`
    ///
    /// After processing all notes:
    /// - Creates wiki directory structure via [`WikiStructure`]
    /// - Generates `index.md` via [`WikiIndex`]
    /// - Logs operations via [`WikiLog`]
    pub fn compile(&self, notes: &[Note], wiki_dir: &Path) -> Result<Vec<WikiPage>> {
        let structure = WikiStructure::new(wiki_dir);
        structure
            .ensure_directories()
            .context("ensure wiki directories")?;

        let log = WikiLog::new(wiki_dir);
        log.append("compile_start", &format!("compiling {} notes", notes.len()))?;

        let mut pages = Vec::with_capacity(notes.len());
        let mut written = 0usize;

        for note in notes {
            let page = self.note_to_page(note)?;
            let full_path = wiki_dir.join(&page.path);

            // Atomic write via parent dir creation + temp rename
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create dir: {}", parent.display()))?;
            }

            let rendered = self.render_page(&page);
            std::fs::write(&full_path, &rendered)
                .with_context(|| format!("write wiki page: {}", full_path.display()))?;

            info!(
                note_id = %note.id,
                title = %page.title,
                path = %page.path.display(),
                wikilinks = page.wikilinks.len(),
                "wiki page written"
            );

            log.append(
                "page_created",
                &format!("{} -> {}", page.title, page.path.display()),
            )?;

            pages.push(page);
            written += 1;
        }

        if !pages.is_empty() {
            let index = WikiIndex::new(wiki_dir);
            index.generate(&pages).context("generate index")?;
            log.append("index_generated", &format!("{} pages indexed", pages.len()))?;
        }

        log.append(
            "compile_complete",
            &format!("{} pages written from {} notes", written, notes.len()),
        )?;

        info!(
            "wiki compile complete: {written} pages from {} notes",
            notes.len()
        );
        Ok(pages)
    }

    /// Compile entity data into wiki pages under `wiki/entities/`.
    /// Relationships rendered as `[[wikilinks]]` for cross-linking.
    pub fn compile_from_entities(&self, entities: &[EntityData], wiki_dir: &Path) -> Result<usize> {
        let structure = WikiStructure::new(wiki_dir);
        structure.ensure_directories()?;

        let log = WikiLog::new(wiki_dir);
        log.append(
            "entity_compile_start",
            &format!("compiling {} entity pages", entities.len()),
        )?;

        let mut written = 0usize;

        for data in entities {
            let slug = slugify(&data.entity.name);
            let rel_path = format!("entities/{slug}.md");
            let full_path = wiki_dir.join(&rel_path);

            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create dir: {}", parent.display()))?;
            }

            let md = self.render_entity_page(data);
            std::fs::write(&full_path, &md)
                .with_context(|| format!("write entity wiki: {}", full_path.display()))?;

            info!(entity = %data.entity.name, path = %rel_path, "entity wiki page written");
            written += 1;
        }

        log.append(
            "entity_compile_complete",
            &format!("{written} entity pages compiled"),
        )?;

        self.generate_entity_index(entities, wiki_dir)?;

        Ok(written)
    }

    /// Generate OKF v0.1 §6 compliant `index.md` — no frontmatter,
    /// standard markdown links with descriptions for progressive disclosure.
    fn generate_entity_index(&self, entities: &[EntityData], wiki_dir: &Path) -> Result<()> {
        let index_path = wiki_dir.join("index.md");
        let mut md = String::new();

        // OKF §6: index files contain no frontmatter.
        md.push_str("# Knowledge Index\n\n## Entities\n\n");

        let mut sorted: Vec<&EntityData> = entities.iter().collect();
        sorted.sort_by_key(|data| data.entity.name.to_lowercase());

        for data in &sorted {
            let slug = slugify(&data.entity.name);
            let desc = data
                .facts
                .first()
                .map(|f| truncate_for_description(f))
                .unwrap_or_else(|| format!("{:?} entity", data.entity.entity_type));
            // OKF §6: "* [Title](relative-url) - description"
            md.push_str(&format!(
                "* [{}]({}.md) - {}\n",
                data.entity.name, slug, desc
            ));
        }

        std::fs::write(&index_path, &md)
            .with_context(|| format!("write entity index: {}", index_path.display()))?;
        Ok(())
    }

    /// Render an entity page as OKF v0.1 compliant markdown.
    ///
    /// Frontmatter follows §4.1: `type` is required; `title`, `description`,
    /// `tags`, `timestamp` are recommended. `created_at` is a zen extension
    /// (§4.1 allows producer-defined keys).
    ///
    /// Cross-links use standard markdown `[text](/path.md)` per §5.1 (absolute,
    /// bundle-relative), not `[[wikilinks]]`.
    fn render_entity_page(&self, data: &EntityData) -> String {
        let mut md = String::new();

        let type_str = format!("{:?}", data.entity.entity_type);
        let now = chrono::Utc::now();

        let description = data
            .facts
            .first()
            .map(|f| truncate_for_description(f))
            .unwrap_or_else(|| format!("{} entity", data.entity.name));

        // OKF §4.1 frontmatter
        let mut fm = format!(
            "---\n\
             type: {type_str}\n\
             title: {name}\n\
             description: {description}\n\
             tags: [{tag}]\n",
            name = data.entity.name,
            tag = type_str.to_lowercase(),
        );

        if let Some(ref domain) = data.entity.domain {
            fm.push_str(&format!("domain: {domain}\n"));
        }
        if !data.entity.aliases.is_empty() {
            let aliases_str = data
                .entity
                .aliases
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            fm.push_str(&format!("aliases: [{aliases_str}]\n"));
        }
        fm.push_str(&format!(
            "timestamp: {ts}\n\
             created_at: {created}\n\
             ---\n\n",
            ts = now.to_rfc3339(),
            created = data.entity.created_at.to_rfc3339(),
        ));
        md.push_str(&fm);

        md.push_str(&format!("# {}\n\n", data.entity.name));

        if !data.facts.is_empty() {
            md.push_str("## Facts\n\n");
            for fact in &data.facts {
                md.push_str(&format!("- {fact}\n"));
            }
            md.push('\n');
        }

        if !data.relationships.is_empty() {
            md.push_str("## Relationships\n\n");
            for (target, rel) in &data.relationships {
                let rel_str = format!("{rel:?}");
                let target_slug = slugify(target);
                // OKF §5.1: absolute bundle-relative links
                md.push_str(&format!(
                    "- [{target}](/entities/{target_slug}.md) — {rel_str}\n"
                ));
            }
        }

        md
    }

    // ── Internal helpers ─────────────────────────────────────────

    /// Convert a single [`Note`] into a [`WikiPage`].
    fn note_to_page(&self, note: &Note) -> Result<WikiPage> {
        let title = extract_title(&note.content).unwrap_or_else(|| slugify(&note.id));
        let content = strip_frontmatter(&note.content);
        let wikilinks = WikiPage::extract_wikilinks(&content);
        let category = classify_note(&note.content, &note.tags);

        let slug = slugify(&title);
        let path = PathBuf::from(format!("{}/{slug}.md", category.directory_name()));

        Ok(WikiPage {
            title,
            path,
            created_at: note.created_at,
            updated_at: note.updated_at,
            tags: note.tags.clone(),
            wikilinks,
            para: note.para.clone(),
            okf_type: note.okf_type.clone(),
            content,
        })
    }

    /// Render a [`WikiPage`] back to markdown with front matter header.
    fn render_page(&self, page: &WikiPage) -> String {
        let tags_str = if page.tags.is_empty() {
            "[]".to_string()
        } else {
            format!(
                "[{}]",
                page.tags
                    .iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let links_str = if page.wikilinks.is_empty() {
            "[]".to_string()
        } else {
            format!(
                "[{}]",
                page.wikilinks
                    .iter()
                    .map(|l| format!("\"{l}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let mut fm = format!(
            "---\ntitle: \"{}\"\ntags: {}\nwikilinks: {}",
            page.title, tags_str, links_str,
        );

        if let Some(ref para) = page.para {
            fm.push_str(&format!("\npara: \"{}\"", para));
        }
        if let Some(ref okf_type) = page.okf_type {
            fm.push_str(&format!("\ntype: \"{}\"", okf_type));
        }

        fm.push_str(&format!(
            "\ncreated_at: \"{}\"\nupdated_at: \"{}\"\n---\n\n{}",
            page.created_at.to_rfc3339(),
            page.updated_at.to_rfc3339(),
            page.content,
        ));

        fm
    }
}

impl Default for WikiCompiler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Skill for WikiCompiler {
    fn id(&self) -> &str {
        "zen-wiki-compilation"
    }

    fn description(&self) -> &str {
        "Compile notes into structured wiki pages with headings, wikilinks, and categorized directories"
    }

    fn applies(&self, _ctx: &InvestigationContext) -> bool {
        true
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let wiki_dir_str = ctx
            .evidence
            .iter()
            .filter_map(|ev| {
                ev.detail
                    .get("wiki_dir")
                    .and_then(|d| d.as_str())
                    .map(String::from)
            })
            .next();

        let wiki_dir = match wiki_dir_str {
            Some(dir) => PathBuf::from(dir),
            None => {
                return Ok(SkillOutcome::noop());
            }
        };

        let notes_val = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("notes").cloned())
            .next();

        let pages = if let Some(notes_json) = notes_val {
            let notes = Self::notes_from_json(&notes_json)
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
            let structure = WikiStructure::new(&wiki_dir);
            structure
                .ensure_directories()
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

            let log = WikiLog::new(&wiki_dir);
            log.append(
                "skill_compile_start",
                &format!("compiling {} notes", notes.len()),
            )
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

            let mut compiled = Vec::with_capacity(notes.len());
            for note in &notes {
                let page = self
                    .note_to_page(note)
                    .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
                let full_path = wiki_dir.join(&page.path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
                }
                let rendered = self.render_page(&page);
                std::fs::write(&full_path, &rendered)
                    .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
                compiled.push(page);
            }

            if !compiled.is_empty() {
                let index = WikiIndex::new(&wiki_dir);
                index
                    .generate(&compiled)
                    .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
            }

            log.append(
                "skill_compile_complete",
                &format!(
                    "{} pages written from {} notes",
                    compiled.len(),
                    notes.len()
                ),
            )
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

            compiled
        } else {
            Vec::new()
        };

        let page_count = pages.len();
        info!(page_count, "Wiki compilation skill complete");

        Ok(SkillOutcome::noop().with_delta(if page_count > 0 { 0.05 } else { 0.0 }))
    }
}

// ── Notes-from-JSON helper ────────────────────────────────────────

impl WikiCompiler {
    fn notes_from_json(notes_val: &serde_json::Value) -> Result<Vec<Note>> {
        let notes_array = notes_val
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("expected notes array"))?;

        let mut notes = Vec::new();
        for note_val in notes_array {
            let note: Note = serde_json::from_value(note_val.clone())
                .map_err(|e| anyhow::anyhow!("failed to parse note: {e}"))?;
            notes.push(note);
        }
        Ok(notes)
    }
}

fn extract_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                return Some(heading.to_string());
            }
        }
    }
    None
}

fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }

    let rest = &trimmed[3..];
    if let Some(end_pos) = rest.find("---") {
        let body = &rest[end_pos + 3..];
        body.trim_start().to_string()
    } else {
        content.to_string()
    }
}

fn classify_note(content: &str, tags: &[String]) -> NoteCategory {
    let content_lower = content.to_lowercase();

    for tag in tags {
        let tag_lower = tag.to_lowercase();
        if TECH_KEYWORDS.iter().any(|k| k.to_lowercase() == tag_lower) {
            return NoteCategory::Technology;
        }
    }

    for word in TECH_KEYWORDS {
        if content_lower.contains(&word.to_lowercase()) {
            return NoteCategory::Technology;
        }
    }

    NoteCategory::Concept
}

fn truncate_for_description(s: &str) -> String {
    const MAX: usize = 100;
    if s.len() <= MAX {
        s.replace('"', "'").replace('\n', " ")
    } else {
        let truncated = s[..s
            .char_indices()
            .take(MAX)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(MAX)]
            .replace('"', "'")
            .replace('\n', " ");
        format!("{truncated}…")
    }
}

fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut prev_dash = false;

    for c in title.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::types::Sensitivity;

    fn make_test_note(content: &str, tags: Vec<String>) -> Note {
        let now = chrono::Utc::now();
        Note {
            id: uuid::Uuid::now_v7().to_string(),
            tags,
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: now,
            updated_at: now,
            domain: vec![],
            project: None,
            para: None,
            okf_type: None,
            content: content.to_string(),
            file_path: None,
        }
    }

    fn make_test_note_with_id(id: &str, content: &str, tags: Vec<String>) -> Note {
        let now = chrono::Utc::now();
        Note {
            id: id.to_string(),
            tags,
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: now,
            updated_at: now,
            domain: vec![],
            project: None,
            para: None,
            okf_type: None,
            content: content.to_string(),
            file_path: None,
        }
    }

    #[test]
    fn test_compile_creates_wiki_directory_structure() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# Hello World\n\nContent", vec![])];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages.len(), 1);

        assert!(dir.path().join("entities").exists());
        assert!(dir.path().join("concepts").exists());
        assert!(dir.path().join("sources").exists());
    }

    #[test]
    fn test_compile_extracts_title_from_heading() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# My Page Title\n\nBody text", vec![])];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages[0].title, "My Page Title");
    }

    #[test]
    fn test_compile_falls_back_to_id_when_no_heading() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let note = make_test_note_with_id("abc-123", "Just plain content", vec![]);
        let notes = vec![note];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages[0].title, "abc-123");
    }

    #[test]
    fn test_compile_extracts_wikilinks() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "See [[Rust]] and [[Tokio]] for details.",
            vec![],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages[0].wikilinks, vec!["Rust", "Tokio"]);
    }

    #[test]
    fn test_compile_copies_tags_from_note() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Tagged Note\n\nContent",
            vec!["tag1".into(), "tag2".into()],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages[0].tags, vec!["tag1", "tag2"]);
    }

    #[test]
    fn test_compile_copies_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let now = chrono::Utc::now();
        let note = Note {
            id: "ts-note".to_string(),
            tags: vec![],
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: now,
            updated_at: now,
            domain: vec![],
            project: None,
            para: None,
            okf_type: None,
            content: "# TS Note\n\nBody".to_string(),
            file_path: None,
        };

        let pages = compiler.compile(&[note], dir.path()).unwrap();
        assert_eq!(pages[0].created_at, now);
        assert_eq!(pages[0].updated_at, now);
    }

    #[test]
    fn test_compile_classifies_tech_entity() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Rust Performance\n\nRust is fast.",
            vec![],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(pages[0].path.to_string_lossy().starts_with("entities/"));
    }

    #[test]
    fn test_compile_classifies_concept() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Weekly Reflection\n\nThinking about life.",
            vec![],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(pages[0].path.to_string_lossy().starts_with("concepts/"));
    }

    #[test]
    fn test_compile_classifies_by_tag_keyword() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Some General Note\n\nContent here.",
            vec!["rust".into()],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(pages[0].path.to_string_lossy().starts_with("entities/"));
    }

    #[test]
    fn test_compile_empty_notes_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let pages = compiler.compile(&[], dir.path()).unwrap();
        assert!(pages.is_empty());
    }

    #[test]
    fn test_compile_strips_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let content = "---\nid: \"existing\"\n---\n\n# After Frontmatter\n\nBody text.";
        let notes = vec![make_test_note(content, vec![])];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(!pages[0].content.contains("---"));
        assert!(pages[0].content.contains("After Frontmatter"));
    }

    #[test]
    fn test_compile_writes_files_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# Disk Page\n\nContent", vec![])];

        compiler.compile(&notes, dir.path()).unwrap();

        // Find the written file (path depends on classification)
        for entry in walkdir::WalkDir::new(dir.path())
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path().extension().is_some_and(|e| e == "md") {
                let content = std::fs::read_to_string(entry.path()).unwrap();
                assert!(content.contains("Disk Page"));
                assert!(content.contains("---")); // frontmatter written by render
                return;
            }
        }
        panic!("No wiki .md file found on disk");
    }

    #[test]
    fn test_compile_generates_index() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![
            make_test_note("# Page One\n\nOne", vec![]),
            make_test_note("# Page Two\n\nTwo", vec![]),
        ];

        compiler.compile(&notes, dir.path()).unwrap();

        let index_path = dir.path().join("index.md");
        assert!(index_path.exists());

        let index_content = std::fs::read_to_string(&index_path).unwrap();
        assert!(index_content.contains("Knowledge Index"));
        assert!(index_content.contains("Page One"));
        assert!(index_content.contains("Page Two"));
    }

    #[test]
    fn test_compile_appends_to_log() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# Log Test\n\nContent", vec![])];

        compiler.compile(&notes, dir.path()).unwrap();

        let log_path = dir.path().join("log.md");
        assert!(log_path.exists());

        let log_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(log_content.contains("compile_start"));
        assert!(log_content.contains("page_created"));
        assert!(log_content.contains("compile_complete"));
    }

    #[test]
    fn test_compile_multiple_notes_to_different_categories() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![
            make_test_note("# Rust Guide\n\nRust is great", vec![]),
            make_test_note("# Life Lessons\n\nJust thinking", vec![]),
        ];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages.len(), 2);

        let has_entity = pages
            .iter()
            .any(|p| p.path.to_string_lossy().starts_with("entities/"));
        let has_concept = pages
            .iter()
            .any(|p| p.path.to_string_lossy().starts_with("concepts/"));
        assert!(has_entity, "should have an entity page");
        assert!(has_concept, "should have a concept page");
    }

    // ── Helper function tests ───────────────────────────────────

    #[test]
    fn test_extract_title_single_heading() {
        assert_eq!(
            extract_title("# My Title\n\nBody"),
            Some("My Title".to_string())
        );
    }

    #[test]
    fn test_extract_title_h2_fallback() {
        assert_eq!(
            extract_title("## Secondary Title\n\nBody"),
            Some("Secondary Title".to_string())
        );
    }

    #[test]
    fn test_extract_title_no_heading() {
        assert_eq!(extract_title("Just a paragraph"), None);
    }

    #[test]
    fn test_extract_title_empty_heading() {
        assert_eq!(extract_title("# \n\nBody"), None);
    }

    #[test]
    fn test_strip_frontmatter_with_fm() {
        let input = "---\nid: \"abc\"\ntitle: \"test\"\n---\n\nBody content here.";
        let result = strip_frontmatter(input);
        assert_eq!(result, "Body content here.");
        assert!(!result.contains("---"));
    }

    #[test]
    fn test_strip_frontmatter_no_fm() {
        let input = "No frontmatter here.\nJust markdown.";
        let result = strip_frontmatter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_frontmatter_unclosed() {
        let input = "---\nid: unclosed\nBody without close";
        let result = strip_frontmatter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_classify_note_technology_by_keyword() {
        assert_eq!(
            classify_note("I love using Rust for systems.", &[]),
            NoteCategory::Technology
        );
    }

    #[test]
    fn test_classify_note_technology_by_tag() {
        assert_eq!(
            classify_note("Some content here.", &["docker".to_string()]),
            NoteCategory::Technology
        );
    }

    #[test]
    fn test_classify_note_concept_fallback() {
        assert_eq!(
            classify_note("Reflecting on personal growth.", &["journal".to_string()]),
            NoteCategory::Concept
        );
    }

    #[test]
    fn test_slugify_simple() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(slugify("What's New in 2024?"), "what-s-new-in-2024");
    }

    #[test]
    fn test_slugify_collapse_dashes() {
        assert_eq!(slugify("Hello   World"), "hello-world");
    }

    #[test]
    fn test_slugify_trim_edges() {
        assert_eq!(slugify("!Hello World!"), "hello-world");
    }

    #[test]
    fn test_slugify_uuid_fallback() {
        assert_eq!(slugify("abc-123-def"), "abc-123-def");
    }

    #[test]
    fn test_note_to_page_basic() {
        let compiler = WikiCompiler::new();
        let note = make_test_note("# Test Page\n\nContent here.", vec!["tag1".into()]);
        let page = compiler.note_to_page(&note).unwrap();

        assert_eq!(page.title, "Test Page");
        assert_eq!(page.tags, vec!["tag1"]);
        assert!(!page.content.contains("---"));
    }

    #[test]
    fn test_render_page_includes_frontmatter() {
        let compiler = WikiCompiler::new();
        let page = WikiPage {
            title: "Rendered Page".to_string(),
            path: PathBuf::from("concepts/rendered-page.md"),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            tags: vec!["tag1".into()],
            wikilinks: vec!["Link1".into()],
            para: None,
            okf_type: None,
            content: "Body content".to_string(),
        };

        let rendered = compiler.render_page(&page);
        assert!(rendered.contains("title: \"Rendered Page\""));
        assert!(rendered.contains("tags: [\"tag1\"]"));
        assert!(rendered.contains("wikilinks: [\"Link1\"]"));
        assert!(rendered.contains("Body content"));
    }
}
