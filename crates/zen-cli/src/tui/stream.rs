use crate::tui::markdown::StreamingMarkdown;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

pub struct StreamCollector {
    buffer: String,
    text_renderer: StreamingMarkdown,
    reasoning_renderer: StreamingMarkdown,
    reasoning_active: bool,
    buffer_changed_since_render: bool,
    last_reasoning_split: Option<(String, String, bool)>,
}

impl StreamCollector {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            text_renderer: StreamingMarkdown::new(),
            reasoning_renderer: StreamingMarkdown::new(),
            reasoning_active: false,
            buffer_changed_since_render: false,
            last_reasoning_split: None,
        }
    }

    pub fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
        self.buffer_changed_since_render = true;
    }

    pub(crate) fn buffer(&self) -> &str {
        &self.buffer
    }

    #[allow(dead_code)]
    pub fn buffer_changed(&self) -> bool {
        self.buffer_changed_since_render
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
            } else if let Some(end) = remaining.find("</think>") {
                reasoning.push_str(&remaining[..end]);
                remaining = &remaining[end + 8..];
                in_think = false;
            } else {
                reasoning.push_str(remaining);
                break;
            }
        }

        (text, reasoning, in_think)
    }

    fn get_or_compute_split(&mut self, buffer: &str) -> (String, String, bool) {
        if self.buffer_changed_since_render || self.last_reasoning_split.is_none() {
            let result = Self::split_reasoning(buffer);
            self.last_reasoning_split = Some(result.clone());
            self.buffer_changed_since_render = false;
            result
        } else {
            self.last_reasoning_split.clone().unwrap()
        }
    }

    /// Render the entire current streaming state into lines.
    pub fn render(&mut self, reasoning_style: Style) -> Vec<Line<'static>> {
        let (committed, pending) = self.split_render(reasoning_style);
        let mut lines = committed;
        lines.extend(pending);
        lines
    }

    /// Drain newly-committed lines that can be moved into terminal scrollback,
    /// and return the remaining pending tail that should stay in the inline
    /// viewport.
    pub fn drain_and_tail(
        &mut self,
        reasoning_style: Style,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        self.split_render(reasoning_style)
    }

    fn split_render(&mut self, reasoning_style: Style) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        if self.buffer.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let (text, reasoning, in_think) = self.get_or_compute_split(&self.buffer.clone());
        self.reasoning_active = in_think;

        let mut committed: Vec<Line<'static>> = Vec::new();
        let mut pending: Vec<Line<'static>> = Vec::new();

        if !reasoning.is_empty() {
            let header_text = if in_think { "Thinking..." } else { "Thought" };
            let header = Line::from(Span::styled(header_text, reasoning_style));

            let reasoning_update = self.reasoning_renderer.append(&reasoning);

            let reasoning_committed: Vec<Line<'static>> = reasoning_update
                .committed
                .iter()
                .flat_map(|b| b.lines.iter().cloned())
                .map(|l| Self::indent_line(l))
                .collect();

            if !reasoning_committed.is_empty() {
                committed.push(header.clone());
                committed.extend(reasoning_committed);
            }

            if let Some(pending_block) = &reasoning_update.pending {
                if committed.is_empty() {
                    pending.push(header);
                }
                for rl in &pending_block.lines {
                    pending.push(Self::indent_line(rl.clone()));
                }
            }

            if !in_think && (!committed.is_empty() || !pending.is_empty()) {
                let target = if pending.is_empty() {
                    &mut committed
                } else {
                    &mut pending
                };
                target.push(Line::from(Span::raw("")));
            }
        }

        if !text.is_empty() {
            let text_update = self.text_renderer.append(&text);
            for block in &text_update.committed {
                committed.extend(block.lines.iter().cloned());
            }
            if let Some(pending_block) = &text_update.pending {
                pending.extend(pending_block.lines.iter().cloned());
            }
        }

        (committed, pending)
    }

    fn indent_line(line: Line<'static>) -> Line<'static> {
        let mut spans = vec![Span::raw("  ")];
        spans.extend(line.spans);
        Line::from(spans)
    }

    pub fn finalize_and_drain(&mut self) -> (String, Option<String>) {
        let raw = std::mem::take(&mut self.buffer);
        self.text_renderer.clear();
        self.reasoning_renderer.clear();
        self.reasoning_active = false;
        self.buffer_changed_since_render = false;
        self.last_reasoning_split = None;
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
        self.buffer_changed_since_render = false;
        self.last_reasoning_split = None;
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

    #[test]
    fn buffer_changed_flag_set_on_push() {
        let mut collector = StreamCollector::new();
        assert!(!collector.buffer_changed());
        collector.push_delta("test");
        assert!(collector.buffer_changed());
    }

    #[test]
    fn buffer_changed_flag_cleared_on_render() {
        let mut collector = StreamCollector::new();
        collector.push_delta("test");
        assert!(collector.buffer_changed());
        collector.render(Style::default());
        assert!(!collector.buffer_changed());
    }
}
