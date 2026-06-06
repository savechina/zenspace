use crate::tui::render::render_markdown_to_lines;
use ratatui::text::Line;
use std::cell::OnceCell;

#[derive(Debug, Clone)]
pub struct MarkdownCell {
    pub content: String,
    rendered: OnceCell<Vec<Line<'static>>>,
}

impl MarkdownCell {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            rendered: OnceCell::new(),
        }
    }

    pub fn from_lines(lines: Vec<Line<'static>>, raw_text: String) -> Self {
        let cell = Self {
            content: raw_text,
            rendered: OnceCell::new(),
        };
        let _ = cell.rendered.set(lines);
        cell
    }

    pub fn display_lines(&self) -> Vec<Line<'static>> {
        self.rendered
            .get_or_init(|| render_markdown_to_lines(&self.content))
            .clone()
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
    fn new_cell_has_no_cached_render() {
        let cell = MarkdownCell::new("# Hello\n\nWorld");
        assert!(
            cell.rendered.get().is_none(),
            "new cell should not have cached render"
        );
    }

    #[test]
    fn display_lines_caches_result() {
        let cell = MarkdownCell::new("# Hello\n\nWorld");
        let first = cell.display_lines();
        let second = cell.display_lines();
        assert_eq!(
            first.len(),
            second.len(),
            "repeated display should return same length"
        );
        assert!(
            cell.rendered.get().is_some(),
            "after display, cache should be populated"
        );
    }

    #[test]
    fn from_lines_uses_pre_rendered() {
        let pre = vec![Line::raw("pre-rendered")];
        let cell = MarkdownCell::from_lines(pre.clone(), "raw content".into());
        let displayed = cell.display_lines();
        assert_eq!(displayed.len(), 1);
        let text: String = displayed[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "pre-rendered");
    }

    #[test]
    fn new_cell_renders_on_display() {
        let cell = MarkdownCell::new("# Heading\n\nParagraph");
        let displayed = cell.display_lines();
        assert!(
            displayed.len() >= 2,
            "heading + paragraph should render as 2+ lines"
        );
    }
}
