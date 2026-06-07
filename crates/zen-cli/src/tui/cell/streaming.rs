use crate::tui::theme::OutputTheme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone)]
pub struct StreamingCell {
    pub buffer: String,
    pub buffer_style: Style,
    pub cursor_style: Style,
}

impl StreamingCell {
    pub fn new(buffer: impl Into<String>, theme: &dyn OutputTheme) -> Self {
        Self {
            buffer: buffer.into(),
            buffer_style: theme.text_muted().add_modifier(Modifier::ITALIC),
            cursor_style: theme.streaming_cursor(),
        }
    }

    pub fn display_lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        if !self.buffer.is_empty() {
            lines.push(Line::from(Span::styled(
                self.buffer.clone(),
                self.buffer_style,
            )));
        }

        lines.push(Line::from(Span::styled("▌", self.cursor_style)));

        lines
    }
}

impl From<String> for StreamingCell {
    fn from(s: String) -> Self {
        use crate::tui::theme::ZenTheme;
        Self::new(s, &ZenTheme)
    }
}

impl From<&str> for StreamingCell {
    fn from(s: &str) -> Self {
        use crate::tui::theme::ZenTheme;
        Self::new(s, &ZenTheme)
    }
}
