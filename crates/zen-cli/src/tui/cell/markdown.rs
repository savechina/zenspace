use crate::tui::markdown::render_markdown;
use crate::tui::render::normalize_compact_markdown;
use ratatui::text::Line;

#[derive(Debug, Clone)]
pub struct MarkdownCell {
    pub content: String,
    cached_lines: Vec<Line<'static>>,
}

impl MarkdownCell {
    pub fn new(content: impl Into<String>) -> Self {
        let raw = content.into();
        let normalized = normalize_compact_markdown(&raw);
        let cached_lines = render_markdown(&normalized);
        Self {
            content: normalized,
            cached_lines,
        }
    }

    pub fn from_lines(_lines: Vec<Line<'static>>, raw_text: String) -> Self {
        Self::new(raw_text)
    }

    pub fn display_lines(&self) -> &[Line<'static>] {
        &self.cached_lines
    }
}

impl From<String> for MarkdownCell {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for MarkdownCell {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_cell_renders_on_display() {
        let cell = MarkdownCell::new("# Heading\n\nParagraph");
        let displayed = cell.display_lines();
        assert!(
            displayed.len() >= 2,
            "heading + paragraph should render as 2+ lines"
        );
    }

    #[test]
    fn cell_normalizes_compact_markdown() {
        let cell = MarkdownCell::new("text. # Heading");
        let displayed = cell.display_lines();
        assert!(
            displayed.len() >= 2,
            "compact markdown should be normalized and rendered as 2+ lines"
        );
    }

    #[test]
    fn cell_caches_rendered_lines() {
        let cell = MarkdownCell::new("# Heading\n\nParagraph");
        let lines1 = cell.display_lines();
        let lines2 = cell.display_lines();
        assert_eq!(lines1.len(), lines2.len());
        for (a, b) in lines1.iter().zip(lines2.iter()) {
            assert_eq!(a.spans.len(), b.spans.len());
        }
    }
}
