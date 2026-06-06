use ratatui::style::{Color, Modifier, Style};

pub trait OutputTheme {
    fn heading(&self, level: u8) -> Style;
    fn bold(&self) -> Style;
    fn italic(&self) -> Style;
    fn code_inline(&self) -> Style;
    fn code_block_border(&self) -> Style;
    fn code_block_lang(&self) -> Style;
    fn table_border(&self) -> Style;
    fn table_header(&self) -> Style;
    fn blockquote(&self) -> Style;
    fn list_bullet(&self) -> Style;
    fn link(&self) -> Style;
    fn error(&self) -> Style;
    fn streaming_cursor(&self) -> Style;
}

pub struct DefaultTheme;

impl OutputTheme for DefaultTheme {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            2 => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        }
    }

    fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default().add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().fg(Color::Cyan)
    }

    fn code_block_border(&self) -> Style {
        Style::default().fg(Color::DarkGray)
    }

    fn code_block_lang(&self) -> Style {
        Style::default().fg(Color::Yellow)
    }

    fn table_border(&self) -> Style {
        Style::default().fg(Color::DarkGray)
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Green)
    }

    fn list_bullet(&self) -> Style {
        Style::default()
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().fg(Color::Red)
    }

    fn streaming_cursor(&self) -> Style {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    }
}

impl Default for DefaultTheme {
    fn default() -> Self {
        Self
    }
}
