use std::time::Instant;

use crate::tui::markdown::StreamingMarkdown;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use zen_gateway::server::hosting::{ToolIntermediate, split_tool_intermediates};

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
    // T057: tool intermediates parsed out of the 🔧/✅ callback stream.
    // `rendered_tools` is the drain watermark (how many blocks have already
    // been emitted into committed scrollback); `tools_expanded` is the user
    // preference toggled via /tools.
    tool_blocks: Vec<ToolIntermediate>,
    rendered_tools: usize,
    tools_expanded: bool,
    // W3: When reasoning first appeared in this turn. Used for the elapsed
    // timer in the `⏳ Thinking… (Ns)` / `✓ Thought for Ns` headers.
    reasoning_started_at: Option<Instant>,
    // W3: Throttle timer repaints — only redraw when the displayed second
    // changes (avoids busy-looping at 30fps just for the counter).
    last_rendered_think_secs: Option<u64>,
    // BUG-1: Guard to ensure the thinking summary is committed exactly once.
    // Without this, each drain after think-close would re-push the ✓ line.
    thinking_summary_committed: bool,
}

/// Maximum reasoning lines kept in the pending tail viewport (header + N lines).
/// Must stay ≤4 total (header + 3 body lines) to fit the 2-4 row tail viewport.
const THINKING_BODY_MAX_LINES: usize = 3;

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
            tool_blocks: Vec::new(),
            rendered_tools: 0,
            tools_expanded: false,
            reasoning_started_at: None,
            last_rendered_think_secs: None,
            thinking_summary_committed: false,
        }
    }

    pub fn push_delta(&mut self, delta: &str) {
        let (text, tool_events) = split_tool_intermediates(delta);
        if !tool_events.is_empty() {
            self.tool_blocks.extend(tool_events);
            self.buffer_changed_since_render = true;
        }
        if !text.is_empty() {
            self.buffer.push_str(&text);
            self.buffer_changed_since_render = true;
        }
    }

    /// Toggles expanded rendering of tool-intermediate blocks (`/tools`).
    /// Affects blocks not yet drained into scrollback; already-flushed
    /// entries keep the form they had (same lifecycle as the delta
    /// pipeline's committed region).
    pub fn toggle_tools_expanded(&mut self) {
        self.tools_expanded = !self.tools_expanded;
    }

    pub fn tools_expanded(&self) -> bool {
        self.tools_expanded
    }

    /// Drains rendered lines for blocks the watermark has not emitted
    /// yet (completion flush; fullscreen never drains) and forgets them.
    pub fn take_tool_lines(&mut self) -> Vec<Line<'static>> {
        let expanded = self.tools_expanded;
        let start = self.rendered_tools.min(self.tool_blocks.len());
        let lines: Vec<Line<'static>> = self.tool_blocks[start..]
            .iter()
            .flat_map(|block| Self::tool_block_lines(block, expanded))
            .collect();
        self.rendered_tools = self.tool_blocks.len();
        lines
    }

    /// Renders one tool block: collapsed → single `🔧/✅ <tool>: …`
    /// line; expanded → metrics (`count/provider`) + ≤100-char preview.
    fn tool_block_lines(block: &ToolIntermediate, expanded: bool) -> Vec<Line<'static>> {
        match block {
            ToolIntermediate::Started { tool, args } => {
                let mut lines = vec![Line::from(Span::raw(format!("🔧 {tool}: …")))];
                if expanded {
                    let detail = args
                        .get("raw")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| args.to_string());
                    if !detail.is_empty() {
                        lines.push(Line::from(Span::raw(format!("  {detail}"))));
                    }
                }
                lines
            }
            ToolIntermediate::Completed {
                tool,
                count,
                duration_ms,
                provider,
                error,
                preview,
            } => {
                let head = match error {
                    Some(e) => format!("✅ {tool}: failed: {e}"),
                    None => format!("✅ {tool}: …"),
                };
                let mut lines = vec![Line::from(Span::raw(head))];
                if expanded {
                    let mut metrics: Vec<String> = Vec::new();
                    if let Some(c) = count {
                        metrics.push(format!("{c} hits"));
                    }
                    if let Some(ms) = duration_ms {
                        metrics.push(format!("{ms}ms"));
                    }
                    if let Some(p) = provider {
                        metrics.push(format!("provider {p}"));
                    }
                    if !metrics.is_empty() {
                        lines.push(Line::from(Span::raw(format!("  {}", metrics.join(" · ")))));
                    }
                    if let Some(pv) = preview
                        && !pv.is_empty()
                    {
                        lines.push(Line::from(Span::raw(format!("  {pv}"))));
                    }
                }
                lines
            }
        }
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
    ///
    /// W3: when `show_thinking` is true, reasoning is NEVER committed to
    /// scrollback while active. Instead:
    /// - Active think: `⏳ Thinking… (Ns)` header + last 3 lines → pending
    ///   tail only (never committed).
    /// - Think close: exactly one `✓ Thought for Ns` line → committed, then
    ///   a blank separator. No reasoning body reaches scrollback.
    pub fn drain_and_tail_filtered(
        &mut self,
        reasoning_style: Style,
        show_thinking: bool,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        self.split_render_opt(reasoning_style, show_thinking, true)
    }

    fn split_render(&mut self, reasoning_style: Style) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        self.split_render_opt(reasoning_style, true, false)
    }

    /// `drain=true` (inline scrollback pipeline) emits each tool block
    /// exactly once past the watermark; `drain=false` (fullscreen
    /// snapshot) re-renders every block so the live view stays stable.
    fn split_render_opt(
        &mut self,
        reasoning_style: Style,
        show_thinking: bool,
        drain: bool,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        if self.buffer.is_empty() && self.tool_blocks.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let (text, reasoning, in_think) = self.get_or_compute_split(&self.buffer.clone());
        self.reasoning_active = in_think;

        // Record the instant when reasoning first appears in this turn.
        if self.reasoning_started_at.is_none() && !reasoning.is_empty() {
            self.reasoning_started_at = Some(Instant::now());
        }

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

        // W3: Collapsed thinking display (Codex pattern: reasoning never
        // pollutes the transcript). Only when show_thinking is true.
        if !reasoning.is_empty() && show_thinking {
            let elapsed_secs = self.thinking_elapsed_secs();
            let header_text = if in_think {
                format!("\u{23f3} Thinking\u{2026} ({elapsed_secs}s)")
            } else {
                format!("\u{2713} Thought for {elapsed_secs}s")
            };

            let reasoning_update = self.reasoning_renderer.append(reasoning_delta);

            if in_think {
                // ACTIVE THINK: all reasoning goes to pending tail ONLY.
                // Header + last 3 rendered reasoning lines. Never committed.
                let mut all_reasoning_lines: Vec<Line<'static>> = reasoning_update
                    .committed
                    .iter()
                    .flat_map(|b| b.lines.iter().cloned())
                    .map(|l| Self::indent_line(l))
                    .collect();
                if let Some(ref pending_block) = reasoning_update.pending {
                    all_reasoning_lines.extend(
                        pending_block
                            .lines
                            .iter()
                            .cloned()
                            .map(|l| Self::indent_line(l)),
                    );
                }

                // Cap to last 3 lines so pending stays ≤4 total (header + 3).
                let start = all_reasoning_lines
                    .len()
                    .saturating_sub(THINKING_BODY_MAX_LINES);
                let tail_lines = &all_reasoning_lines[start..];

                let header = Line::from(Span::styled(header_text, reasoning_style));
                pending.push(header);
                for rl in tail_lines {
                    pending.push(rl.clone());
                }
            } else if !self.thinking_summary_committed {
                // BUG-1: THINK CLOSE: commit exactly ONE summary line, no body.
                // The once-guard prevents duplicate ✓ lines on repeated drains.
                committed.push(Line::from(Span::styled(header_text, reasoning_style)));
                // Blank separator after the thought summary.
                committed.push(Line::from(Span::raw("")));
                self.thinking_summary_committed = true;
            }
        }

        // T057: tool blocks render before the text tail (they arrive
        // between LLM rounds, so this preserves arrival order for the
        // common case).
        if drain {
            let expanded = self.tools_expanded;
            let start = self.rendered_tools.min(self.tool_blocks.len());
            for block in &self.tool_blocks[start..] {
                committed.extend(Self::tool_block_lines(block, expanded));
            }
            self.rendered_tools = self.tool_blocks.len();
        } else {
            let expanded = self.tools_expanded;
            for block in &self.tool_blocks {
                committed.extend(Self::tool_block_lines(block, expanded));
            }
        }

        if !text.is_empty() {
            let text_update = self.text_renderer.append(text_delta);
            for block in &text_update.committed {
                committed.extend(block.lines.iter().cloned());
            }
            if let Some(pending_block) = &text_update.pending {
                // W4a: mdstream 0.3.0 guarantees pending blocks only contain
                // complete lines (it breaks out of append_core's line-processing
                // loop when `line_has_newline` returns false — see
                // mdstream/src/stream/engine.rs L90-96). No newline gate needed;
                // partial lines stay in the LineBuffer until the next chunk arrives.
                //
                // W4c: mdstream also handles table holdback correctly —
                // `BlockMode::Table` is tracked by BlockMachine; tables commit
                // atomically (only when a blank line triggers `after_blank_line_decision`
                // in BoundaryDetector, which means the table is complete). No
                // speculative holdback scanner needed at this layer.
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

    /// W3: elapsed seconds since reasoning first appeared in this turn.
    pub fn thinking_elapsed_secs(&self) -> u64 {
        self.reasoning_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0)
    }

    /// W3: whether the timer display changed since last render tick.
    /// Returns true when the displayed second differs from the last render,
    /// signalling the inline event loop to redraw.
    pub fn thinking_timer_changed(&mut self) -> bool {
        if !self.reasoning_active {
            return false;
        }
        let now_secs = self.thinking_elapsed_secs();
        if self.last_rendered_think_secs != Some(now_secs) {
            self.last_rendered_think_secs = Some(now_secs);
            true
        } else {
            false
        }
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
        self.tool_blocks.clear();
        self.rendered_tools = 0;
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
        self.tool_blocks.clear();
        self.rendered_tools = 0;
        // W3: reset thinking timer for the next turn.
        self.reasoning_started_at = None;
        self.last_rendered_think_secs = None;
        // BUG-1: reset the once-guard so next turn can commit its summary.
        self.thinking_summary_committed = false;
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
        // W3: open think tag â reasoning renders in pending tail only.
        let mut c = StreamCollector::new();
        c.push_delta("<think>visible reasoning");
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
        assert!(
            all.contains("\u{23f3}"),
            "must have thinking header: {all:?}"
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

    /// T057: 🔧/✅ lines become collapsible blocks, not buffer text —
    /// collapsed renders one-liners only; the raw text never reaches
    /// `finalize_and_drain`.
    #[test]
    fn tool_lines_route_to_collapsible_blocks() {
        let mut c = StreamCollector::new();
        c.push_delta("🔧 web.search — searching…\n");
        c.push_delta("answer text ");
        c.push_delta(
            "✅ web.search done 5 hits 1234ms provider=brave\nfirst result preview text\n",
        );
        c.push_delta("continues");

        assert_eq!(c.buffer(), "answer text continues");

        let (committed, pending) = c.drain_and_tail_filtered(Style::default(), true);
        let all = lines_text(&committed) + &lines_text(&pending);
        assert!(all.contains("🔧 web.search: …"), "{all:?}");
        assert!(all.contains("✅ web.search: …"), "{all:?}");
        assert!(
            !all.contains("first result preview text") && !all.contains("1234ms"),
            "collapsed block must hide preview/metrics: {all:?}"
        );
        assert!(all.contains("answer text continues"), "{all:?}");

        let (raw, _) = c.finalize_and_drain();
        assert_eq!(raw, "answer text continues");
        assert!(!raw.contains('🔧') && !raw.contains('✅'));
    }

    /// T057: expanded mode surfaces metrics + ≤100-char preview.
    #[test]
    fn expanded_block_shows_metrics_and_preview() {
        let mut c = StreamCollector::new();
        c.toggle_tools_expanded();
        assert!(c.tools_expanded());
        c.push_delta(
            "✅ web.search done 5 hits 1234ms provider=brave\nfirst result preview text\n",
        );
        let (committed, pending) = c.drain_and_tail_filtered(Style::default(), true);
        let all = lines_text(&committed) + &lines_text(&pending);
        assert!(all.contains("5 hits · 1234ms · provider brave"), "{all:?}");
        assert!(all.contains("first result preview text"), "{all:?}");
    }

    /// T057: drain emits each tool block exactly once (same idempotency
    /// contract as text blocks).
    #[test]
    fn tool_blocks_drain_exactly_once() {
        let mut c = StreamCollector::new();
        c.push_delta("🔧 web.search — searching…\n");
        let (c1, _) = c.drain_and_tail_filtered(Style::default(), true);
        assert!(lines_text(&c1).contains("🔧 web.search: …"));
        let (c2, _) = c.drain_and_tail_filtered(Style::default(), true);
        assert!(
            !lines_text(&c2).contains('🔧'),
            "block re-emitted on second drain: {c2:?}"
        );
    }

    /// T057: take_tool_lines flushes blocks the watermark missed
    /// (fullscreen completion path) and forgets them.
    #[test]
    fn take_tool_lines_flushes_remaining_blocks() {
        let mut c = StreamCollector::new();
        c.push_delta("✅ web.search done 2 hits 90ms provider=ddg\npreview line\n");
        let lines = c.take_tool_lines();
        let text = lines_text(&lines);
        assert!(text.contains("✅ web.search: …"), "{text:?}");
        assert!(c.take_tool_lines().is_empty(), "must not re-flush");
    }

    /// T057: mixed token — text around a 🔧 line survives intact in
    /// the buffer with newlines preserved.
    #[test]
    fn mixed_token_text_flows_around_tool_line() {
        let mut c = StreamCollector::new();
        c.push_delta("text before\n🔧 web.search x\nmiddle");
        assert_eq!(c.buffer(), "text before\nmiddle");
    }

    // --- W3: Thinking collapse tests ---

    /// W3: while a think block is open, reasoning renders ONLY in the pending
    /// tail with the `⏳ Thinking… (` header. No reasoning body reaches
    /// committed scrollback.
    #[test]
    fn thinking_active_pending_only() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>Let me think step by step");
        let (committed, pending) = c.drain_and_tail_filtered(Style::default(), true);

        // Committed must NOT contain any reasoning body or header.
        let committed_text = lines_text(&committed);
        assert!(
            !committed_text.contains("Let me think"),
            "reasoning body must NOT be in committed: {committed_text:?}"
        );
        assert!(
            !committed_text.contains("Thinking"),
            "thinking header must NOT be in committed: {committed_text:?}"
        );

        // Pending must contain the header with ⏳ and the reasoning.
        let pending_text = lines_text(&pending);
        assert!(
            pending_text.contains("\u{23f3} Thinking\u{2026}"),
            "pending must have ⏳ Thinking… header: {pending_text:?}"
        );
        assert!(
            pending_text.contains("Let me think"),
            "pending must show reasoning body: {pending_text:?}"
        );
    }

    /// W3: when think block closes, exactly one `✓ Thought for Ns` line is
    /// committed with a blank separator. No reasoning body in committed.
    #[test]
    fn thinking_close_commits_summary_only() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>reasoning content</think>The answer.");
        let (committed, pending) = c.drain_and_tail_filtered(Style::default(), true);

        let committed_text = lines_text(&committed);
        // Must contain the ✓ summary.
        assert!(
            committed_text.contains("\u{2713} Thought for"),
            "committed must have ✓ Thought for: {committed_text:?}"
        );
        // Must NOT contain the reasoning body.
        assert!(
            !committed_text.contains("reasoning content"),
            "committed must NOT contain reasoning body: {committed_text:?}"
        );
        // Pending must contain the answer text (from text_renderer).
        let pending_text = lines_text(&pending);
        assert!(
            pending_text.contains("The answer."),
            "pending must have answer text: {pending_text:?}"
        );
    }

    /// W3: show_thinking=false still emits zero reasoning (unchanged contract).
    #[test]
    fn thinking_disabled_emits_zero_reasoning() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>secret</think>visible answer");
        let (committed, pending) = c.drain_and_tail_filtered(Style::default(), false);
        let all = lines_text(&committed) + &lines_text(&pending);
        assert!(
            !all.contains("secret"),
            "thinking disabled must hide reasoning: {all:?}"
        );
        assert!(
            !all.contains("Thinking") && !all.contains("Thought"),
            "thinking disabled must hide headers: {all:?}"
        );
        assert!(
            all.contains("visible answer"),
            "text must still render: {all:?}"
        );
    }

    /// W3: the pending tail caps reasoning lines to last 3 (header + 3 body).
    #[test]
    fn thinking_pending_caps_lines() {
        let mut c = StreamCollector::new();
        // Simulate long reasoning that produces many rendered lines.
        let reasoning = (0..20)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        c.push_delta(&format!("<think>{reasoning}"));
        let (_, pending) = c.drain_and_tail_filtered(Style::default(), true);
        // Header (1 line) + ≤3 body lines = ≤4 lines total.
        assert!(
            pending.len() <= 4,
            "pending must be ≤4 lines (header + 3 body), got {}",
            pending.len()
        );
        // Verify the header is present.
        assert!(
            lines_text(&[pending[0].clone()]).contains("\u{23f3}"),
            "first line must be the header"
        );
    }

    /// W3: thinking_elapsed_secs returns non-zero after reasoning starts.
    #[test]
    fn thinking_elapsed_nonzero() {
        let mut c = StreamCollector::new();
        assert_eq!(c.thinking_elapsed_secs(), 0, "before reasoning, elapsed=0");
        c.push_delta("<think>thinking");
        let _ = c.drain_and_tail_filtered(Style::default(), true);
        // The instant was set on first reasoning delta.
        assert!(
            c.thinking_elapsed_secs() < 2,
            "elapsed should be small right after start"
        );
    }

    /// W3: clear() resets the thinking timer.
    #[test]
    fn clear_resets_thinking_timer() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>thinking");
        let _ = c.drain_and_tail_filtered(Style::default(), true);
        assert!(c.reasoning_started_at.is_some());
        c.clear();
        assert!(
            c.reasoning_started_at.is_none(),
            "clear must reset reasoning_started_at"
        );
        assert_eq!(c.thinking_elapsed_secs(), 0);
    }

    // BUG-1: Regression tests for duplicate thinking summary commits.

    /// BUG-1: After think-close, multiple drains must NOT produce duplicate
    /// summary lines in committed output.
    #[test]
    fn thinking_summary_committed_once_across_drains() {
        let mut c = StreamCollector::new();
        // Push think tags that close in the same chunk.
        c.push_delta("<think>reasoning</think>chunk1");
        let (c1, _) = c.drain_and_tail_filtered(Style::default(), true);
        let text1 = lines_text(&c1);
        assert!(
            text1.contains("\u{2713} Thought for"),
            "first drain must have checkmark: {:?}",
            text1
        );

        // Push more text, drain again.
        c.push_delta("chunk2");
        let (c2, _) = c.drain_and_tail_filtered(Style::default(), true);
        let text2 = lines_text(&c2);

        // Push final text, drain once more.
        c.push_delta("chunk3");
        let (c3, _) = c.drain_and_tail_filtered(Style::default(), true);
        let text3 = lines_text(&c3);

        // Across ALL committed outputs, exactly ONE line contains the summary.
        let all_committed = format!("{}\\n{}\\n{}", text1, text2, text3);
        let count = all_committed.matches("\u{2713} Thought for").count();
        assert_eq!(
            count, 1,
            "expected exactly one summary line across all drains, got {}: {:?}",
            count, all_committed
        );
    }

    /// BUG-1: Drain twice with NO new delta after close - second drain's
    /// committed contains no summary line.
    #[test]
    fn thinking_summary_no_duplicate_on_idle_drain() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>reasoning</think>answer");
        let (c1, _) = c.drain_and_tail_filtered(Style::default(), true);
        assert!(lines_text(&c1).contains("\u{2713} Thought for"));

        // Second drain with no new content - must NOT emit another summary.
        let (c2, _) = c.drain_and_tail_filtered(Style::default(), true);
        let text2 = lines_text(&c2);
        assert!(
            !text2.contains("\u{2713} Thought for"),
            "second idle drain must not duplicate summary: {:?}",
            text2
        );
    }

    /// BUG-1: clear() resets the once-guard so a new turn can commit its summary.
    #[test]
    fn thinking_summary_guard_resets_on_clear() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>reasoning</think>answer");
        let (c1, _) = c.drain_and_tail_filtered(Style::default(), true);
        assert!(lines_text(&c1).contains("\u{2713} Thought for"));

        // Clear (simulates new turn).
        c.clear();

        // Push new think block, drain - must commit a new summary.
        c.push_delta("<think>more reasoning</think>more answer");
        let (c2, _) = c.drain_and_tail_filtered(Style::default(), true);
        assert!(
            lines_text(&c2).contains("\u{2713} Thought for"),
            "after clear, new turn must commit summary"
        );
    }
}
