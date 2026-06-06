use ratatui::text::Line;

#[derive(Debug, Clone)]
pub struct PlainCell {
    pub text: String,
}

impl PlainCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    pub fn display_lines(&self) -> Vec<Line<'static>> {
        self.text
            .lines()
            .map(|l| Line::raw(l.to_string()))
            .collect()
    }
}

impl From<String> for PlainCell {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for PlainCell {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}
