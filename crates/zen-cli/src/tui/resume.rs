#![allow(dead_code)] // T079 FR-025 replay rendering
//! FR-025: Session-resume replay event processing.
//!
//! PURPOSE: Parse gateway session/resume replay events, buffer per turn,
//!   commit finalized text on turn_completed, detect gaps (seq discontinuities),
//!   and produce messages for the TUI main thread channel.
//!
//! USAGE: `ResumeProcessor` is created per resume_session call. Events arrive
//!   from the gateway RPC as JSON `Value`. The processor buffers deltas per turn
//!   and emits `ResumeMessage`s via a channel consumed by `poll_llm_response`.
//!
//! EXPECTED: Complete turns render with no torn content. Gaps emit a one-line
//!   scrollback notice. Completed(-32004) renders the final response text.
//!
//! ERRORS: None — pure processing. Invalid events are logged and skipped.

use ratatui::text::{Line, Span};
use serde_json::Value;

/// Messages sent from the resume-producer task to the TUI main thread.
/// Consumed in `poll_llm_response` and routed through `enqueue_scrollback`.
#[derive(Debug, Clone)]
pub enum ResumeMessage {
    /// Complete turn text to render. Always a finalized turn (never torn).
    TurnText { turn_id: String, text: String },
    /// One or more events were skipped (gap/corrupt). Render a notice line.
    GapNotice { skipped_count: usize },
    /// The gateway returned -32004 (turn already completed). Render the
    /// final response text.
    CompletedResponse { text: String },
}

/// A parsed replay event from the gateway session/resume response.
///
/// Each event in the replay array has shape:
/// ```json
/// { "turnId": "...", "seq": N, "kind": "delta|turn_completed|...", "payload": {...} }
/// ```
#[derive(Debug, Clone)]
pub struct ReplayEvent {
    pub turn_id: String,
    pub seq: u64,
    pub kind: EventKind,
    pub payload: Option<Value>,
}

/// The `kind` field of a session/event notification (contract-02 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Delta,
    TurnCompleted,
    TurnError,
    ToolStarted,
    ToolCompleted,
    ToolError,
    Unknown(String),
}

impl EventKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "delta" => Self::Delta,
            "turn_completed" => Self::TurnCompleted,
            "turn_error" => Self::TurnError,
            "tool_started" => Self::ToolStarted,
            "tool_completed" => Self::ToolCompleted,
            "tool_error" => Self::ToolError,
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// State for buffering events per turn during replay processing.
#[derive(Debug, Default)]
struct TurnBuffer {
    /// Accumulated text deltas (in seq order).
    deltas: Vec<String>,
}

/// Processes a stream of gateway replay events, buffering per turn and
/// emitting `ResumeMessage`s via a channel.
///
/// # Merge/dedup rule (documented per FR-025)
///
/// The local resume path (SessionManager + ConversationStore) already renders
/// archived turns synchronously and clears output first. Gateway replay
/// **complements** this: only events from the replay that contain content the
/// local store does not already have need rendering.
///
/// **Chosen rule**: When the local path succeeds (Ok branch of `resume_session`)
/// AND the replay contains no `turn_completed` events (empty or delta-only),
/// skip replay rendering entirely. When the local path succeeds but the replay
/// HAS completed turns (e.g. an in-flight turn the local store missed), render
/// only those turns. When the local path fails (gateway-only resume), render
/// all replay content.
///
/// This is safe because:
/// - The local store is the authoritative source for committed turns.
/// - The gateway replay adds in-flight/very-recent content.
/// - Empty replay = nothing new to add.
#[derive(Debug)]
pub struct ResumeProcessor {
    /// Current turn buffer (accumulates deltas until turn_completed).
    current_turn: Option<TurnBuffer>,
    current_turn_id: Option<String>,
    /// Events seen so far (for gap detection).
    last_seq: Option<u64>,
    /// Count of skipped/unparseable events.
    skipped_count: usize,
}

impl ResumeProcessor {
    pub fn new() -> Self {
        Self {
            current_turn: None,
            current_turn_id: None,
            last_seq: None,
            skipped_count: 0,
        }
    }

    /// Parse the replay value (an array of event objects) into `ReplayEvent`s.
    ///
    /// Returns `(events, skipped_count)` where `skipped_count` covers
    /// unparseable elements in the array.
    pub fn parse_events(value: &Value) -> (Vec<ReplayEvent>, usize) {
        let arr = match value.as_array() {
            Some(a) => a,
            None => return (vec![], 0),
        };

        let mut events = Vec::new();
        let mut skipped = 0usize;

        for item in arr {
            match Self::parse_single_event(item) {
                Some(ev) => events.push(ev),
                None => skipped += 1,
            }
        }

        (events, skipped)
    }

    /// Parse a single event JSON object into a `ReplayEvent`.
    fn parse_single_event(item: &Value) -> Option<ReplayEvent> {
        let turn_id = item.get("turnId")?.as_str()?.to_string();
        let seq = item.get("seq")?.as_u64()?;
        let kind_str = item.get("kind")?.as_str()?;
        let kind = EventKind::from_str(kind_str);
        let payload = item.get("payload").cloned();

        Some(ReplayEvent {
            turn_id,
            seq,
            kind,
            payload,
        })
    }

