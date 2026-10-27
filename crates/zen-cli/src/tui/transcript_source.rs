//! Wave 2A: source-backed transcript replay (codex-rs `resize_reflow.rs`
//! precedent).
//!
//! The inline TUI commits rendered ratatui `Line`s into native terminal
//! scrollback, hard-wrapped at the INSERT-time width. After the terminal
//! width changes, the historical transcript keeps its stale wrapping (the
//! user-reported 显示错乱). The resize coalescer + synchronized-update
//! wrapping + ghost-viewport clearing (commit 6148889) fixed the live
//! viewport; this module adds the replay half:
//!
//! - [`TranscriptSourceRegistry`] keeps the SOURCE of every committed
//!   transcript block (markdown text, user echo, tool blocks, banner,
//!   pre-rendered one-offs), capped, so a width change can purge native
//!   scrollback and re-render everything at the new width through the
//!   EXISTING markdown/insert pipeline.
//! - [`replay_decision`] is the PURE gate (no clock/IO): width-change only,
//!   deferred while streaming or reading scrollback, run at the next
//!   boundary (stream completion / reading-mode exit).
//! - [`watchdog_due`] + [`watchdog_size_mismatch`] power a 500ms
//!   `crossterm::terminal::size()` probe that synthesizes a resize when the
//!   live size drifts from the coalescer's applied size (covers missed
//!   SIGWINCH on some tmux setups). Reuses crossterm — no new deps.
//!
//! SCOPE LOGIC (Principle XV):
//! - Trigger: WIDTH change only (height-only changes keep the cheap no-op
//!   path). Fires at the resize coalescer TRAILING edge, or at the
//!   stream-completion / reading-mode-exit boundary when it was deferred.
//! - Streaming: replay is deferred while `app.is_streaming`; it runs once at
//!   the completion boundary if the width changed meanwhile.
//! - Reading mode (PageUp scrollback viewing): replay is deferred until
//!   reading mode exits, so the view never jumps mid-read.
//! - Accepted cost: replay emits `Clear(PurgeHistory)` (ED 3), which wipes
//!   the user's PRE-session shell scrollback, not just zen's own transcript.
//!   This destruction is EXPLICITLY ACCEPTED by the product owner (wave 2A
//!   contract, 2026-10); codex-rs makes the same trade in
//!   `clear_for_viewport_change`.
//! - Failure: replay I/O is fail-soft (T250 degrade contract) — a failed
//!   replay logs a warning, keeps the session alive, and leaves the registry
//!   intact so the next width change can retry.

use std::collections::VecDeque;
use std::time::Instant;

use ratatui::text::Line;
use zen_gateway::server::hosting::ToolIntermediate;

/// Total source-byte budget for the registry (codex `resize_reflow` uses a
/// similar cap). When a push overflows the cap, the OLDEST entries are
/// dropped and [`TranscriptSourceRegistry::truncated`] flips true; replay
/// then prepends a visible truncation notice instead of silently losing
/// history shape.
pub(crate) const TRANSCRIPT_SOURCE_CAP_BYTES: usize = 200 * 1024;

/// Conservative byte estimate for one captured [`TranscriptSource::Tool`]
/// entry (tool lines are short: a marker header plus a 100-char-capped
/// preview). Exact accounting would need serialization; tools are a minor
/// share of transcript volume, so a flat conservative constant suffices.
pub(crate) const TOOL_ENTRY_ESTIMATE_BYTES: usize = 512;

/// Flat byte estimate for the welcome-banner entry. The banner is
/// REGENERATED from its constants at replay (the logo selection is
/// width-dependent), so this only feeds the cap accounting.
pub(crate) const BANNER_ENTRY_ESTIMATE_BYTES: usize = 1024;

/// Visible notice prepended to a replay when entries were dropped by the
/// cap (rendered in the muted theme style).
pub(crate) const TRUNCATION_NOTICE: &str = "... older transcript trimmed (replay source cap)";

