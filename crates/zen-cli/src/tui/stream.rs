use super::render::render_markdown_with_thoughts;
use ratatui::text::Line;

pub struct MarkdownStreamCollector {
    buffer: String,
    committed_line_count: usize,
}

impl MarkdownStreamCollector {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            committed_line_count: 0,
        }
    }

    pub fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    pub fn commit_complete_lines(&mut self) -> (Vec<Line<'static>>, String) {
        let source = self.buffer.clone();
        let last_newline = source.rfind('\n');

        let renderable = if let Some(idx) = last_newline {
            source[..=idx].to_string()
        } else {
            return (Vec::new(), String::new());
        };

        let lines = render_markdown_with_thoughts(&renderable);
        let complete_count = lines.len();

        if self.committed_line_count >= complete_count {
            return (Vec::new(), String::new());
        }

        let out: Vec<Line<'static>> = lines[self.committed_line_count..complete_count].to_vec();
        let raw_text = renderable.clone();

        self.committed_line_count = complete_count;
        (out, raw_text)
    }

    pub fn finalize_and_drain(&mut self) -> (Vec<Line<'static>>, String) {
        let mut source = self.buffer.clone();
        if !source.ends_with('\n') {
            source.push('\n');
        }

        let lines = render_markdown_with_thoughts(&source);
        let out = if self.committed_line_count >= lines.len() {
            Vec::new()
        } else {
            lines[self.committed_line_count..].to_vec()
        };
        let raw_text = source.clone();

        self.clear();
        (out, raw_text)
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.committed_line_count = 0;
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    #[allow(dead_code)]
    pub fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

impl Default for MarkdownStreamCollector {
    fn default() -> Self {
        Self::new()
    }
}
