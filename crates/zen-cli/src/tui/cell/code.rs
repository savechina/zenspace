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
    cached_lines: Vec<Line<'static>>,
}

impl CodeCell {
    pub fn new(lang: impl Into<String>, code: impl Into<String>, theme: &dyn OutputTheme) -> Self {
        let lang = lang.into();
        let code = code.into();
        let cached_lines = Self::render(&lang, &code, theme);
        Self {
            lang,
            code,
            border_style: theme.code_block_border(),
            lang_style: theme.code_block_lang(),
            cached_lines,
        }
    }

    fn render(lang: &str, code: &str, theme: &dyn OutputTheme) -> Vec<Line<'static>> {
        let mut lines = vec![Line::styled(
            format!("┌─ {} ", lang),
            theme.code_block_lang(),
        )];
        let highlighted = highlight::highlight_code(code, lang);
        lines.extend(highlighted);
        lines.push(Line::styled("└─", theme.code_block_border()));
        lines
    }

    pub fn display_lines(&self) -> &[Line<'static>] {
        &self.cached_lines
    }
}
