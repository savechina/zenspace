pub mod banner;
pub mod code;
pub mod error;
pub mod markdown;
pub mod plain;
pub mod streaming;

use ratatui::text::Line;

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
}

impl OutputCell {
    pub fn display_lines(&self) -> Vec<Line<'static>> {
        match self {
            Self::Banner(b) => b.display_lines(),
            Self::Markdown(m) => m.display_lines(),
            Self::Code(c) => c.display_lines(),
            Self::Error(e) => e.display_lines(),
            Self::Streaming(s) => s.display_lines(),
            Self::Plain(p) => p.display_lines(),
        }
    }

    pub fn raw_text(&self) -> String {
        match self {
            Self::Plain(p) => p.text.clone(),
            Self::Markdown(m) => m.content.clone(),
            Self::Code(c) => c.code.clone(),
            Self::Error(e) => e.message.clone(),
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

impl From<OutputCell> for Vec<Line<'static>> {
    fn from(cell: OutputCell) -> Self {
        cell.display_lines()
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
