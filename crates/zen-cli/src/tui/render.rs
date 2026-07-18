use ratatui::style::{Color, Style};
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

pub fn render_markdown_with_thoughts(content: &str) -> Vec<Line<'static>> {
    let thought_open = "<think>";
    let thought_close = "</think>";

    if !content.contains(thought_open) && !content.contains(thought_close) {
        return render_markdown_to_lines(content);
    }

    let mut all_lines: Vec<Line<'static>> = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        if let Some(open_pos) = remaining.find(thought_open) {
            if open_pos > 0 {
                let before = &remaining[..open_pos];
                all_lines.extend(render_markdown_to_lines(before));
            }

            let after_open = &remaining[open_pos + thought_open.len()..];
            if let Some(close_pos) = after_open.find(thought_close) {
                let thought_content = &after_open[..close_pos];
                let dimmed_style = Style::default().fg(Color::DarkGray);
                let thought_lines = render_markdown_to_lines(thought_content);
                for line in thought_lines {
                    let dimmed_spans: Vec<Span<'static>> = line
                        .spans
                        .into_iter()
                        .map(|span| Span::styled(span.content.clone(), dimmed_style))
                        .collect();
                    all_lines.push(Line::from(dimmed_spans));
                }

                remaining = &after_open[close_pos + thought_close.len()..];
            } else {
                let dimmed_style = Style::default().fg(Color::DarkGray);
                let rest_lines = render_markdown_to_lines(after_open);
                for line in rest_lines {
                    let dimmed_spans: Vec<Span<'static>> = line
                        .spans
                        .into_iter()
                        .map(|span| Span::styled(span.content.clone(), dimmed_style))
                        .collect();
                    all_lines.push(Line::from(dimmed_spans));
                }
                break;
            }
        } else {
            all_lines.extend(render_markdown_to_lines(remaining));
            break;
        }
    }

    all_lines
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

    #[test]
    fn thought_tags_render_dimmed() {
        let content = "Before <think>thinking content </think> After";
        let lines = render_markdown_with_thoughts(content);
        assert!(!lines.is_empty());
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("Before"));
        assert!(all_text.contains("thinking content"));
        assert!(all_text.contains("After"));
        assert!(!all_text.contains("<think>"));
        assert!(!all_text.contains("</think>"));
    }

    #[test]
    fn no_thought_tags_renders_normally() {
        let content = "Normal **markdown** content";
        let lines = render_markdown_with_thoughts(content);
        let normal_lines = render_markdown_to_lines(content);
        assert_eq!(lines.len(), normal_lines.len());
    }
}
