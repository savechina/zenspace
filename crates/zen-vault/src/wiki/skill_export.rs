//! E7 (compile-hygiene): SKILL.md export for external agents.
//!
//! Renders the compiled wiki's inventory into an [Agent Skills](https://agentskills.io/specification)
//! `SKILL.md` so Claude Code / Codex CLI / Gemini CLI can discover and read
//! the zen knowledge base with zero runtime machinery. The vault stays the
//! source of truth — the skill is a rendered view, regenerated on demand.
//!
//! Spec discipline: the frontmatter emits ONLY the six portable fields
//! (name, description, license, compatibility, metadata, allowed-tools) —
//! tool-specific extensions (Claude Code's `when_to_use`, `allowed-tools`
//! semantics, …) would fail claude.ai/Skills-API packaging with a hard
//! error and are deliberately avoided. `name` rules: `[a-z0-9-]`, ≤64
//! chars, no leading/trailing/double hyphens, must equal the parent
//! directory name — hence the fixed `zen-wiki` identity. Body: < 500 lines
//! (progressive disclosure); the page index is capped with an explicit
//! truncation marker rather than silently growing.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

/// Fixed skill identity — MUST equal the directory the file is written to
/// (Agent Skills spec: name must match the parent directory name).
pub const WIKI_SKILL_NAME: &str = "zen-wiki";

/// One wiki page as rendered into the skill's page index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiSkillPage {
    /// Wiki-relative path (also the link target external agents read).
    pub rel_path: String,
    /// Frontmatter `title:` → first `# ` heading → file stem fallback.
    pub title: String,
    /// Frontmatter `description:` (may be empty).
    pub description: String,
}

/// The skill file name and directory this export writes to.
pub fn skill_file_name() -> &'static str {
    "SKILL.md"
}

/// Walk `wiki_dir` (same enumeration the compile whitelist uses) and
/// collect per-page index metadata.
pub fn collect_skill_pages(wiki_dir: &Path) -> Result<Vec<WikiSkillPage>> {
    let mut pages = Vec::new();
    for (name, rel) in crate::graph_verify::wiki_page_inventory(wiki_dir) {
        // OKF §6: machine-rendered bookkeeping files are not pages.
        if name == "index" || name == "log" {
            continue;
        }
        let abs = wiki_dir.join(&rel);
        let content = std::fs::read_to_string(&abs)
            .with_context(|| format!("read wiki page: {}", abs.display()))?;
        let (title, description) = page_metadata(&content, &name);
        pages.push(WikiSkillPage {
            rel_path: rel.to_string(),
            title,
            description,
        });
    }
    Ok(pages)
}

/// Title/description extraction: frontmatter keys first, then heading,
/// then the inventory stem. Bounded so a pathological page cannot blow
/// up the index line length.
fn page_metadata(content: &str, stem: &str) -> (String, String) {
    let mut title = String::new();
    let mut description = String::new();
    if let Some(frontmatter) = content.strip_prefix("---\n").and_then(|c| {
        c.split("\n---")
            .next()
            .map(|rest| rest.trim_end().to_string())
    }) {
        for line in frontmatter.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("title:") {
                title = v.trim().trim_matches('"').to_string();
            } else if let Some(v) = line.strip_prefix("description:") {
                description = v.trim().trim_matches('"').to_string();
            }
        }
    }
    if title.is_empty() {
        title = content
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(|h| h.trim().to_string())
            .unwrap_or_else(|| stem.to_string());
    }
    title.truncate(120);
    description.truncate(200);
    (title, description)
}

