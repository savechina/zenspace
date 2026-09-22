//! T082 (spec 002-agentic-tui): NFR-009 stream-stall instrumentation.
//!
//! # Functionality
//! Measurement ONLY. While a streaming turn is in flight, the TUI records
//! the gap between consecutive stream-delta arrivals; any gap exceeding
//! [`STALL_THRESHOLD_MS`] becomes ONE audit line (`kind: tui.stream.stall`)
//! appended to `<global logs>/audit.jsonl`, surfaced through
//! `zen discover report` → `orchestration.stream_stalls`.
//!
//! # Scope
//! - No UI behavior change, no threshold enforcement, no warning spam —
//!   NFR-008/009 stay advisory until the numbers justify a calibrated gate.
//! - The 200 ms stall definition (Eloquent, arXiv:2401.12961) is a constant
//!   on purpose: advisory measurement has no config knob surface.
//! - Debounce: one line per stall EPISODE. A continuous stall produces a
//!   single inter-token gap, measured once when the next delta arrives (or
//!   at turn end); polling ticks never emit extra lines.
//!
//! # Errors
//! The audit append is fire-and-forget: I/O failures log a `tracing::warn!`
//! and never fail the turn (same convention as the gateway `SessionHost::audit`
//! and orchestrator `loop.turn.review` writers).

use std::time::Instant;

/// NFR-009 (spec 002-agentic-tui): an inter-token gap >200 ms is a stall.
/// T082 keeps this advisory — measurement precedes assertion (SC-004
/// lesson) — so the threshold is deliberately NOT configurable.
pub(crate) const STALL_THRESHOLD_MS: u64 = 200;

/// Pure NFR-009 episode filter: of the consecutive inter-token gaps (ms),
/// return the ones that constitute a stall episode (> [`STALL_THRESHOLD_MS`],
/// one entry per gap — a resumed-then-restalled turn yields multiple).
/// Feeding `[10, 250, 30, 900, 5]` returns `[250, 900]`.
pub(crate) fn stall_episode_gaps(gaps_ms: &[u64]) -> Vec<u64> {
    gaps_ms
        .iter()
        .copied()
        .filter(|gap| *gap > STALL_THRESHOLD_MS)
        .collect()
}

/// Per-turn tracker of inter-token arrival gaps. Owned by
/// [`super::stream::StreamCollector`]; reset via [`StreamCollector::clear`]
/// between turns.
#[derive(Debug, Default)]
pub(crate) struct StallTracker {
    /// Arrival instant of the most recent delta; `None` before the first
    /// delta of a turn (the turn-start gap is not an inter-token gap).
    last_delta_at: Option<Instant>,
    /// Measured stall gaps awaiting their audit line.
    pending_gaps: Vec<u64>,
}

impl StallTracker {
    /// Records one delta arrival at instant `now`; a gap since the previous
    /// arrival exceeding the threshold is queued as one stall episode.
    /// Instant is injected for deterministic tests.
    pub(crate) fn note_delta_at(&mut self, now: Instant) {
        if let Some(last) = self.last_delta_at {
            let gap = now.saturating_duration_since(last).as_millis() as u64;
            self.pending_gaps.extend(stall_episode_gaps(&[gap]));
        }
        self.last_delta_at = Some(now);
    }

    /// Convenience wrapper over [`Self::note_delta_at`] for the live path.
    pub(crate) fn note_delta(&mut self) {
        self.note_delta_at(Instant::now());
    }

    /// Turn ended: a trailing gap (last delta → now) still counts as one
    /// final episode — measured once, never re-emitted by later ticks.
    pub(crate) fn flush_turn_end_at(&mut self, now: Instant) {
        if let Some(last) = self.last_delta_at.take() {
            let gap = now.saturating_duration_since(last).as_millis() as u64;
            self.pending_gaps.extend(stall_episode_gaps(&[gap]));
        }
    }

    /// Drains the stall gaps awaiting their audit line.
    pub(crate) fn take_pending_gaps(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.pending_gaps)
    }

    /// Resets for a new turn (called from `StreamCollector::clear`).
    pub(crate) fn reset(&mut self) {
        self.last_delta_at = None;
        self.pending_gaps.clear();
    }
}

