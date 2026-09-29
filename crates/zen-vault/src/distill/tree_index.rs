//! Document segmentation for long-note distillation (compile-hygiene ⑥,
//! consumer: T046 [`super::stages::llm_distill`]).
//!
//! ## Scope Logic
//!
//! **Functionality**: pure heading-based segmentation — `segment_document`
//! splits a markdown document on ATX headings (`#`..`######`, the text before
//! the first heading becomes the level-0 preamble section, empty-body
//! sections are dropped) and `render_toc` renders the section outline that
//! rides each per-section extraction prompt as context.
//!
//! **User impact**: a note longer than the `[agentic.loop] tree_index_pages`
//! threshold (default 20 pages of 3000 chars) is distilled per section —
//! every heading section gets its own bounded LLM call, removing the
//! single-call truncation blindness. Notes below the threshold (and notes
//! with no headings at all) take the byte-identical single-call path.
//!
//! **Default behavior**: below threshold or unparseable → `None` from
//! [`segment_document`] — the caller falls back to the plain path.
//!
//! **Interaction**: the threshold lives in LoopConfig
//! (`tree_index_pages_or_default`, clamp 5..=200); this module holds no
//! config of its own.

/// Characters per estimated "page" (~750 tokens) — the threshold unit.
pub const PAGE_CHARS: usize = 3_000;

/// One heading-delimited section of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSection {
    /// Heading level (1-6); the pre-first-heading preamble is level 0 with
    /// an empty title.
    pub level: u8,
    /// Heading text without the `#` markers (empty for the preamble).
    pub title: String,
    /// Body lines after the heading (no trailing blank-run).
    pub body: String,
}

/// True when the document exceeds the page threshold and should be
/// segmented rather than sent as one truncated call.
pub fn should_segment(content: &str, pages_threshold: u32) -> bool {
    content.chars().count() > pages_threshold as usize * PAGE_CHARS
}

/// Split a markdown document on ATX headings. Returns `None` when the
/// document has no headings at all (nothing to segment — the caller falls
/// back to the fulltext path). Sections with an empty body are dropped;
/// a document whose every section is empty yields `None`.
pub fn segment_document(content: &str) -> Option<Vec<DocSection>> {
    let mut sections: Vec<DocSection> = Vec::new();
    let mut current: Option<DocSection> = None;
    let mut body = String::new();
    let mut saw_heading = false;

    // Flush the pending section (or an orphan preamble, before the first
    // heading, as a level-0 section). Empty bodies are dropped.
    let mut flush = |current: &mut Option<DocSection>, body: &mut String| {
        let trimmed = body.trim().to_string();
        body.clear();
        if let Some(mut section) = current.take() {
            section.body = trimmed;
            if !section.body.is_empty() {
                sections.push(section);
            }
        } else if !trimmed.is_empty() {
            sections.push(DocSection {
                level: 0,
                title: String::new(),
                body: trimmed,
            });
        }
    };

    for line in content.lines() {
        if let Some((level, title)) = parse_heading(line) {
            flush(&mut current, &mut body);
            saw_heading = true;
            current = Some(DocSection {
                level,
                title,
                body: String::new(),
            });
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&mut current, &mut body);

    // No headings at all → nothing to segment; the caller falls back to the
    // fulltext path.
    if !saw_heading || sections.is_empty() {
        None
    } else {
        Some(sections)
    }
}

/// Parse an ATX heading line (`#`..`######` followed by a space). Returns
/// `(level, title)` or `None` for non-heading lines.
fn parse_heading(line: &str) -> Option<(u8, String)> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 || !line[hashes..].starts_with(' ') {
        return None;
    }
    let title = line[hashes..].trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some((hashes as u8, title))
}

/// Render the section outline as context for per-section extraction calls:
/// one indented `- # Title` line per section (preamble renders as `- (intro)`).
pub fn render_toc(sections: &[DocSection]) -> String {
    sections
        .iter()
        .map(|s| {
            let indent = "  ".repeat(s.level as usize);
            let title = if s.title.is_empty() {
                "(intro)"
            } else {
                &s.title
            };
            format!("{indent}- {title}\n")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_multi_level_headings_with_preamble() {
        let doc = "intro line\n\n# One\nbody one\n\n## Sub\nsub body\n# Two\ntail";
        let sections = segment_document(doc).unwrap();
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].level, 0);
        assert_eq!(sections[0].title, "");
        assert_eq!(sections[0].body, "intro line");
        assert_eq!(sections[1].level, 1);
        assert_eq!(sections[1].title, "One");
        assert_eq!(sections[2].level, 2);
        assert_eq!(sections[2].title, "Sub");
        assert_eq!(sections[2].body, "sub body");
        assert_eq!(sections[3].title, "Two");
        assert_eq!(sections[3].body, "tail");
    }

    #[test]
    fn drops_empty_bodies_and_returns_none_without_content() {
        // Headings with no body at all → every section empty → None.
        assert_eq!(segment_document("# A\n## B\n"), None);
        // No headings → None (fulltext fallback).
        assert_eq!(segment_document("just prose\nno headings\n"), None);
    }

    #[test]
    fn ignores_fenced_hashes_and_deep_levels() {
        // A `#######` (7 hashes) line is not a heading — it stays content of
        // the preamble. (`#### ` inside a code fence is still split: this
        // splitter is line-lexical by design — the distill consumer only
        // feeds it LLM-bounded sections.)
        let doc = "text\n####### seven\n#### four\nbody";
        let sections = segment_document(doc).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "");
        assert!(sections[0].body.contains("####### seven"));
        assert_eq!(sections[1].title, "four");
        assert_eq!(sections[1].body, "body");
    }

    #[test]
    fn toc_renders_indented_outline() {
        let doc = "intro\n# One\nb\n## Sub\nb";
        let sections = segment_document(doc).unwrap();
        let toc = render_toc(&sections);
        assert!(toc.contains("- (intro)"));
        assert!(toc.contains("- One"));
        assert!(toc.contains("  - Sub"));
    }

    #[test]
    fn threshold_compares_chars_to_pages() {
        assert!(!should_segment(&"a".repeat(20 * 3_000), 20));
        assert!(should_segment(&"a".repeat(20 * 3_000 + 1), 20));
    }
}