/// Render the SKILL.md content. Pure — pinned by tests for byte-0
/// frontmatter, name conformance, description budget and the line cap.
pub fn render_wiki_skill_md(pages: &[WikiSkillPage]) -> String {
    let description = format!(
        "Read and navigate the zen personal knowledge base: a plain-markdown \
wiki of {page_count} pages covering concepts, technology notes, research, \
coding references and reports, cross-linked with [[wikilinks]] and carrying \
frontmatter (type/description/tags/sources). Use when the user asks about \
their notes, knowledge base, wiki pages, or anything documented there - \
locate pages, read topics, follow links, or summarize the KB's contents.",
        page_count = pages.len()
    );
    let description = truncate_chars(&description, 1024);

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {WIKI_SKILL_NAME}\n"));
    // Single YAML line — the description contains no control characters
    // (frontmatter descriptions are single-line values in this vault).
    out.push_str(&format!("description: {description}\n"));
    out.push_str("---\n\n");

    out.push_str(&format!("# {WIKI_SKILL_NAME}\n\n"));
    out.push_str(
        "The zen knowledge base is a directory tree of plain markdown pages.\n\
         Read any page directly with your file tools; follow `[[wikilinks]]` by\n\
         resolving the link text against the page index below (link text is the\n\
         page title; file names are the kebab-cased slug).\n\n\
         ## Conventions\n\n\
         - Frontmatter keys: `type` (page kind), `title`, `description`, `tags`,\n\
         `sources` (originating note paths), `created_at`/`updated_at`.\n\
         - `[[wikilink]]` targets are page titles or slugs; a link to a page\n\
         listed below resolves to that file.\n\
         - Directories group pages: `notions/concepts`, `notions/technology`,\n\
         `coding`, `research`, `reports`, `topics`, `wisdom`.\n\n\
         ## Page index\n\n",
    );

    const MAX_INDEX_LINES: usize = 240;
    let mut by_dir: BTreeMap<&str, Vec<&WikiSkillPage>> = BTreeMap::new();
    for page in pages {
        let dir = page.rel_path.split('/').next().unwrap_or("");
        by_dir.entry(dir).or_default().push(page);
    }
    let mut index_lines = 0usize;
    let mut truncated = false;
    for (dir, group) in &by_dir {
        if index_lines >= MAX_INDEX_LINES {
            truncated = true;
            break;
        }
        out.push_str(&format!("### {dir}\n\n"));
        for page in group {
            if index_lines >= MAX_INDEX_LINES {
                truncated = true;
                break;
            }
            if page.description.is_empty() {
                out.push_str(&format!("- [{}]({})\n", page.title, page.rel_path));
            } else {
                out.push_str(&format!(
                    "- [{}]({}) - {}\n",
                    page.title, page.rel_path, page.description
                ));
            }
            index_lines += 1;
        }
        out.push('\n');
    }
    if truncated {
        out.push_str(&format!(
            "> Index truncated at {MAX_INDEX_LINES} entries to keep this file\n\
             > under the skill body budget. Enumerate the wiki directory for\n\
             > the full page list.\n"
        ));
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(i) if i > max / 2 => format!("{}...", &cut[..i]),
        _ => format!("{cut}..."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(rel: &str, title: &str, description: &str) -> WikiSkillPage {
        WikiSkillPage {
            rel_path: rel.to_string(),
            title: title.to_string(),
            description: description.to_string(),
        }
    }

    #[test]
    fn frontmatter_is_byte_zero_with_conformant_name() {
        let out = render_wiki_skill_md(&[]);
        assert!(out.starts_with("---\n"), "`---` must be byte 0");
        let name_line = out.lines().nth(1).unwrap();
        assert_eq!(name_line, "name: zen-wiki");
        // Spec name rules: [a-z0-9-], no double/trailing hyphens.
        let name = WIKI_SKILL_NAME;
        assert!(name.len() <= 64);
        assert!(name.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
        assert!(!name.starts_with('-') && !name.ends_with('-'));
        assert!(!name.contains("--"));
    }

    #[test]
    fn description_within_budget_and_mentions_when_to_use() {
        let out = render_wiki_skill_md(&[page("topics/a.md", "A", "about a")]);
        let description = out
            .lines()
            .find(|l| l.starts_with("description: "))
            .unwrap()
            .trim_start_matches("description: ");
        assert!(description.chars().count() <= 1024);
        assert!(description.contains("Use when"));
    }

    #[test]
    fn index_groups_by_directory_and_links_are_relative() {
        let pages = vec![
            page("notions/concepts/rust.md", "Rust", "systems language"),
            page("topics/zen.md", "Zen", ""),
        ];
        let out = render_wiki_skill_md(&pages);
        assert!(out.contains("### notions\n"));
        assert!(out.contains("### topics\n"));
        assert!(out.contains("- [Rust](notions/concepts/rust.md) - systems language"));
        assert!(out.contains("- [Zen](topics/zen.md)\n"));
    }

    #[test]
    fn huge_index_is_capped_with_truncation_marker() {
        let pages: Vec<WikiSkillPage> = (0..400)
            .map(|i| page(&format!("topics/p{i}.md"), &format!("P{i}"), ""))
            .collect();
        let out = render_wiki_skill_md(&pages);
        assert!(out.contains("Index truncated at"));
        assert!(out.lines().count() < 500, "skill body budget");
    }

    #[test]
    fn collect_reads_inventory_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        std::fs::create_dir_all(wiki.join("topics")).unwrap();
        std::fs::write(
            wiki.join("topics/zen.md"),
            "---\ntitle: Zen Practice\ndescription: daily sits\ntype: note\n---\n\n# Zen\n",
        )
        .unwrap();
        std::fs::write(wiki.join("log.md"), "# log\n").unwrap();
        let pages = collect_skill_pages(&wiki).unwrap();
        assert_eq!(pages.len(), 1, "index.md/log.md exempt like OKF §6");
        assert_eq!(pages[0].title, "Zen Practice");
        assert_eq!(pages[0].description, "daily sits");
        assert_eq!(pages[0].rel_path, "topics/zen.md");
    }
}