/// The source form of one committed transcript block.
///
/// Every registry kind is explicit; content that cannot be regenerated
/// without capture is stored as [`TranscriptSource::PreRendered`] so it
/// never silently vanishes from a replay (fidelity note: PreRendered blocks
/// re-insert with their capture-time content — the insert path still
/// soft-wraps them at the live width, but any internal truncation decided
/// at capture time is kept).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TranscriptSource {
    /// Assistant markdown (committed block display text) or slash-command
    /// output — re-rendered through `render_markdown` at replay time.
    Markdown(String),
    /// User echo line. `queued` reproduces the queued-submission marker.
    UserEcho { text: String, queued: bool },
    /// One parsed tool intermediate — re-rendered through the collector's
    /// block renderer so replay honors the CURRENT /tools expanded state.
    Tool(ToolIntermediate),
    /// Welcome splash banner — REGENERATED from its constants at replay
    /// time because the logo selection depends on terminal width.
    Banner,
    /// Rendered Lines kept as-is (thinking summary lines, turn separators,
    /// error cells, resume notices, cancel notices, ...). Re-inserted
    /// unchanged; see the fidelity note above.
    PreRendered(Vec<Line<'static>>),
}

impl TranscriptSource {
    /// Cap-accounting byte size of this entry (conservative estimates for
    /// non-text kinds; see the constant docs).
    fn source_bytes(&self) -> usize {
        match self {
            TranscriptSource::Markdown(s) => s.len(),
            TranscriptSource::UserEcho { text, .. } => text.len(),
            TranscriptSource::Tool(_) => TOOL_ENTRY_ESTIMATE_BYTES,
            TranscriptSource::Banner => BANNER_ENTRY_ESTIMATE_BYTES,
            TranscriptSource::PreRendered(lines) => lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.len()).sum::<usize>())
                .sum(),
        }
    }
}

/// Capped, append-ordered registry of transcript block sources.
///
/// Single responsibility: hold WHAT was said (source form) so a width
/// change can RE-RENDER the transcript; rendering itself stays in the
/// existing markdown/scrollback pipeline. Oldest entries are at the FRONT
/// (replay iterates front to back = oldest-to-newest insertion order).
#[derive(Default)]
pub(crate) struct TranscriptSourceRegistry {
    entries: VecDeque<TranscriptSource>,
    total_bytes: usize,
    truncated: bool,
}

impl TranscriptSourceRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Append one source entry, dropping oldest entries when the cap is
    /// exceeded (normally the newest survives). Drops flip
    /// [`Self::truncated`]. Edge: a SINGLE entry larger than the entire cap
    /// exceeds any possible budget and is dropped itself — the truncation
    /// notice renders, so the loss is visible, never silent.
    pub(crate) fn push(&mut self, entry: TranscriptSource) {
        self.total_bytes += entry.source_bytes();
        self.entries.push_back(entry);
        while self.total_bytes > TRANSCRIPT_SOURCE_CAP_BYTES {
            match self.entries.pop_front() {
                Some(oldest) => {
                    self.total_bytes = self.total_bytes.saturating_sub(oldest.source_bytes());
                    self.truncated = true;
                }
                None => {
                    self.total_bytes = 0;
                    break;
                }
            }
        }
    }

    /// Append many entries in order.
    pub(crate) fn extend(&mut self, entries: impl IntoIterator<Item = TranscriptSource>) {
        for e in entries {
            self.push(e);
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &TranscriptSource> {
        self.entries.iter()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the cap ever dropped entries (drives the replay notice).
    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }

    /// `/clear` semantics: the transcript restarts from here.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
        // Keep `truncated` — the notice describes history that left the
        // registry earlier; clearing the screen does not resurrect it.
    }
}

/// Verdict of the pure replay gate [`replay_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayVerdict {
    /// Run the replay now (inside one synchronized-update region).
    Now,
    /// A width change is waiting; set the pending flag and run at the next
    /// boundary (stream completion / reading-mode exit).
    Defer,
    /// Nothing to do (no width change, no pending flag).
    Noop,
}

/// PURE replay gate — pins enumerate all 16 input combinations.
///
/// Inputs:
/// - `width_changed`: the applied/live width differs from the last replayed
///   width (height-only changes report false — replay is width-only).
/// - `is_streaming`: defer to the stream-completion boundary.
/// - `reading_mode`: defer to reading-mode exit (view must not jump).
/// - `replay_pending`: a previous decision deferred and has not run yet.
pub(crate) fn replay_decision(
    width_changed: bool,
    is_streaming: bool,
    reading_mode: bool,
    replay_pending: bool,
) -> ReplayVerdict {
    let wanted = width_changed || replay_pending;
    if !wanted {
        return ReplayVerdict::Noop;
    }
    if is_streaming || reading_mode {
        ReplayVerdict::Defer
    } else {
        ReplayVerdict::Now
    }
}

