use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::OnceLock;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use two_face::syntax::extra_newlines;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

const MAX_HIGHLIGHT_BYTES: usize = 100_000;
const MAX_HIGHLIGHT_LINES: usize = 5_000;

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(extra_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let themes = ThemeSet::load_defaults();
        themes.themes["base16-ocean.dark"].clone()
    })
}

pub fn highlight_code(code: &str, lang: &str) -> Vec<Line<'static>> {
    if code.is_empty() {
        return vec![Line::raw(String::new())];
    }
    if code.len() > MAX_HIGHLIGHT_BYTES || code.lines().count() > MAX_HIGHLIGHT_LINES {
        return code.lines().map(|l| Line::raw(l.to_string())).collect();
    }

    let ss = syntax_set();
    let syntax = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme());
    let mut lines: Vec<Line<'static>> = Vec::new();

    for line in syntect::util::LinesWithEndings::from(code) {
        let ranges = highlighter.highlight_line(line, ss).unwrap_or_default();
        let spans: Vec<Span<'static>> = ranges
            .into_iter()
            .filter_map(|(style, text)| {
                let text = text.trim_end_matches(['\n', '\r']).to_string();
                if text.is_empty() {
                    None
                } else {
                    Some(Span::styled(text, convert_style(style)))
                }
            })
            .collect();

        if spans.is_empty() {
            lines.push(Line::raw(String::new()));
        } else {
            lines.push(Line::from(spans));
        }
    }

    lines
}

fn convert_style(style: syntect::highlighting::Style) -> Style {
    let fg = convert_color(style.foreground);
    let mut modifiers = Modifier::empty();
    if style.font_style.contains(FontStyle::BOLD) {
        modifiers |= Modifier::BOLD;
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        modifiers |= Modifier::ITALIC;
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        modifiers |= Modifier::UNDERLINED;
    }
    match fg {
        Some(color) => Style::default().fg(color).add_modifier(modifiers),
        None => Style::default().add_modifier(modifiers),
    }
}

fn convert_color(color: syntect::highlighting::Color) -> Option<Color> {
    match color.a {
        0x00 => Some(ansi_palette_color(color.r)),
        0x01 => None,
        _ => Some(Color::Rgb(color.r, color.g, color.b)),
    }
}

fn ansi_palette_color(index: u8) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::DarkGray,
        8 => Color::Black,
        9 => Color::Red,
        10 => Color::Green,
        11 => Color::Yellow,
        12 => Color::Blue,
        13 => Color::Magenta,
        14 => Color::Cyan,
        15 => Color::White,
        _ => Color::White,
    }
}
