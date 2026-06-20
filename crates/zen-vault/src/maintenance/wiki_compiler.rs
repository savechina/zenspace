use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::wiki::WikiPage;

const TECH_KEYWORDS: &[&str] = &[
    "rust", "python", "javascript", "typescript", "go", "sqlite", "postgresql",
    "redis", "docker", "kubernetes", "react", "vue", "tokio", "async", "llm",
    "ai", "mcp", "wasm", "rig-core", "ratatui", "grpc", "http", "tcp", "udp",
    "nginx", "apache", "git", "linux", "macos", "windows", "aws", "gcp",
    "azure", "kafka", "rabbitmq", "graphql", "rest",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledWikiPage {
    pub title: String,
    pub path: PathBuf,
    pub content: String,
    pub wikilinks: Vec<String>,
    pub tags: Vec<String>,
}

pub struct WikiCompilerSkill {
    wiki_dir: PathBuf,
}

impl WikiCompilerSkill {
    pub fn new(wiki_dir: PathBuf) -> Self {
        Self { wiki_dir }
    }

    fn compile_pages(&self, notes: &[serde_json::Value]) -> Result<Vec<CompiledWikiPage>> {
        let mut pages = Vec::new();

        for note_val in notes {
            let content = note_val
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("");

            let tags: Vec<String> = note_val
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let title = self.extract_title(content).unwrap_or_else(|| {
                note_val
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("untitled")
                    .to_string()
            });

            let stripped_content = strip_frontmatter(content);
            let wikilinks = WikiPage::extract_wikilinks(&stripped_content);
            let category = self.classify_note(&content, &tags);
            let slug = self.slugify(&title);
            let path = PathBuf::from(format!("{category}/{slug}.md"));

            self.write_page(&title, &stripped_content, &wikilinks, &tags, &path)?;

            pages.push(CompiledWikiPage {
                title,
                path,
                content: stripped_content,
                wikilinks,
                tags,
            });
        }

        Ok(pages)
    }

    fn classify_note(&self, content: &str, tags: &[String]) -> &'static str {
        let content_lower = content.to_lowercase();

        for tag in tags {
            let tag_lower = tag.to_lowercase();
            if TECH_KEYWORDS.iter().any(|k| k == &tag_lower.as_str()) {
                return "entities";
            }
        }

        for word in TECH_KEYWORDS {
            if content_lower.contains(word) {
                return "entities";
            }
        }

        "concepts"
    }

    fn extract_title(&self, content: &str) -> Option<String> {
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

    fn slugify(&self, title: &str) -> String {
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

    fn write_page(
        &self,
        title: &str,
        content: &str,
        wikilinks: &[String],
        tags: &[String],
        path: &PathBuf,
    ) -> Result<()> {
        let full_path = self.wiki_dir.join(path);

        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir: {}", parent.display()))?;
        }

        let tags_str = if tags.is_empty() {
            "[]".to_string()
        } else {
            format!(
                "[{}]",
                tags.iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let links_str = if wikilinks.is_empty() {
            "[]".to_string()
        } else {
            format!(
                "[{}]",
                wikilinks
                    .iter()
                    .map(|l| format!("\"{l}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let now = Utc::now();
        let rendered = format!(
            "---\ntitle: \"{title}\"\ntags: {tags_str}\nwikilinks: {links_str}\ncreated_at: \"{created}\"\nupdated_at: \"{updated}\"\n---\n\n{content}",
            title = title,
            tags_str = tags_str,
            links_str = links_str,
            created = now.to_rfc3339(),
            updated = now.to_rfc3339(),
            content = content,
        );

        std::fs::write(&full_path, rendered)
            .with_context(|| format!("write wiki page: {}", full_path.display()))?;

        Ok(())
    }
}

impl Default for WikiCompilerSkill {
    fn default() -> Self {
        Self {
            wiki_dir: PathBuf::from("."),
        }
    }
}

#[async_trait]
impl Skill for WikiCompilerSkill {
    fn id(&self) -> &str {
        "zen-maintenance-wiki-compiler"
    }

    fn description(&self) -> &str {
        "Compile wiki pages from extracted entities, generates markdown files with wikilinks"
    }

    fn applies(&self, ctx: &InvestigationContext) -> bool {
        ctx.evidence.iter().any(|ev| ev.detail.get("notes").is_some())
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let notes_val = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("notes").cloned())
            .next();

        let notes_array = match notes_val {
            Some(ref v) => v
                .as_array()
                .ok_or_else(|| KernelError::SkillFailed("expected notes array".into()))?
                .clone(),
            None => {
                info!("WikiCompilerSkill: no notes in context, skipping");
                return Ok(SkillOutcome::noop());
            }
        };

        let pages = self
            .compile_pages(&notes_array)
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

        let page_count = pages.len();

        ctx.evidence.push(rig_compose::context::Evidence::new(
            self.id(),
            "compiled_wiki_pages",
        ).with_detail(serde_json::json!({
            "pages": pages,
            "count": page_count,
        })));

        if page_count > 0 {
            ctx.signals.push(rig_compose::context::Signal::new("wiki_pages_compiled"));
        }

        info!(
            page_count,
            "WikiCompilerSkill: execution complete"
        );

        Ok(SkillOutcome::noop().with_delta(if page_count > 0 { 0.05 } else { 0.0 }))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_note(id: &str, content: &str, tags: Vec<String>) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "content": content,
            "tags": tags,
        })
    }

    #[test]
    fn test_compile_creates_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = WikiCompilerSkill::new(tmp.path().to_path_buf());
        let notes = vec![make_note(
            "note-1",
            "# Rust Guide\n\nRust is a systems language.",
            vec![],
        )];

        let pages = skill.compile_pages(&notes).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].title, "Rust Guide");
    }

    #[test]
    fn test_classify_tech_note() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = WikiCompilerSkill::new(tmp.path().to_path_buf());
        let notes = vec![make_note("note-1", "# Rust", vec![])];
        let pages = skill.compile_pages(&notes).unwrap();
        assert!(pages[0].path.to_string_lossy().starts_with("entities/"));
    }

    #[test]
    fn test_classify_concept_note() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = WikiCompilerSkill::new(tmp.path().to_path_buf());
        let notes = vec![make_note("note-1", "# Weekly Reflection", vec![])];
        let pages = skill.compile_pages(&notes).unwrap();
        assert!(pages[0].path.to_string_lossy().starts_with("concepts/"));
    }

    #[test]
    fn test_extract_wikilinks() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = WikiCompilerSkill::new(tmp.path().to_path_buf());
        let notes = vec![make_note(
            "note-1",
            "See [[Rust]] and [[Tokio]] for details.",
            vec![],
        )];
        let pages = skill.compile_pages(&notes).unwrap();
        assert_eq!(pages[0].wikilinks, vec!["Rust", "Tokio"]);
    }

    #[test]
    fn test_strip_frontmatter_with_fm() {
        let input = "---\nid: \"abc\"\n---\n\nBody content.";
        assert_eq!(strip_frontmatter(input), "Body content.");
    }

    #[test]
    fn test_strip_frontmatter_no_fm() {
        let input = "No frontmatter.";
        assert_eq!(strip_frontmatter(input), input);
    }

    #[test]
    fn test_empty_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = WikiCompilerSkill::new(tmp.path().to_path_buf());
        let notes: Vec<serde_json::Value> = vec![];
        let pages = skill.compile_pages(&notes).unwrap();
        assert!(pages.is_empty());
    }
}
