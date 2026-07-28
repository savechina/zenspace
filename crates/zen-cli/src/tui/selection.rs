use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Character-level position in the rendered line list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPosition {
    /// Index in the rendered `all_lines` Vec built by `ui::build_all_lines`.
    pub line_idx: usize,
    /// Unicode scalar value offset within the line's text content.
    pub char_idx: usize,
}

impl TextPosition {
    pub fn new(line_idx: usize, char_idx: usize) -> Self {
        Self { line_idx, char_idx }
    }
}

/// Visual text selection with anchor and cursor endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: TextPosition,
    pub cursor: TextPosition,
}

impl Selection {
    pub fn new(anchor: TextPosition, cursor: TextPosition) -> Self {
        Self { anchor, cursor }
    }

    /// Normalized start (min) of the selection region.
    pub fn start(&self) -> TextPosition {
        if self.anchor.line_idx < self.cursor.line_idx
            || (self.anchor.line_idx == self.cursor.line_idx
                && self.anchor.char_idx <= self.cursor.char_idx)
        {
            self.anchor
        } else {
            self.cursor
        }
    }

    /// Normalized end (max) of the selection region.
    pub fn end(&self) -> TextPosition {
        if self.anchor.line_idx < self.cursor.line_idx
            || (self.anchor.line_idx == self.cursor.line_idx
                && self.anchor.char_idx <= self.cursor.char_idx)
        {
            self.cursor
        } else {
            self.anchor
        }
    }

    /// Returns true if the given line/char is within the selected region.
    #[allow(dead_code)]
    pub fn contains(&self, line_idx: usize, char_idx: usize) -> bool {
        let s = self.start();
        let e = self.end();
        if line_idx < s.line_idx || line_idx > e.line_idx {
            return false;
        }
        if line_idx == s.line_idx && line_idx == e.line_idx {
            return char_idx >= s.char_idx && char_idx < e.char_idx;
        }
        if line_idx == s.line_idx {
            return char_idx >= s.char_idx;
        }
        if line_idx == e.line_idx {
            return char_idx < e.char_idx;
        }
        true
    }

    /// Extract the plain-text content of the selected region from the rendered lines.
    pub fn selected_text(&self, all_lines: &[Line<'static>]) -> String {
        let s = self.start();
        let e = self.end();
        let mut result = String::new();

        for line_idx in s.line_idx..=e.line_idx {
            if let Some(line) = all_lines.get(line_idx) {
                let text = line_text(line);
                let chars: Vec<char> = text.chars().collect();
                let line_len = chars.len();

                let start_char = if line_idx == s.line_idx {
                    s.char_idx.min(line_len)
                } else {
                    0
                };
                let end_char = if line_idx == e.line_idx {
                    (e.char_idx).min(line_len)
                } else {
                    line_len
                };

                if start_char < end_char {
                    let slice: String = chars[start_char..end_char].iter().collect();
                    result.push_str(&slice);
                }

                if line_idx < e.line_idx {
                    result.push('\n');
                }
            }
        }

        result
    }
}

/// Extract the plain text content from a ratatui Line (concatenation of all spans).
pub fn line_text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Apply a highlight style to the selected portion of a single line.
///
/// Splits spans at selection boundaries using char-level indexing so multi-byte
/// characters (CJK, emoji) are handled correctly.
pub fn highlight_line(
    line: &Line<'static>,
    line_idx: usize,
    selection: &Selection,
    highlight: Style,
) -> Line<'static> {
    let s = selection.start();
    let e = selection.end();

    let (sel_start, sel_end) = if line_idx < s.line_idx || line_idx > e.line_idx {
        return line.clone();
    } else if line_idx == s.line_idx && line_idx == e.line_idx {
        (s.char_idx, e.char_idx)
    } else if line_idx == s.line_idx {
        (s.char_idx, usize::MAX)
    } else if line_idx == e.line_idx {
        (0, e.char_idx)
    } else {
        (0, usize::MAX)
    };

    let mut result_spans: Vec<Span<'static>> = Vec::new();
    let mut char_offset: usize = 0;

