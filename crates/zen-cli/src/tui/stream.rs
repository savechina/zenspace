use super::render::render_markdown_with_thoughts;
use ratatui::text::Line;

pub struct MarkdownStreamCollector {
    buffer: String,
    committed_chars: usize,
    committed_lines: Vec<Line<'static>>,
}

impl MarkdownStreamCollector {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            committed_chars: 0,
            committed_lines: Vec::new(),
        }
    }

    pub fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    pub fn commit_complete_lines(&mut self) -> (Vec<Line<'static>>, String) {
        let last_newline = self.buffer.rfind('\n');

        let renderable_end = match last_newline {
            Some(idx) => idx + 1,
            None => return (Vec::new(), String::new()),
        };

        if renderable_end <= self.committed_chars {
            return (Vec::new(), String::new());
        }

        let renderable = &self.buffer[..renderable_end];
        let all_lines = render_markdown_with_thoughts(renderable);

        let old_line_count = self.committed_lines.len();
        if all_lines.len() > old_line_count {
            let new_lines = all_lines[old_line_count..].to_vec();
            self.committed_lines = all_lines;
            self.committed_chars = renderable_end;
            (new_lines, renderable.to_string())
        } else {
            (Vec::new(), String::new())
        }
    }

    pub fn finalize_and_drain(&mut self) -> (Vec<Line<'static>>, String) {
        let mut source = self.buffer.clone();
        if !source.ends_with('\n') {
            source.push('\n');
        }

        let all_lines = render_markdown_with_thoughts(&source);
        let old_line_count = self.committed_lines.len();
        let out: Vec<Line<'static>> = if all_lines.len() > old_line_count {
            all_lines[old_line_count..].to_vec()
        } else {
            all_lines
        };
        let raw_text = source.clone();

        self.clear();
        (out, raw_text)
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.committed_chars = 0;
        self.committed_lines.clear();
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
