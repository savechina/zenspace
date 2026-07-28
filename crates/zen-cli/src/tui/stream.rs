use crate::tui::markdown::IncrementalMarkdownRenderer;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

pub struct StreamCollector {
    buffer: String,
    text_renderer: IncrementalMarkdownRenderer,
    reasoning_renderer: IncrementalMarkdownRenderer,
    reasoning_active: bool,
}

impl StreamCollector {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            text_renderer: IncrementalMarkdownRenderer::new(),
            reasoning_renderer: IncrementalMarkdownRenderer::new(),
            reasoning_active: false,
        }
    }

    pub fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    fn split_reasoning(content: &str) -> (String, String, bool) {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut remaining = content;
        let mut in_think = false;

        while !remaining.is_empty() {
            if !in_think {
                if let Some(start) = remaining.find("<think>") {
                    text.push_str(&remaining[..start]);
                    remaining = &remaining[start + 7..];
                    in_think = true;
                } else {
                    text.push_str(remaining);
                    break;
                }
            } else {
                if let Some(end) = remaining.find("</think>") {
                    reasoning.push_str(&remaining[..end]);
                    remaining = &remaining[end + 8..];
                    in_think = false;
                } else {
                    reasoning.push_str(remaining);
                    break;
                }
            }
        }

        (text, reasoning, in_think)
    }

    pub fn render(&mut self, reasoning_style: Style) -> Vec<Line<'static>> {
        if self.buffer.is_empty() {
            return Vec::new();
        }

        let (text, reasoning, in_think) = Self::split_reasoning(&self.buffer);
        self.reasoning_active = in_think;

        let mut lines: Vec<Line<'static>> = Vec::new();

        if !reasoning.is_empty() {
            let header_text = if in_think {
                "Thinking...".to_string()
            } else {
                "Thought".to_string()
            };
            lines.push(Line::from(Span::styled(
                header_text,
                reasoning_style,
            )));

            let reasoning_lines = self.reasoning_renderer.update(&reasoning);
            for rl in reasoning_lines {
                let indented_spans: Vec<Span<'static>> = std::iter::once(Span::raw("  "))
                    .chain(rl.spans)
                    .collect();
                lines.push(Line::from(indented_spans));
            }

            if !in_think {
                lines.push(Line::from(Span::raw("")));
            }
        }

        if !text.is_empty() {
            let text_lines = self.text_renderer.update(&text);
            lines.extend(text_lines);
        }

        lines
    }

    pub fn finalize_and_drain(&mut self) -> (String, Option<String>) {
        let raw = std::mem::take(&mut self.buffer);
        self.text_renderer.clear();
        self.reasoning_renderer.clear();
        self.reasoning_active = false;
        let (text, reasoning, _) = Self::split_reasoning(&raw);
        let reasoning = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };
        (text, reasoning)
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.text_renderer.clear();
        self.reasoning_renderer.clear();
        self.reasoning_active = false;
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    #[allow(dead_code)]
    pub fn is_reasoning_active(&self) -> bool {
        self.reasoning_active
    }
}

impl Default for StreamCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_plain_text() {
        let (text, reasoning, in_think) = StreamCollector::split_reasoning("Hello world");
        assert_eq!(text, "Hello world");
        assert!(reasoning.is_empty());
        assert!(!in_think);
    }

    #[test]
    fn split_closed_think_tag() {
        let content = "<think>Let me reason</think>Here is the answer.";
        let (text, reasoning, in_think) = StreamCollector::split_reasoning(content);
        assert_eq!(text, "Here is the answer.");
        assert_eq!(reasoning, "Let me reason");
        assert!(!in_think);
    }

    #[test]
    fn split_open_think_tag() {
        let content = "<think>Still thinking";
        let (text, reasoning, in_think) = StreamCollector::split_reasoning(content);
        assert!(text.is_empty());
        assert_eq!(reasoning, "Still thinking");
        assert!(in_think);
    }

    #[test]
    fn split_multiple_think_blocks() {
        let content = "<think>First</think>Answer1<think>Second</think>Answer2";
        let (text, reasoning, in_think) = StreamCollector::split_reasoning(content);
        assert_eq!(text, "Answer1Answer2");
        assert_eq!(reasoning, "FirstSecond");
        assert!(!in_think);
    }

    #[test]
    fn collector_renders_text() {
        let mut collector = StreamCollector::new();
        collector.push_delta("Hello ");
        collector.push_delta("world");
        let lines = collector.render(Style::default());
        assert!(!lines.is_empty());
    }

    #[test]
    fn collector_renders_reasoning_then_text() {
        let mut collector = StreamCollector::new();
        collector.push_delta("<think>Reasoning here</think>Answer here");
        let style = Style::default();
        let lines = collector.render(style);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains("Thinking") || s.content.contains("Thought"))
        }));
    }

    #[test]
    fn collector_tracks_reasoning_active() {
        let mut collector = StreamCollector::new();
        collector.push_delta("<think>Still going");
        let style = Style::default();
        collector.render(style);
        assert!(collector.is_reasoning_active());

        collector.push_delta("</think>Done");
        collector.render(style);
        assert!(!collector.is_reasoning_active());
    }

    #[test]
    fn finalize_strips_think_tags() {
        let mut collector = StreamCollector::new();
        collector.push_delta("<think>secret reasoning</think>visible answer");
        let (finalized, reasoning) = collector.finalize_and_drain();
        assert_eq!(finalized, "visible answer");
        assert!(!finalized.contains("<think>"));
        assert!(!finalized.contains("</think>"));
        assert_eq!(reasoning.as_deref(), Some("secret reasoning"));
    }

    #[test]
    fn finalize_stips_multiple_think_blocks() {
        let mut collector = StreamCollector::new();
        collector.push_delta("<think>part1</think>text1<think>part2</think>text2");
        let (finalized, reasoning) = collector.finalize_and_drain();
        assert_eq!(finalized, "text1text2");
        assert_eq!(reasoning.as_deref(), Some("part1part2"));
    }

    #[test]
    fn finalize_no_reasoning_returns_none() {
        let mut collector = StreamCollector::new();
        collector.push_delta("just plain text");
        let (finalized, reasoning) = collector.finalize_and_drain();
        assert_eq!(finalized, "just plain text");
        assert!(reasoning.is_none());
    }
}
