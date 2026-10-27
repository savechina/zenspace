use std::time::Instant;

use super::transcript_source::TranscriptSource;
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
    // (a) Footer live-summary cache: latest reasoning line while a think
    // block is open. Refreshed on EVERY split REGARDLESS of show_thinking —
    // the footer status header is mode-independent (codex separates the
    // status header from transcript gating).
    latest_reasoning_summary: Option<String>,
    // (c) Brief pending-tail preference mirror, set by the /thinking handler
    // alongside App::thinking_brief (same shape as tools_expanded — keeps the
    // frozen inline.rs/chat.rs drain call sites signature-identical).
    thinking_brief: bool,
    // T082 (NFR-009): inter-token stall tracker for the current turn.
    stall: super::stall::StallTracker,
    // Wave 2A: source log for transcript replay. Appended ONLY on the
    // inline drain path (`drain == true`) in the exact order committed
    // lines are assembled, so the registry mirrors the rendered transcript.
    // The fullscreen snapshot path (drain == false) never accumulates.
    source_log: Vec<TranscriptSource>,
    // Wave 2A: display source of the current pending tail block (the block
    // mdstream has not committed yet). Consumed once at the completion
    // boundary — the final block never passes through the committed path,
    // so without this its source would vanish from replay.
    last_pending_raw: Option<String>,
}

/// Maximum reasoning lines kept in the pending tail viewport (header + N lines).
/// Must stay ≤4 total (header + 3 body lines) to fit the 2-4 row tail viewport.
const THINKING_BODY_MAX_LINES: usize = 3;

/// (a) Footer live-summary cap (codex `latest_summary_line` truncation).
const REASONING_SUMMARY_MAX_CHARS: usize = 120;

/// (c) Brief pending-tail single-line cap (hermes THINKING_COT_MAX).
const THINKING_BRIEF_MAX_CHARS: usize = 160;

/// (b) Thought-graduation title cap (hermes: 72 chars).
const THOUGHT_TITLE_MAX_CHARS: usize = 72;