/// Wave 2A replay engine state carried across inline-loop iterations
/// (lives on [`super::inline::InlineLoopState`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplayState {
    /// Width the last SUCCESSFUL replay rendered at. Initialized to the
    /// live startup width by `run_inline_session`; the `Default` of 80 only
    /// matters for headless TestBackend pins (which use 80-col terminals,
    /// so a spurious width-change can never fire there).
    pub(crate) last_replay_width: u16,
    /// Set when a width change arrived while streaming/reading and the
    /// replay was deferred; consumed by the boundary hooks (stream
    /// completion / reading-mode exit).
    pub(crate) pending: bool,
    /// Last watchdog probe instant (`None` = never probed — due at once).
    pub(crate) last_watchdog: Option<Instant>,
}

impl ReplayState {
    pub(crate) fn new() -> Self {
        Self {
            last_replay_width: 80,
            pending: false,
            last_watchdog: None,
        }
    }
}

impl Default for ReplayState {
    fn default() -> Self {
        Self::new()
    }
}

/// Watchdog probe interval (codex `size_monitor.rs` uses 500ms ioctl polls).
pub(crate) const REPLAY_WATCHDOG_INTERVAL_MS: u64 = 500;

/// PURE: is the periodic size probe due? `None` = never probed, so due.
pub(crate) fn watchdog_due(last_probe: Option<Instant>, now: Instant) -> bool {
    match last_probe {
        None => true,
        Some(last) => now.duration_since(last).as_millis() as u64 >= REPLAY_WATCHDOG_INTERVAL_MS,
    }
}

