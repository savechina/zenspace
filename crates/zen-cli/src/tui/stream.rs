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
    // How much of the current text/reasoning split has already been fed to the
    // incremental renderers. MdStream::append ACCUMULATES, so each render must
    // feed only the newly-arrived suffix (see split_render_opt).
    text_fed_len: usize,
    reasoning_fed_len: usize,
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
            text_fed_len: 0,
            reasoning_fed_len: 0,
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
    /// viewport, honoring the `/thinking` toggle (T063/G4): when
    /// `show_thinking` is false, reasoning blocks are omitted from BOTH the
    /// committed and pending regions (they are stripped from the final
    /// response anyway) instead of flashing through the viewport.
    /// `show_thinking` is false, reasoning blocks are omitted from BOTH the
    /// committed and pending regions (they are stripped from the final
    /// response anyway) instead of flashing through the viewport.
    pub fn drain_and_tail_filtered(
        &mut self,
        reasoning_style: Style,
        show_thinking: bool,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        self.split_render_opt(reasoning_style, show_thinking)
    }

    fn split_render(&mut self, reasoning_style: Style) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        self.split_render_opt(reasoning_style, true)
    }

    fn split_render_opt(
        &mut self,
        reasoning_style: Style,
        show_thinking: bool,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        if self.buffer.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let (text, reasoning, in_think) = self.get_or_compute_split(&self.buffer.clone());
        self.reasoning_active = in_think;

        // MdStream::append ACCUMULATES its input; feed only the suffix that
        // arrived since the last render, or every already-committed block gets
        // re-emitted (duplicate scrollback output). The split can transiently
        // shrink while a `<think>`/`</think>` tag is mid-arrival, so clamp the
        // slice instead of panicking on an out-of-range index.
        let text_delta = text.get(self.text_fed_len..).unwrap_or("");
        self.text_fed_len = text.len();
        let reasoning_delta = if show_thinking {
            let delta = reasoning.get(self.reasoning_fed_len..).unwrap_or("");
            self.reasoning_fed_len = reasoning.len();
            delta
        } else {
            ""
        };

        let mut committed: Vec<Line<'static>> = Vec::new();
        let mut pending: Vec<Line<'static>> = Vec::new();

        if !reasoning.is_empty() && show_thinking {
            let header_text = if in_think { "Thinking..." } else { "Thought" };
            let header = Line::from(Span::styled(header_text, reasoning_style));

            let reasoning_update = self.reasoning_renderer.append(reasoning_delta);

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
            let text_update = self.text_renderer.append(text_delta);
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
        self.text_fed_len = 0;
        self.reasoning_fed_len = 0;
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
        self.text_fed_len = 0;
        self.reasoning_fed_len = 0;
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
    fn filtered_mode_hides_reasoning() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>secret reasoning</think>Answer text");
        let (committed, pending) = c.drain_and_tail_filtered(Style::default(), false);
        let all: String = committed
            .iter()
            .chain(pending.iter())
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all.contains("Answer text"), "text must render: {all:?}");
        assert!(
            !all.contains("secret reasoning"),
            "reasoning must be hidden: {all:?}"
        );
        assert!(
            !all.contains("Thinking"),
            "reasoning header must be hidden: {all:?}"
        );
    }

    #[test]
    fn filtered_mode_shows_reasoning_when_enabled() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>visible reasoning</think>Answer");
        let (committed, pending) = c.drain_and_tail_filtered(Style::default(), true);
        let all: String = committed
            .iter()
            .chain(pending.iter())
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            all.contains("visible reasoning"),
            "reasoning must render: {all:?}"
        );
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

    fn lines_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect()
    }

    #[test]
    fn drain_does_not_reemit_committed_blocks() {
        // Regression: split_render_opt used to feed the FULL accumulated text
        // to MdStream::append on every tick. MdStream appends (never replaces),
        // so a second drain with no new content doubled the pending tail (and
        // re-committed any committed blocks), duplicating scrollback output.
        let mut collector = StreamCollector::new();
        collector.push_delta("Hello world");
        let (c1, p1) = collector.drain_and_tail_filtered(Style::default(), true);
        let first = lines_text(&c1) + &lines_text(&p1);
        assert!(first.contains("Hello world"));

        // No new content: the output must be identical (idempotent), not doubled.
        let (c2, p2) = collector.drain_and_tail_filtered(Style::default(), true);
        let second = lines_text(&c2) + &lines_text(&p2);
        assert_eq!(
            first, second,
            "drain without new content must be idempotent"
        );

        // New content: prior text must appear exactly once, not re-emitted.
        collector.push_delta(" Second");
        let (c3, p3) = collector.drain_and_tail_filtered(Style::default(), true);
        let third = lines_text(&c3) + &lines_text(&p3);
        assert!(third.contains("Second"));
        assert_eq!(
            third.matches("Hello world").count(),
            1,
            "prior text re-emitted: {third:?}"
        );
    }
}
