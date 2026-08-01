pub mod banner;
pub mod code;
pub mod error;
pub mod markdown;
pub mod plain;
pub mod streaming;

use crate::tui::theme::OutputTheme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::borrow::Cow;

pub use banner::BannerCell;
pub use code::CodeCell;
pub use error::ErrorCell;
pub use markdown::MarkdownCell;
pub use plain::PlainCell;
pub use streaming::StreamingCell;

#[derive(Debug, Clone)]
pub enum OutputCell {
    Banner(BannerCell),
    Markdown(MarkdownCell),
    Code(CodeCell),
    Error(ErrorCell),
    Streaming(StreamingCell),
    Plain(PlainCell),
    User {
        text: String,
    },
    Agent {
        text: String,
        reasoning: Option<String>,
        cached_text_lines: Vec<Line<'static>>,
        cached_reasoning_lines: Option<Vec<Line<'static>>>,
    },
    Separator {
        label: Option<String>,
    },
}

impl OutputCell {
    pub fn display_lines(
        &self,
        theme: &dyn OutputTheme,
        show_reasoning: bool,
    ) -> Cow<'_, [Line<'static>]> {
        match self {
            Self::Banner(b) => Cow::Borrowed(b.display_lines()),
            Self::Markdown(m) => Cow::Borrowed(m.display_lines()),
            Self::Code(c) => Cow::Borrowed(c.display_lines()),
            Self::Error(e) => Cow::Owned(e.display_lines()),
            Self::Streaming(s) => Cow::Owned(s.display_lines()),
            Self::Plain(p) => Cow::Owned(p.display_lines()),
            Self::User { text } => Cow::Owned(render_user_lines(text, theme)),
            Self::Agent {
                text: _,
                reasoning: _,
                cached_text_lines,
                cached_reasoning_lines,
            } => Cow::Owned(render_agent_lines(
                cached_text_lines,
                cached_reasoning_lines.as_deref(),
                theme,
                show_reasoning,
            )),
            Self::Separator { label } => {
                let content = match label {
                    Some(l) => format!("── {} ──", l),
                    None => "───".to_string(),
                };
                Cow::Owned(vec![Line::from(Span::styled(content, theme.separator()))])
            }
        }
    }

    pub fn raw_text(&self) -> String {
        match self {
            Self::Plain(p) => p.text.clone(),
            Self::Markdown(m) => m.content.clone(),
            Self::Code(c) => c.code.clone(),
            Self::Error(e) => e.message.clone(),
            Self::User { text } => text.clone(),
            Self::Agent { text, .. } => text.clone(),
            Self::Separator { label } => label.clone().unwrap_or_default(),
            Self::Banner(_) | Self::Streaming(_) => String::new(),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming(_))
    }
}

fn render_user_lines(text: &str, theme: &dyn OutputTheme) -> Vec<Line<'static>> {
    let bg = theme.user_bg();
    let prefix_style = theme.user_prefix();
    let text_style = Style::default().bg(bg);
    let prefix_bg = Style::default().bg(bg);

    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::with_capacity(lines.len().max(1));

    for (i, line) in lines.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if i == 0 {
            spans.push(Span::styled("› ".to_string(), prefix_style.bg(bg)));
        } else {
            spans.push(Span::styled("  ".to_string(), prefix_bg));
        }
        spans.push(Span::styled(line.to_string(), text_style));
        result.push(Line::from(spans));
    }

    if result.is_empty() {
        result.push(Line::from(Span::styled(
            "› ".to_string(),
            prefix_style.bg(bg),
        )));
    }

    result
}

fn render_agent_lines(
    cached_text_lines: &[Line<'static>],
    cached_reasoning_lines: Option<&[Line<'static>]>,
    theme: &dyn OutputTheme,
    show_reasoning: bool,
) -> Vec<Line<'static>> {
    let prefix_style = theme.agent_prefix();
    let mut result: Vec<Line<'static>> = Vec::new();

    if show_reasoning && let Some(r_lines) = cached_reasoning_lines {
        let reasoning_style = theme.text_muted().add_modifier(Modifier::ITALIC);
        result.push(Line::from(Span::styled("Thought", reasoning_style)));
        for rl in r_lines {
            let indented_spans: Vec<Span<'static>> = std::iter::once(Span::raw("  "))
                .chain(rl.spans.clone())
                .collect();
            result.push(Line::from(indented_spans));
        }
        result.push(Line::from(Span::raw("")));
    }

    for (i, line) in cached_text_lines.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if i == 0 {
            spans.push(Span::styled("• ".to_string(), prefix_style));
        } else {
            spans.push(Span::styled("  ".to_string(), Style::default()));
        }
        spans.extend(line.spans.clone());
        result.push(Line::from(spans));
    }
    result
}

impl From<OutputCell> for Vec<Line<'static>> {
    fn from(cell: OutputCell) -> Self {
        use crate::tui::theme::ZenTheme;
        cell.display_lines(&ZenTheme, false).into_owned()
    }
}

impl From<BannerCell> for OutputCell {
    fn from(cell: BannerCell) -> Self {
        Self::Banner(cell)
    }
}

impl From<MarkdownCell> for OutputCell {
    fn from(cell: MarkdownCell) -> Self {
        Self::Markdown(cell)
    }
}

impl From<CodeCell> for OutputCell {
    fn from(cell: CodeCell) -> Self {
        Self::Code(cell)
    }
}

impl From<ErrorCell> for OutputCell {
    fn from(cell: ErrorCell) -> Self {
        Self::Error(cell)
    }
}

impl From<StreamingCell> for OutputCell {
    fn from(cell: StreamingCell) -> Self {
        Self::Streaming(cell)
    }
}

impl From<PlainCell> for OutputCell {
    fn from(cell: PlainCell) -> Self {
        Self::Plain(cell)
    }
}

impl OutputCell {
    pub fn user(text: impl Into<String>) -> Self {
        Self::User { text: text.into() }
    }

    pub fn agent(text: impl Into<String>, reasoning: Option<String>) -> Self {
        use crate::tui::markdown::render_markdown;
        use crate::tui::render::normalize_compact_markdown;

        let text = text.into();
        let normalized_text = normalize_compact_markdown(&text);
        let cached_text_lines = render_markdown(&normalized_text);

        let (reasoning_norm, cached_reasoning_lines) = match reasoning {
            Some(r) => {
                let nr = normalize_compact_markdown(&r);
                let cached = render_markdown(&nr);
                (Some(nr), Some(cached))
            }
            None => (None, None),
        };

        Self::Agent {
            text: normalized_text,
            reasoning: reasoning_norm,
            cached_text_lines,
            cached_reasoning_lines,
        }
    }

    pub fn separator(label: impl Into<Option<String>>) -> Self {
        Self::Separator {
            label: label.into(),
        }
    }
}