    for span in &line.spans {
        let span_text = span.content.as_ref();
        let span_len = span_text.chars().count();
        let span_end = char_offset + span_len;

        if span_end <= sel_start || char_offset >= sel_end {
            result_spans.push(span.clone());
        } else if char_offset >= sel_start && span_end <= sel_end {
            let merged = highlight
                .bg
                .or(span.style.bg)
                .map(|bg| {
                    Style::default()
                        .fg(
                            highlight
                                .fg
                                .or(span.style.fg)
                                .unwrap_or(ratatui::style::Color::Reset),
                        )
                        .bg(bg)
                })
                .unwrap_or(highlight);
            result_spans.push(Span::styled(span.content.clone(), merged));
        } else {
            let chars: Vec<char> = span_text.chars().collect();
            let local_start = sel_start.saturating_sub(char_offset);
            let local_end = sel_end.saturating_sub(char_offset).min(span_len);

            if local_start > 0 {
                let prefix: String = chars[..local_start].iter().collect();
                result_spans.push(Span::styled(prefix, span.style));
            }
            if local_start < local_end {
                let selected: String = chars[local_start..local_end].iter().collect();
                let merged = highlight
                    .bg
                    .or(span.style.bg)
                    .map(|bg| {
                        Style::default()
                            .fg(
                                highlight
                                    .fg
                                    .or(span.style.fg)
                                    .unwrap_or(ratatui::style::Color::Reset),
                            )
                            .bg(bg)
                    })
                    .unwrap_or(highlight);
                result_spans.push(Span::styled(selected, merged));
            }
            if local_end < span_len {
                let suffix: String = chars[local_end..].iter().collect();
                result_spans.push(Span::styled(suffix, span.style));
            }
        }

        char_offset = span_end;
    }

    Line::from(result_spans)
}