/// Appends ONE `tui.stream.stall` line to `<global logs>/audit.jsonl`.
///
/// Line shape matches the gateway audit convention (`ts` = epoch-millis
/// string, `kind` discriminator, payload fields):
/// `{"gap_ms":N,"kind":"tui.stream.stall","session_id":"…","ts":"…","turn_id":"…"}`
pub(crate) fn append_stall_audit(session_id: &str, turn_id: &str, gap_ms: u64) {
    let Ok(paths) = zen_core::paths::ZenPaths::detect() else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let entry = serde_json::json!({
        "gap_ms": gap_ms,
        "kind": "tui.stream.stall",
        "session_id": session_id,
        "ts": ts,
        "turn_id": turn_id,
    });
    let log_path = paths.logs().join("audit.jsonl");
    let write = || -> std::io::Result<()> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        use std::io::Write as _;
        writeln!(f, "{entry}")
    };
    if let Err(e) = write() {
        tracing::warn!(path = %log_path.display(), "stall audit write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    /// Task-specified sequence: gaps [10, 250, 30, 900, 5] → 2 episodes
    /// (250 + 900 recorded), max 900.
    #[test]
    fn episode_filter_matches_task_spec() {
        let episodes = stall_episode_gaps(&[10, 250, 30, 900, 5]);
        assert_eq!(episodes, vec![250, 900]);
        assert_eq!(episodes.iter().max(), Some(&900));
    }

    #[test]
    fn threshold_is_strictly_greater() {
        assert!(
            stall_episode_gaps(&[200]).is_empty(),
            "200 ms is NOT a stall"
        );
        assert_eq!(stall_episode_gaps(&[201]), vec![201]);
    }

    #[test]
    fn tracker_records_gap_only_when_next_delta_arrives() {
        let mut tracker = StallTracker::default();
        // t=0 first delta (no gap), t=10 flow, t=260 → gap 250 recorded once.
        tracker.note_delta_at(at(0));
        tracker.note_delta_at(at(10));
        assert!(tracker.take_pending_gaps().is_empty());
        tracker.note_delta_at(at(260));
        assert_eq!(tracker.take_pending_gaps(), vec![250]);
        // Debounced: draining again yields nothing (no per-tick spam).
        assert!(tracker.take_pending_gaps().is_empty());
    }

    #[test]
    fn continuous_stall_is_one_episode_not_one_per_tick() {
        let mut tracker = StallTracker::default();
        tracker.note_delta_at(at(0));
        // 600 ms of silence, then ONE delta. Only one gap exists.
        tracker.note_delta_at(at(600));
        assert_eq!(tracker.take_pending_gaps(), vec![600]);
    }

    #[test]
    fn turn_end_flushes_trailing_gap_once() {
        let mut tracker = StallTracker::default();
        tracker.note_delta_at(at(0));
        tracker.flush_turn_end_at(at(450));
        assert_eq!(tracker.take_pending_gaps(), vec![450]);
        // Consumed: a second flush (late tick) emits nothing.
        tracker.flush_turn_end_at(at(900));
        assert!(tracker.take_pending_gaps().is_empty());
    }

    #[test]
    fn sub_threshold_trailing_gap_is_not_a_stall() {
        let mut tracker = StallTracker::default();
        tracker.note_delta_at(at(0));
        tracker.flush_turn_end_at(at(150));
        assert!(tracker.take_pending_gaps().is_empty());
    }

    #[test]
    fn first_delta_never_produces_a_gap_and_reset_clears_state() {
        let mut tracker = StallTracker::default();
        tracker.flush_turn_end_at(at(5_000)); // no delta yet → no-op
        assert!(tracker.take_pending_gaps().is_empty());
        tracker.note_delta_at(at(0));
        tracker.note_delta_at(at(1_000));
        tracker.reset();
        assert!(tracker.take_pending_gaps().is_empty());
        // After reset the old anchor is gone: next delta starts a fresh turn.
        tracker.note_delta_at(at(1_010));
        assert!(tracker.take_pending_gaps().is_empty());
    }
}