/// Collapses every whitespace run (incl. newlines/tabs) into single spaces.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncates to at most `max_chars` on a CHAR boundary (CJK-safe — never
/// slices mid-char), appending an ellipsis when clipping occurred.
pub(crate) fn truncate_char_ellipsis(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// (a) codex `latest_summary_line`: the LAST line of the reasoning body that
/// qualifies — empty/whitespace lines are skipped, leading `#` header markers
/// and `**` bold markers are stripped (plus the bold CLOSER, so bolded lines
/// read as prose), whitespace is collapsed, and the result is capped at
/// `max_chars` with an ellipsis. Returns `None` when nothing qualifies.
fn reasoning_summary_line_capped(reasoning: &str, max_chars: usize) -> Option<String> {
    let mut latest: Option<String> = None;
    for raw in reasoning.lines() {
        // Owns the marker-stripped line when the mid-line closer rewrite
        // fires (borrow must outlive the loop iteration).
        let rewritten;
        let mut line = raw.trim().trim_start_matches('#').trim();
        let was_bold = line.starts_with("**");
        line = line.trim_start_matches("**").trim();
        if was_bold {
            if let Some(rest) = line.strip_suffix("**") {
                line = rest;
            } else if let Some(idx) = line.find("**") {
                // Mid-line bold closer: drop the two-marker span.
                rewritten = format!("{}{}", &line[..idx], &line[idx + 2..]);
                line = rewritten.trim();
            }
        }
        if line.is_empty() {
            continue;
        }
        latest = Some(collapse_whitespace(line));
    }
    latest.map(|s| truncate_char_ellipsis(&s, max_chars))
}

/// (a) Footer-capped variant of [`reasoning_summary_line_capped`].
fn reasoning_summary_line(reasoning: &str) -> Option<String> {
    reasoning_summary_line_capped(reasoning, REASONING_SUMMARY_MAX_CHARS)
}

/// (b) hermes thought-graduation title: the first `**bold**` segment wins;
/// otherwise the first TERMINATED sentence (terminators CJK/ASCII plus
/// newline). An unterminated run deliberately yields NO title — that keeps
/// the W3 no-reasoning-body-in-scrollback pin intact for bare one-liners.
/// Whitespace-collapsed, trailing punctuation trimmed, capped at 72 chars on
/// a char boundary. `None` when nothing is extractable — the graduation line
/// then stays bare (no dangling separator).
fn thought_title(reasoning: &str) -> Option<String> {
    if reasoning.trim().is_empty() {
        return None;
    }
    let raw = reasoning
        .split_once("**")
        .and_then(|(_, rest)| rest.split_once("**"))
        .map(|(seg, _)| collapse_whitespace(seg))
        .filter(|seg| !seg.is_empty())
        .or_else(|| {
            let mut sentence = String::new();
            let mut terminated = false;
            for ch in reasoning.trim_start().chars() {
                if matches!(
                    ch,
                    '\u{3002}'
                        | '\u{ff0e}'
                        | '.'
                        | '!'
                        | '?'
                        | '\u{ff01}'
                        | '\u{ff1f}'
                        | '\n'
                        | '\r'
                ) {
                    terminated = true;
                    break;
                }
                sentence.push(ch);
            }
            if !terminated {
                return None;
            }
            let collapsed = collapse_whitespace(&sentence);
            if collapsed.is_empty() {
                None
            } else {
                Some(collapsed)
            }
        })?;
    let trimmed = raw
        .trim()
        .trim_end_matches([
            '.', '\u{3002}', '\u{ff0e}', '!', '\u{ff01}', '?', '\u{ff1f}', ',', '\u{ff0c}', ';',
            '\u{ff1b}', ':', '\u{ff1a}', '\u{2026}',
        ])
        .trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_char_ellipsis(trimmed, THOUGHT_TITLE_MAX_CHARS))
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
            tool_blocks: Vec::new(),
            source_log: Vec::new(),
            last_pending_raw: None,
            rendered_tools: 0,
            tools_expanded: false,
            reasoning_started_at: None,
            last_rendered_think_secs: None,
            thinking_summary_committed: false,
            latest_reasoning_summary: None,
            thinking_brief: false,
            stall: super::stall::StallTracker::default(),
        }
    }

    pub fn push_delta(&mut self, delta: &str) {
        // T082 (NFR-009): timestamp the delta arrival before any parsing —
        // the inter-token gap is measured at the point text enters the TUI.
        self.stall.note_delta();
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

    /// (c) Brief pending-tail preference mirror (see the field comment).
    pub fn set_thinking_brief(&mut self, brief: bool) {
        self.thinking_brief = brief;
    }

    /// (a) codex-style live status-header payload: the LATEST reasoning line
    /// while the model is emitting reasoning (open think block with content).
    /// Mode-independent by contract — callers must NOT gate on /thinking.
    pub fn live_reasoning_summary(&self) -> Option<String> {
        self.latest_reasoning_summary.clone()
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

        // (a) Refresh the footer live-summary cache on every split, in BOTH
        // /thinking modes — the split always runs, only the delta feed is
        // mode-gated above.
        self.latest_reasoning_summary = if in_think && !reasoning.is_empty() {
            reasoning_summary_line(&reasoning)
        } else {
            None
        };

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
                // (b) Thought graduation title (hermes `· <title>`): first
                // bold segment else first terminated sentence. No extractable
                // title keeps today's bare line (no dangling separator).
                match thought_title(&reasoning) {
                    Some(title) => {
                        format!("\u{2713} Thought for {elapsed_secs}s \u{b7} {title}")
                    }
                    None => format!("\u{2713} Thought for {elapsed_secs}s"),
                }
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
                if self.thinking_brief {
                    // (c) BRIEF mode (hermes `truncated`): ONE collapsed line
                    // capped at THINKING_BRIEF_MAX_CHARS instead of the last-3
                    // tail. The renderer above still consumes the delta, so a
                    // mid-think switch back to full resumes without a gap.
                    let collapsed = truncate_char_ellipsis(
                        &collapse_whitespace(&reasoning),
                        THINKING_BRIEF_MAX_CHARS,
                    );
                    if !collapsed.is_empty() {
                        pending.push(Self::indent_line(Line::from(Span::styled(
                            collapsed,
                            reasoning_style,
                        ))));
                    }
                } else {
                    for rl in tail_lines {
                        pending.push(rl.clone());
                    }
                }
            } else if !self.thinking_summary_committed {
                // BUG-1: THINK CLOSE: commit exactly ONE summary line, no body.
                // The once-guard prevents duplicate ✓ lines on repeated drains.
                let header_line = Line::from(Span::styled(header_text, reasoning_style));
                committed.push(header_line.clone());
                // Blank separator after the thought summary.
                committed.push(Line::from(Span::raw("")));
                self.thinking_summary_committed = true;
                // Wave 2A: the summary line is timing-derived (elapsed secs)
                // and NOT re-derivable from raw source — capture the rendered
                // Lines so replay keeps them (PreRendered kind).
                if drain {
                    self.source_log.push(TranscriptSource::PreRendered(vec![
                        header_line,
                        Line::from(Span::raw("")),
                    ]));
                }
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
                // Wave 2A: capture the ToolIntermediate itself so replay
                // re-renders with the CURRENT /tools expanded state.
                self.source_log.push(TranscriptSource::Tool(block.clone()));
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
                // Wave 2A: capture the block's display source — replay
                // re-runs it through render_markdown (the exact pipeline
                // that produced these lines) at the new width.
                if drain {
                    self.source_log
                        .push(TranscriptSource::Markdown(block.display.clone()));
                }
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
                // Wave 2A: remember the pending block's source so the
                // completion boundary can capture it exactly once (take_).
                if drain && !pending_block.lines.is_empty() {
                    self.last_pending_raw = Some(pending_block.raw.clone());
                }
            }
        }

        (committed, pending)
    }

    /// Wave 2A: drain the source log accumulated by the inline drain path.
    /// Called right after `drain_and_tail_filtered` by the inline pipeline;
    /// entries append to the App-level registry in commit order.
    pub(crate) fn take_source_log(&mut self) -> Vec<TranscriptSource> {
        std::mem::take(&mut self.source_log)
    }

    /// Wave 2A: consume the pending tail block's source once (completion
    /// boundary only — mid-stream calls would race the growing tail).
    pub fn take_pending_source(&mut self) -> Option<String> {
        self.last_pending_raw.take()
    }

    /// Wave 2A: render one captured tool intermediate (exposed for the
    /// replay path so registry entries re-render with the CURRENT /tools
    /// expanded preference instead of the capture-time one).
    pub(crate) fn render_tool_block(
        block: &ToolIntermediate,
        expanded: bool,
    ) -> Vec<Line<'static>> {
        Self::tool_block_lines(block, expanded)
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
        // Wave 2A: the turn ended — any unconsumed replay sources are stale.
        self.source_log.clear();
        self.last_pending_raw = None;
        // (a) the turn is over — drop the live footer summary.
        self.latest_reasoning_summary = None;
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
        self.source_log.clear();
        self.last_pending_raw = None;
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
        // (a) fresh footer summary for the next turn.
        self.latest_reasoning_summary = None;
        // T082: fresh stall tracking for the next turn.
        self.stall.reset();
    }

    /// T082 (NFR-009): inter-token stall gaps measured so far this turn and
    /// awaiting their `tui.stream.stall` audit line.
    pub fn take_stall_gaps(&mut self) -> Vec<u64> {
        self.stall.take_pending_gaps()
    }

    /// T082 (NFR-009): the turn ended — a trailing gap since the last delta
    /// still counts as one final stall episode (measured once).
    pub fn stall_turn_end(&mut self) {
        self.stall.flush_turn_end_at(std::time::Instant::now());
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

    // --- (a) footer live-summary extraction (codex latest_summary_line) ---

    #[test]
    fn reasoning_summary_line_multi_line_picks_last_non_empty() {
        let s = "first thought\n\n  \nsecond thought\n";
        assert_eq!(
            reasoning_summary_line(s).as_deref(),
            Some("second thought"),
            "must pick the LAST qualifying line"
        );
    }

    #[test]
    fn reasoning_summary_line_strips_hashes_bold_and_collapses() {
        // Leading `#`s stripped from qualifying lines; a fully-bolded line
        // loses BOTH markers (opener + closer); whitespace runs collapse.
        assert_eq!(
            reasoning_summary_line("## Heading").as_deref(),
            Some("Heading")
        );
        assert_eq!(
            reasoning_summary_line("**bold point**").as_deref(),
            Some("bold point")
        );
        assert_eq!(
            reasoning_summary_line("a\tb\nc   d").as_deref(),
            Some("c d")
        );
        // Mid-line bold closer is dropped too (opener was stripped).
        assert_eq!(
            reasoning_summary_line("**plan** for retrieval").as_deref(),
            Some("plan for retrieval")
        );
    }

    #[test]
    fn reasoning_summary_line_truncates_at_char_cap_with_ellipsis() {
        let long = "x".repeat(REASONING_SUMMARY_MAX_CHARS + 40);
        let out = reasoning_summary_line(&long).expect("non-empty");
        assert_eq!(out.chars().count(), REASONING_SUMMARY_MAX_CHARS);
        assert!(
            out.ends_with('\u{2026}'),
            "clipped output ends with ellipsis"
        );
        // CJK safety: multi-byte text truncates on a char boundary.
        let cjk = "\u{8bb0}".repeat(REASONING_SUMMARY_MAX_CHARS + 40);
        let out = reasoning_summary_line(&cjk).expect("non-empty");
        assert_eq!(out.chars().count(), REASONING_SUMMARY_MAX_CHARS);
    }

    #[test]
    fn reasoning_summary_line_none_on_blank() {
        assert_eq!(reasoning_summary_line(""), None);
        assert_eq!(reasoning_summary_line("  \n\t\n"), None);
        assert_eq!(
            reasoning_summary_line("###\n**"),
            None,
            "marker-only lines skip"
        );
    }

    #[test]
    fn live_reasoning_summary_tracks_open_think_regardless_of_mode() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>## Plan\nlatest idea here");
        // show_thinking=false drain must STILL refresh the footer cache.
        let _ = c.drain_and_tail_filtered(Style::default(), false);
        assert_eq!(
            c.live_reasoning_summary().as_deref(),
            Some("latest idea here"),
            "footer summary is mode-independent"
        );
        // Think closed: no live summary — the footer falls back.
        c.push_delta("</think>answer");
        let _ = c.drain_and_tail_filtered(Style::default(), false);
        assert_eq!(c.live_reasoning_summary(), None);
    }

    // --- (b) thought graduation title (hermes convention) ---

    #[test]
    fn thought_title_from_bold_segment() {
        assert_eq!(
            thought_title("先看背景。 **设计记忆检索** 然后是实现。").as_deref(),
            Some("设计记忆检索"),
            "first bold segment wins over the first sentence"
        );
        assert_eq!(
            thought_title("**Verify pin suite** then gates.").as_deref(),
            Some("Verify pin suite")
        );
    }

    #[test]
    fn thought_title_from_first_sentence_cjk_safe() {
        assert_eq!(
            thought_title("第一个想法是关于记忆系统。然后是检索策略。").as_deref(),
            Some("第一个想法是关于记忆系统"),
            "CJK sentence splits on the ideographic full stop, char-safe"
        );
        assert_eq!(
            thought_title("Check the config. Then run tests!").as_deref(),
            Some("Check the config"),
            "ASCII sentence + trailing punctuation trimmed"
        );
    }

    #[test]
    fn thought_title_truncates_at_char_boundary() {
        let body = "\u{8bb0}".repeat(100);
        let title = thought_title(&format!("{body}\u{3002}")).expect("terminated sentence");
        assert_eq!(title.chars().count(), THOUGHT_TITLE_MAX_CHARS);
        assert!(title.ends_with('\u{2026}'));
    }

    #[test]
    fn thought_title_none_on_blank() {
        assert_eq!(thought_title(""), None);
        assert_eq!(thought_title("   \n\t "), None);
    }

    #[test]
    fn thought_title_none_on_unterminated_prose() {
        // No bold span, no terminator: no title — the W3 no-body pin holds
        // for bare one-liner reasoning (graduation line stays bare).
        assert_eq!(thought_title("reasoning content"), None);
    }

    #[test]
    fn graduation_line_includes_title() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>**Verify pin suite** then gates.</think>Answer.");
        let (committed, _) = c.drain_and_tail_filtered(Style::default(), true);
        let text = lines_text(&committed);
        assert!(text.contains("\u{2713} Thought for"), "{text:?}");
        assert!(
            text.contains("\u{b7} Verify pin suite"),
            "graduation line must carry the title: {text:?}"
        );
    }

    #[test]
    fn graduation_line_bare_when_no_title() {
        let mut c = StreamCollector::new();
        c.push_delta("<think>reasoning content</think>Answer.");
        let (committed, _) = c.drain_and_tail_filtered(Style::default(), true);
        let text = lines_text(&committed);
        assert!(text.contains("\u{2713} Thought for"), "{text:?}");
        assert!(
            !text.contains('\u{b7}'),
            "no title means NO dangling separator: {text:?}"
        );
        assert!(
            !text.contains("reasoning content"),
            "no reasoning body leaks into scrollback: {text:?}"
        );
    }

    // --- (c) three-state thinking display ---

    #[test]
    fn brief_mode_shows_single_truncated_line() {
        let mut c = StreamCollector::new();
        c.set_thinking_brief(true);
        let reasoning = (0..40)
            .map(|i| format!("point {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        c.push_delta(&format!("<think>{reasoning}"));
        let (_, pending) = c.drain_and_tail_filtered(Style::default(), true);
        assert_eq!(
            pending.len(),
            2,
            "brief mode: header + exactly ONE body line: {pending:?}"
        );
        assert!(
            lines_text(&[pending[0].clone()]).contains("\u{23f3} Thinking"),
            "the ⏳ header is kept in brief mode"
        );
        let body = lines_text(&[pending[1].clone()]);
        assert!(
            body.starts_with("  point 0"),
            "brief body collapses from the head: {body:?}"
        );
        assert!(!body.contains('\n'), "single line: {body:?}");
        assert!(
            body.trim().chars().count() <= THINKING_BRIEF_MAX_CHARS,
            "brief body capped at {THINKING_BRIEF_MAX_CHARS} chars: {body:?}"
        );
        assert!(body.trim().ends_with('\u{2026}'), "truncated: {body:?}");
    }

    #[test]
    fn full_mode_keeps_three_lines() {
        let mut c = StreamCollector::new();
        // Blank-line separated: each line renders as its OWN markdown line
        // (single \n would merge into one paragraph line).
        let reasoning = (0..20)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        c.push_delta(&format!("<think>{reasoning}"));
        let (_, pending) = c.drain_and_tail_filtered(Style::default(), true);
        assert_eq!(
            pending.len(),
            4,
            "full mode unchanged: header + exactly 3 body lines"
        );
        assert!(lines_text(&[pending[1].clone()]).contains("Line 17"));
        assert!(lines_text(&[pending[3].clone()]).contains("Line 19"));
    }
}