    /// Feed a parsed event into the processor. Returns `Some(ResumeMessage)`
    /// when a turn is finalized or a gap is detected.
    pub fn process_event(&mut self, event: &ReplayEvent) -> Option<ResumeMessage> {
        // Gap detection: check for seq discontinuities
        if let Some(prev) = self.last_seq
            && event.seq > prev + 1
        {
            let skipped = (event.seq - prev - 1) as usize;
            self.skipped_count += skipped;
        }
        self.last_seq = Some(event.seq);

        match &event.kind {
            EventKind::Delta => {
                let text = event
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("text").or_else(|| p.get("delta")))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if self.current_turn_id.as_deref() != Some(&event.turn_id) {
                    let old_msg = self.finalize_current_turn();
                    self.start_new_turn(&event.turn_id);
                    if !text.is_empty()
                        && let Some(ref mut buf) = self.current_turn
                    {
                        buf.deltas.push(text);
                    }
                    return old_msg;
                }

                if !text.is_empty()
                    && let Some(ref mut buf) = self.current_turn
                {
                    buf.deltas.push(text);
                }
                None
            }
            EventKind::TurnCompleted | EventKind::TurnError => {
                if self.current_turn_id.as_deref() != Some(&event.turn_id) {
                    let old_msg = self.finalize_current_turn();
                    self.start_new_turn(&event.turn_id);
                    let new_msg = self.finalize_current_turn();
                    return new_msg.or(old_msg);
                }
                self.finalize_current_turn()
            }
            EventKind::ToolStarted | EventKind::ToolCompleted | EventKind::ToolError => None,
            EventKind::Unknown(_) => {
                self.skipped_count += 1;
                None
            }
        }
    }

    /// Flush any remaining buffered content. Call after all events are processed.
    pub fn flush(mut self) -> Vec<ResumeMessage> {
        let mut messages = Vec::new();

        if self.skipped_count > 0 {
            messages.push(ResumeMessage::GapNotice {
                skipped_count: self.skipped_count,
            });
        }

        if let Some(msg) = self.finalize_current_turn() {
            messages.push(msg);
        }

        messages
    }

    fn start_new_turn(&mut self, turn_id: &str) {
        self.current_turn = Some(TurnBuffer::default());
        self.current_turn_id = Some(turn_id.to_string());
    }

    fn finalize_current_turn(&mut self) -> Option<ResumeMessage> {
        let buf = self.current_turn.take()?;
        let turn_id = self.current_turn_id.take().unwrap_or_default();
        self.current_turn = None;

        if buf.deltas.is_empty() {
            return None;
        }

        let text = buf.deltas.join("");
        Some(ResumeMessage::TurnText { turn_id, text })
    }

    /// Detect gaps in a list of raw JSON events. Returns the count of
    /// skipped/missing sequence numbers.
    pub fn detect_gaps(events: &[Value]) -> usize {
        let mut last_seq: Option<u64> = None;
        let mut skipped = 0usize;

        for item in events {
            if let Some(seq) = item.get("seq").and_then(|v| v.as_u64()) {
                if let Some(prev) = last_seq
                    && seq > prev + 1
                {
                    skipped += (seq - prev - 1) as usize;
                }
                last_seq = Some(seq);
            }
        }

        skipped
    }
}

/// Render replayed turn text as scrollback lines.
pub fn render_replay_turn(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![];
    }
    super::markdown::render_markdown(text)
}

/// Render a gap notice as a single scrollback line.
pub fn render_gap_notice(skipped: usize) -> Vec<Line<'static>> {
    vec![Line::from(vec![Span::styled(
        format!(
            "\u{26a0}\u{fe0f} {} event(s) unavailable \u{2014} session history incomplete",
            skipped
        ),
        ratatui::style::Style::default().fg(ratatui::style::Color::Yellow),
    )])]
}

