use crate::tui::highlight;
use crate::tui::theme::OutputTheme;
use ratatui::style::Style;
use ratatui::text::Line;

#[derive(Debug, Clone)]
pub struct CodeCell {
    pub lang: String,
    pub code: String,
    pub border_style: Style,
    pub lang_style: Style,
}

impl CodeCell {
    pub fn new(lang: impl Into<String>, code: impl Into<String>, theme: &dyn OutputTheme) -> Self {
        Self {
            lang: lang.into(),
            code: code.into(),
            border_style: theme.code_block_border(),
            lang_style: theme.code_block_lang(),
        }
    }

    pub fn display_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::styled(format!("┌─ {} ", self.lang), self.lang_style)];
        let highlighted = highlight::highlight_code(&self.code, &self.lang);
        lines.extend(highlighted);
        lines.push(Line::styled("└─", self.border_style));
        lines
    }
}
