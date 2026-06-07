use crate::tui::theme::OutputTheme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

#[derive(Debug, Clone)]
pub struct ErrorCell {
    pub message: String,
    pub style: Style,
}

impl ErrorCell {
    pub fn new(message: impl Into<String>, theme: &dyn OutputTheme) -> Self {
        Self {
            message: message.into(),
            style: theme.error(),
        }
    }

    pub fn display_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from(Span::styled(self.message.clone(), self.style))]
    }
}

impl From<String> for ErrorCell {
    fn from(s: String) -> Self {
        use crate::tui::theme::ZenTheme;
        Self::new(s, &ZenTheme)
    }
}

impl From<&str> for ErrorCell {
    fn from(s: &str) -> Self {
        use crate::tui::theme::ZenTheme;
        Self::new(s, &ZenTheme)
    }
}