/// Render a Completed(-32004) response as scrollback lines.
pub fn render_completed_response(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![];
    }
    super::markdown::render_markdown(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Synthetic event list: [delta×2, turn_completed] -> one buffered turn committed,
    /// torn-turn impossible (no commit before turn_completed).
    #[test]
    fn replay_event_parsing_buffered_turn() {
        let events = json!([
            { "turnId": "t1", "seq": 0, "kind": "delta", "payload": { "text": "Hello " } },
            { "turnId": "t1", "seq": 1, "kind": "delta", "payload": { "text": "world" } },
            { "turnId": "t1", "seq": 2, "kind": "turn_completed", "payload": {} }
        ]);

        let (parsed, skipped) = ResumeProcessor::parse_events(&events);
        assert_eq!(skipped, 0);
        assert_eq!(parsed.len(), 3);

        let mut proc = ResumeProcessor::new();
        let mut messages = Vec::new();
        for ev in &parsed {
            if let Some(msg) = proc.process_event(ev) {
                messages.push(msg);
            }
        }
        messages.extend(proc.flush());

        let turn_msgs: Vec<_> = messages
            .iter()
            .filter(|m| matches!(m, ResumeMessage::TurnText { .. }))
            .collect();
        assert_eq!(turn_msgs.len(), 1, "exactly one turn should be committed");
        if let ResumeMessage::TurnText { text, turn_id } = &turn_msgs[0] {
            assert_eq!(turn_id, "t1");
            assert_eq!(text, "Hello world");
        }
    }

    /// Gap notice emitted on seq discontinuity.
    #[test]
    fn gap_notice_on_seq_discontinuity() {
        let events = json!([
            { "turnId": "t1", "seq": 0, "kind": "delta", "payload": { "text": "A" } },
            { "turnId": "t1", "seq": 3, "kind": "delta", "payload": { "text": "B" } },
            { "turnId": "t1", "seq": 4, "kind": "turn_completed", "payload": {} }
        ]);

        let (parsed, _skipped) = ResumeProcessor::parse_events(&events);
        let mut proc = ResumeProcessor::new();
        let mut messages = Vec::new();
        for ev in &parsed {
            if let Some(msg) = proc.process_event(ev) {
                messages.push(msg);
            }
        }
        messages.extend(proc.flush());

        let gap_msgs: Vec<_> = messages
            .iter()
            .filter(|m| matches!(m, ResumeMessage::GapNotice { skipped_count: 2 }))
            .collect();
        assert_eq!(
            gap_msgs.len(),
            1,
            "exactly one gap notice for 2 missing seqs"
        );
    }

    /// Completed(-32004) response renders.
    #[test]
    fn completed_response_renders() {
        let text = "This is the final response from the completed turn.";
        let lines = render_completed_response(text);
        assert!(!lines.is_empty());
        let rendered: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(rendered.contains("final response"));
    }

    /// detect_gaps standalone function test.
    #[test]
    fn detect_gaps_standalone() {
        let events = json!([{ "seq": 0 }, { "seq": 1 }, { "seq": 5 }, { "seq": 6 }]);
        let gaps = ResumeProcessor::detect_gaps(events.as_array().unwrap());
        assert_eq!(gaps, 3);
    }

    /// Gap notice rendering test.
    #[test]
    fn gap_notice_rendering() {
        let lines = render_gap_notice(5);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("5"));
        assert!(text.contains("unavailable"));
    }

    /// Multiple turns: each turn commits separately.
    #[test]
    fn multiple_turns_commit_separately() {
        let events = json!([
            { "turnId": "t1", "seq": 0, "kind": "delta", "payload": { "text": "Turn1" } },
            { "turnId": "t1", "seq": 1, "kind": "turn_completed", "payload": {} },
            { "turnId": "t2", "seq": 2, "kind": "delta", "payload": { "text": "Turn2" } },
            { "turnId": "t2", "seq": 3, "kind": "turn_completed", "payload": {} }
        ]);

        let (parsed, skipped) = ResumeProcessor::parse_events(&events);
        assert_eq!(skipped, 0);

        let mut proc = ResumeProcessor::new();
        let mut messages = Vec::new();
        for ev in &parsed {
            if let Some(msg) = proc.process_event(ev) {
                messages.push(msg);
            }
        }
        messages.extend(proc.flush());

        let turn_msgs: Vec<_> = messages
            .iter()
            .filter(|m| matches!(m, ResumeMessage::TurnText { .. }))
            .collect();
        assert_eq!(turn_msgs.len(), 2);
    }

    /// Empty replay produces no messages.
    #[test]
    fn empty_replay_produces_nothing() {
        let events = json!([]);
        let (parsed, skipped) = ResumeProcessor::parse_events(&events);
        assert_eq!(skipped, 0);
        assert!(parsed.is_empty());

        let proc = ResumeProcessor::new();
        let messages = proc.flush();
        assert!(messages.is_empty());
    }

    /// Unknown event kinds are counted as skipped.
    #[test]
    fn unknown_kinds_counted_as_skipped() {
        let events = json!([
            { "turnId": "t1", "seq": 0, "kind": "delta", "payload": { "text": "A" } },
            { "turnId": "t1", "seq": 1, "kind": "something_unknown", "payload": {} },
            { "turnId": "t1", "seq": 2, "kind": "turn_completed", "payload": {} }
        ]);

        let (parsed, _skipped_parse) = ResumeProcessor::parse_events(&events);
        let mut proc = ResumeProcessor::new();
        let mut messages = Vec::new();
        for ev in &parsed {
            if let Some(msg) = proc.process_event(ev) {
                messages.push(msg);
            }
        }
        // flush consumes proc
        let mut messages2 = proc.flush();
        messages.append(&mut messages2);

        let gap_msgs: Vec<_> = messages
            .iter()
            .filter(|m| matches!(m, ResumeMessage::GapNotice { .. }))
            .collect();
        assert_eq!(gap_msgs.len(), 1);
    }

    /// resume_session headless-no-runtime does not panic.
    #[test]
    fn resume_session_headless_no_panic() {
        use crate::tui::app::App;
        let config = Box::leak(Box::default());
        let mut app = App::new(config);
        app.resume_session("nonexistent-session-id");
    }
}