/// PURE: does the live terminal size drift from the coalescer's applied
/// size? ANY component drift counts (a missed height resize must still be
/// synthesized); the REPLAY decision separately filters to width changes.
pub(crate) fn watchdog_size_mismatch(applied: (u16, u16), live: (u16, u16)) -> bool {
    applied != live
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;
    use std::time::Duration;

    fn md(s: &str) -> TranscriptSource {
        TranscriptSource::Markdown(s.to_string())
    }

    /// Registry keeps insertion order (oldest front, newest back) and
    /// exposes every explicit kind.
    #[test]
    fn registry_preserves_order_and_kinds() {
        let mut r = TranscriptSourceRegistry::new();
        r.push(md("first"));
        r.push(TranscriptSource::UserEcho {
            text: "hello".into(),
            queued: false,
        });
        r.push(TranscriptSource::Tool(ToolIntermediate::Started {
            tool: "fs.read".into(),
            args: serde_json::json!({}),
        }));
        r.push(TranscriptSource::Banner);
        r.push(TranscriptSource::PreRendered(vec![Line::from("sep")]));
        let kinds: Vec<String> = r
            .iter()
            .map(|e| match e {
                TranscriptSource::Markdown(s) => format!("md:{s}"),
                TranscriptSource::UserEcho { text, queued } => {
                    format!("echo:{text}:{queued}")
                }
                TranscriptSource::Tool(_) => "tool".to_string(),
                TranscriptSource::Banner => "banner".to_string(),
                TranscriptSource::PreRendered(_) => "prerendered".to_string(),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "md:first".to_string(),
                "echo:hello:false".to_string(),
                "tool".to_string(),
                "banner".to_string(),
                "prerendered".to_string(),
            ]
        );
    }

    /// Cap: pushing past the budget drops the OLDEST entries, flips
    /// `truncated`, and the newest entry always survives.
    #[test]
    fn cap_drops_oldest_and_sets_truncated() {
        let mut r = TranscriptSourceRegistry::new();
        // 1024 entries x ~256 bytes is far above the 200 KiB cap.
        for i in 0..1024 {
            r.push(md(&format!("entry-{i:04}-{}", "x".repeat(256))));
        }
        assert!(r.truncated(), "cap overflow must set truncated");
        assert!(r.total_bytes <= TRANSCRIPT_SOURCE_CAP_BYTES);
        // The oldest entry is gone; the newest survives.
        let first = match r.iter().next() {
            Some(TranscriptSource::Markdown(s)) => s.clone(),
            other => panic!("expected markdown front entry, got {other:?}"),
        };
        assert!(
            !first.starts_with("entry-0000"),
            "oldest entries must be dropped first: {first}"
        );
        let last = match r.iter().last() {
            Some(TranscriptSource::Markdown(s)) => s.clone(),
            other => panic!("expected markdown back entry, got {other:?}"),
        };
        assert!(
            last.starts_with("entry-1023"),
            "newest must survive: {last}"
        );
    }

    /// A single entry larger than the whole cap exceeds any possible budget:
    /// it is dropped itself, the registry empties, and `truncated` is set so
    /// the replay renders the visible notice instead of losing shape
    /// silently.
    #[test]
    fn oversized_entry_is_dropped_with_truncation_notice() {
        let mut r = TranscriptSourceRegistry::new();
        r.push(md("small"));
        r.push(md(&"y".repeat(TRANSCRIPT_SOURCE_CAP_BYTES + 1)));
        assert!(r.truncated());
        assert_eq!(r.iter().count(), 0, "oversized entry exceeds any budget");
        assert_eq!(r.total_bytes, 0);
    }

    /// `/clear` empties the registry but the truncation flag survives
    /// (history that left the registry is not resurrected).
    #[test]
    fn clear_resets_entries_keeps_truncated_flag() {
        let mut r = TranscriptSourceRegistry::new();
        for i in 0..1024 {
            r.push(md(&format!("entry-{i:04}-{}", "x".repeat(256))));
        }
        assert!(r.truncated());
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.total_bytes, 0);
        assert!(r.truncated(), "truncation notice must survive /clear");
    }

    /// The pure decision gate: all 16 input combinations.
    #[test]
    fn replay_decision_full_truth_table() {
        for width_changed in [false, true] {
            for streaming in [false, true] {
                for reading in [false, true] {
                    for pending in [false, true] {
                        let verdict = replay_decision(width_changed, streaming, reading, pending);
                        let wanted = width_changed || pending;
                        let expected = if !wanted {
                            ReplayVerdict::Noop
                        } else if streaming || reading {
                            ReplayVerdict::Defer
                        } else {
                            ReplayVerdict::Now
                        };
                        assert_eq!(
                            verdict, expected,
                            "width_changed={width_changed} streaming={streaming} \
                             reading={reading} pending={pending}"
                        );
                    }
                }
            }
        }
    }

    /// Height-only drift is a watchdog mismatch (resize must be
    /// synthesized) even though REPLAY itself ignores height-only changes.
    #[test]
    fn watchdog_size_mismatch_detects_any_component_drift() {
        assert!(!watchdog_size_mismatch((80, 24), (80, 24)));
        assert!(watchdog_size_mismatch((80, 24), (100, 24)), "width drift");
        assert!(watchdog_size_mismatch((80, 24), (80, 40)), "height drift");
        assert!(watchdog_size_mismatch((80, 24), (100, 40)));
    }

    /// Watchdog cadence: never-probed, so due; fresh probe not due; stale
    /// probe due. Pure in `(last, now)` — no sleeps.
    #[test]
    fn watchdog_due_is_interval_gated() {
        let now = Instant::now();
        assert!(watchdog_due(None, now), "first probe is always due");
        assert!(!watchdog_due(Some(now), now), "fresh probe not due");
        assert!(!watchdog_due(
            Some(now - Duration::from_millis(REPLAY_WATCHDOG_INTERVAL_MS - 1)),
            now
        ));
        assert!(watchdog_due(
            Some(now - Duration::from_millis(REPLAY_WATCHDOG_INTERVAL_MS)),
            now
        ));
    }

    /// PreRendered byte accounting sums span content.
    #[test]
    fn prerendered_bytes_sum_span_content() {
        let entry = TranscriptSource::PreRendered(vec![
            Line::from(vec![Span::from("abc"), Span::from("de")]),
            Line::from("xy"),
        ]);
        assert_eq!(entry.source_bytes(), 7);
    }
}
