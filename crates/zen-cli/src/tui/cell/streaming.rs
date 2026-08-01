use crate::tui::theme::OutputTheme;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};

#[derive(Debug, Clone)]
pub struct StreamingCell {
    pub unstable_text: Text<'static>,
    pub cursor_style: Style,
}

impl StreamingCell {
    pub fn new(unstable_text: Text<'static>, theme: &dyn OutputTheme) -> Self {
        Self {
            unstable_text,
            cursor_style: theme.streaming_cursor(),
        }
    }

    pub fn display_lines(&self) -> Vec<Line<'static>> {
        let mut lines = self.unstable_text.lines.clone();

        if let Some(last) = lines.last_mut() {
            last.spans.push(Span::styled("▌", self.cursor_style));
        } else {
            lines.push(Line::from(Span::styled("▌", self.cursor_style)));
        }

        lines
    }
}