/// Compute a `TextPosition` from a character offset within the concatenated text
/// of all lines.
#[allow(dead_code)]
pub fn char_offset_to_position(
    all_lines: &[Line<'static>],
    target_offset: usize,
) -> TextPosition {
    let mut offset = 0usize;
    for (line_idx, line) in all_lines.iter().enumerate() {
        let text = line_text(line);
        let len = text.chars().count();
        if offset + len > target_offset {
            return TextPosition::new(line_idx, target_offset - offset);
        }
        offset += len;
    }
    let last_idx = all_lines.len().saturating_sub(1);
    TextPosition::new(last_idx, 0)
}

/// Map a (column, row) terminal coordinate to a `TextPosition` in the rendered
/// lines, accounting for scroll offset and the chat block border.
pub fn mouse_to_position(
    col: u16,
    row: u16,
    chat_area: ratatui::layout::Rect,
    scroll: usize,
    inner_width: usize,
    all_lines: &[Line<'static>],
) -> Option<TextPosition> {
    let border_width: u16 = 1;
    let inner_area_y = chat_area.y + border_width;

    if row <= chat_area.y || row >= chat_area.y + chat_area.height.saturating_sub(1) {
        return None;
    }
    if col <= chat_area.x || col >= chat_area.x + chat_area.width.saturating_sub(1) {
        return None;
    }

    let visual_row = (row - inner_area_y) as usize + scroll;
    let click_col = (col - chat_area.x - border_width) as usize;

    let mut visual_row_offset: usize = 0;
    for (line_idx, line) in all_lines.iter().enumerate() {
        let line_width = line.width();
        let wrapped_rows = if line_width == 0 || inner_width == 0 {
            1
        } else {
            line_width.div_ceil(inner_width)
        };

        if visual_row_offset + wrapped_rows > visual_row {
            let row_within_line = visual_row - visual_row_offset;
            let text = line_text(line);
            let mut col_offset: usize = 0;
            for (char_idx, ch) in text.chars().enumerate() {
                let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if row_within_line == 0 && col_offset + char_width > click_col {
                    return Some(TextPosition::new(line_idx, char_idx));
                }
                if row_within_line == 0 && col_offset + char_width == click_col {
                    return Some(TextPosition::new(line_idx, char_idx + 1));
                }
                col_offset += char_width;

                if col_offset >= inner_width && char_idx + 1 < text.chars().count() {
                    return Some(TextPosition::new(line_idx, char_idx + 1));
                }
            }
            return Some(TextPosition::new(line_idx, text.chars().count()));
        }
        visual_row_offset += wrapped_rows;
    }

    let last_idx = all_lines.len().saturating_sub(1);
    Some(TextPosition::new(last_idx, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn make_line(text: &str) -> Line<'static> {
        Line::raw(text.to_string())
    }

    fn make_styled_line(spans: Vec<(&str, Style)>) -> Line<'static> {
        Line::from(
            spans
                .into_iter()
                .map(|(t, s)| Span::styled(t.to_string(), s))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn selection_start_end_simple() {
        let sel = Selection::new(TextPosition::new(1, 5), TextPosition::new(1, 10));
        assert_eq!(sel.start(), TextPosition::new(1, 5));
        assert_eq!(sel.end(), TextPosition::new(1, 10));
    }

    #[test]
    fn selection_start_end_reversed() {
        let sel = Selection::new(TextPosition::new(2, 10), TextPosition::new(0, 3));
        assert_eq!(sel.start(), TextPosition::new(0, 3));
        assert_eq!(sel.end(), TextPosition::new(2, 10));
    }

    #[test]
    fn selection_start_end_multiline() {
        let sel = Selection::new(TextPosition::new(3, 0), TextPosition::new(1, 5));
        assert_eq!(sel.start(), TextPosition::new(1, 5));
        assert_eq!(sel.end(), TextPosition::new(3, 0));
    }

    #[test]
    fn contains_single_line() {
        let sel = Selection::new(TextPosition::new(0, 2), TextPosition::new(0, 5));
        assert!(!sel.contains(0, 1));
        assert!(sel.contains(0, 2));
        assert!(sel.contains(0, 4));
        assert!(!sel.contains(0, 5));
    }

    #[test]
    fn contains_multiline() {
        let sel = Selection::new(TextPosition::new(1, 3), TextPosition::new(3, 2));
        assert!(!sel.contains(0, 0));
        assert!(sel.contains(1, 3));
        assert!(sel.contains(1, 100));
        assert!(sel.contains(2, 0));
        assert!(sel.contains(3, 1));
        assert!(!sel.contains(3, 2));
        assert!(!sel.contains(4, 0));
    }

    #[test]
    fn selected_text_single_line() {
        let lines = vec![make_line("Hello, World!")];
        let sel = Selection::new(TextPosition::new(0, 7), TextPosition::new(0, 12));
        assert_eq!(sel.selected_text(&lines), "World");
    }

    #[test]
    fn selected_text_multiline() {
        let lines = vec![
            make_line("Line one"),
            make_line("Line two"),
            make_line("Line three"),
        ];
        let sel = Selection::new(TextPosition::new(0, 5), TextPosition::new(2, 4));
        let text = sel.selected_text(&lines);
        assert_eq!(text, "one\nLine two\nLine");
    }

    #[test]
    fn selected_text_full_line() {
        let lines = vec![make_line("Full line selected")];
        let sel = Selection::new(TextPosition::new(0, 0), TextPosition::new(0, 18));
        assert_eq!(sel.selected_text(&lines), "Full line selected");
    }

    #[test]
    fn highlight_line_single_span() {
        let line = make_line("Hello World");
        let sel = Selection::new(TextPosition::new(0, 6), TextPosition::new(0, 11));
        let highlight = Style::default().bg(Color::Blue);
        let result = highlight_line(&line, 0, &sel, highlight);
        assert_eq!(result.spans.len(), 2);
        assert_eq!(result.spans[0].content.as_ref(), "Hello ");
        assert_eq!(result.spans[1].content.as_ref(), "World");
        assert_eq!(result.spans[1].style.bg, Some(Color::Blue));
    }

    #[test]
    fn highlight_line_multi_span() {
        let line = make_styled_line(vec![
            ("Hello ", Style::default().fg(Color::Red)),
            ("World", Style::default().fg(Color::Green)),
        ]);
        let sel = Selection::new(TextPosition::new(0, 3), TextPosition::new(0, 8));
        let highlight = Style::default().bg(Color::Blue);
        let result = highlight_line(&line, 0, &sel, highlight);
        assert!(result.spans.len() >= 3);
    }

    #[test]
    fn highlight_line_outside_range() {
        let line = make_line("Hello");
        let sel = Selection::new(TextPosition::new(1, 0), TextPosition::new(1, 5));
        let highlight = Style::default().bg(Color::Blue);
        let result = highlight_line(&line, 0, &sel, highlight);
        assert_eq!(result.spans.len(), 1);
        assert_eq!(result.spans[0].content.as_ref(), "Hello");
    }

    #[test]
    fn highlight_line_entire_line() {
        let line = make_line("Select all");
        let sel = Selection::new(TextPosition::new(0, 0), TextPosition::new(0, 10));
        let highlight = Style::default().bg(Color::Blue);
        let result = highlight_line(&line, 0, &sel, highlight);
        assert_eq!(result.spans.len(), 1);
        assert_eq!(result.spans[0].content.as_ref(), "Select all");
        assert_eq!(result.spans[0].style.bg, Some(Color::Blue));
    }

    #[test]
    fn line_text_extraction() {
        let line = make_styled_line(vec![
            ("Hello ", Style::default()),
            ("World", Style::default()),
        ]);
        assert_eq!(line_text(&line), "Hello World");
    }

    #[test]
    fn char_offset_to_position_basic() {
        let lines = vec![make_line("Hello"), make_line("World")];
        let pos = char_offset_to_position(&lines, 7);
        assert_eq!(pos.line_idx, 1);
        assert_eq!(pos.char_idx, 2);
    }

    #[test]
    fn char_offset_to_position_past_end() {
        let lines = vec![make_line("Hi")];
        let pos = char_offset_to_position(&lines, 100);
        assert_eq!(pos.line_idx, 0);
        assert_eq!(pos.char_idx, 0);
    }

    #[test]
    fn selection_cjk_characters() {
        let lines = vec![make_line("你好世界测试")];
        let sel = Selection::new(TextPosition::new(0, 1), TextPosition::new(0, 3));
        assert_eq!(sel.selected_text(&lines), "好世");
    }

    #[test]
    fn selection_emoji() {
        let lines = vec![make_line("Hello 🌍🌎🌏")];
        let sel = Selection::new(TextPosition::new(0, 6), TextPosition::new(0, 8));
        assert_eq!(sel.selected_text(&lines), "🌍🌎");
    }
}
