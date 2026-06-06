use ratatui::text::{Line, Span};

pub fn render_markdown_to_lines(content: &str) -> Vec<Line<'static>> {
    let text = tui_markdown::from_str(content);
    text.lines
        .into_iter()
        .map(|line| {
            Line::from(
                line.spans
                    .into_iter()
                    .map(|span| Span::styled(span.content.to_string(), span.style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

pub fn normalize_compact_markdown(content: &str) -> String {
    if content.contains("\n\n") || !content.contains(' ') {
        return content.to_string();
    }
    let s = content.replace(" #", "\n\n#");
    let s = s.replace(" ```", "\n\n```");
    let s = s.replace(" - ", "\n- ");
    s.replace(" | ", "\n| ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_markdown_renders_multi_line() {
        let md = "# Introduction\n\nRust is a **bold** text.\n\n## Hello\n\n- Item 1\n- Item 2";
        let lines = render_markdown_to_lines(md);
        assert!(
            lines.len() >= 5,
            "expected multi-line render, got {} lines",
            lines.len()
        );
        let first: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(
            first.contains("Introduction"),
            "heading should be present, got {:?}",
            first
        );
    }

    #[test]
    fn single_line_no_newlines_renders_as_one() {
        let md = "# Title body text";
        let lines = render_markdown_to_lines(md);
        assert_eq!(
            lines.len(),
            1,
            "single-line input should produce single output line"
        );
    }

    #[test]
    fn normalize_compact_markdown_inserts_blank_lines() {
        let compact = "Some prose. # Heading more text. ```rust fn main() {} ``` - item";
        let normalized = normalize_compact_markdown(compact);
        assert!(
            normalized.contains("\n\n#"),
            "should insert blank line before heading"
        );
        assert!(
            normalized.contains("\n\n```"),
            "should insert blank line before code fence"
        );
        assert!(
            normalized.contains("\n- "),
            "should insert newline before list item"
        );
    }

    #[test]
    fn normalize_compact_preserves_already_spaced() {
        let good = "# Title\n\nBody text\n\n## Heading\n\n- item";
        let normalized = normalize_compact_markdown(good);
        assert_eq!(
            normalized, good,
            "already-spaced markdown should be unchanged"
        );
    }

    #[test]
    fn normalize_compact_skips_no_spaces() {
        let bare = "singleword";
        let normalized = normalize_compact_markdown(bare);
        assert_eq!(normalized, bare, "no-space content should be unchanged");
    }
}
