//! Retention strength — pure Ebbinghaus forgetting-curve model (Phase 31 T207,
//! design `docs/designs/memory-strength-knowledge-wave.md` Feature 1).
//!
//! Strength is NEVER persisted: it is a pure function of persisted inputs
//! (content anchor, last reinforcement event, event count) evaluated lazily at
//! read time. Because nothing accumulates, the T153 compounding-decay bug
//! class (a per-cycle re-application of a decay factor to a stored value) is
//! structurally impossible — the mandatory idempotency tests below pin it.
//!
//! This module performs NO I/O and reads NO config: every input is injected by
//! the caller (later waves gather them from frame metadata, file mtimes, and
//! the `MemoryReward` sidecars, and gate on
//! `[agentic.memory_strength] enabled` / `search_rerank`).

use chrono::{DateTime, Utc};

/// Seconds in one day — unit conversion for the day-fractional exponent.
const SECONDS_PER_DAY: f64 = 86_400.0;

/// Retention strength `R ∈ (0, 1]` of one memory (design F1-D3):
///
/// ```text
/// R = 0.5 ^ ( days_since(effective_anchor)
///             / (half_life_days × (1 + log2(1 + events))) )
/// effective_anchor = max(anchor, last_event.unwrap_or(anchor))
/// ```
///
/// - `anchor` — the content anchor from [`resolve_content_anchor`] (authored
///   date preferred over index date).
/// - `last_event` — the most recent reinforcement (the reward sidecar's
///   `last_reward_at`); the elapsed time is measured from the LATER of anchor
///   and last event, mirroring the `belief.rs` anchor discipline.
/// - `events` — reinforcement count = `access_count + downstream_citations`
///   from the `MemoryReward` sidecar. `correction_count` is DELIBERATELY
///   EXCLUDED: corrections signal ERRONEOUS memories (FR-034 semantics) — a
///   corrected memory is not a strengthened one, and subtracting corrections
///   would require an invented penalty weight (design F1-D5). Excluding is the
///   conservative reading. The `1 + log2(1 + n)` stability multiplier is
///   structural, not tuned: each doubling of reinforcement events adds one
///   half-life.
/// - `half_life_days` — MUST be > 0; callers pass
///   `MemoryStrengthConfig::half_life_days_or_default()` (validated/clamped
///   1.0..=365.0 there). This function additionally guards defensively with
///   `max(half_life_days, f64::EPSILON)` so garbage input can never produce a
///   division by zero or NaN — with the epsilon floor such input decays to the
///   `f64::MIN_POSITIVE` bound instead of poisoning rankings.
/// - Future anchors (clock skew) clamp elapsed to 0 ⇒ `R = 1.0`
///   (`recency_weight` precedent, `memvid_index.rs`); the result is always
///   finite and in `(0, 1]` — the lower bound on float underflow is
///   `f64::MIN_POSITIVE` (structural, not a tuned floor).
///
/// # Examples
///
/// ```
/// use chrono::{DateTime, TimeDelta, Utc};
/// use zen_memory::strength::retention_strength;
///
/// let now = Utc::now();
/// let anchor = now - TimeDelta::try_days(30).unwrap();
/// // One half-life old, no reinforcement ⇒ R = 0.5.
/// let r = retention_strength(anchor, None, 0, now, 30.0);
/// assert!((r - 0.5).abs() < 1e-9);
/// // Two reinforcement events triple the stability (1 + log2(3)) ⇒ slower decay.
/// let reinforced = retention_strength(anchor, None, 2, now, 30.0);
/// assert!(reinforced > r);
/// ```
pub fn retention_strength(
    anchor: DateTime<Utc>,
    last_event: Option<DateTime<Utc>>,
    events: u32,
    now: DateTime<Utc>,
    half_life_days: f64,
) -> f64 {
    let effective_anchor = last_event.map_or(anchor, |last| last.max(anchor));
    // Clock skew (anchor in the future) ⇒ age 0 ⇒ R = 1.0, never negative.
    let elapsed_days = ((now - effective_anchor).num_seconds() as f64 / SECONDS_PER_DAY).max(0.0);
    // Defensive guard: the config accessor validates > 0; `f64::max` also
    // maps NaN to the epsilon (Rust semantics: the non-NaN operand wins).
    let half_life = half_life_days.max(f64::EPSILON);
    let stability_days = half_life * (1.0 + (1.0 + f64::from(events)).log2());
    let r = 0.5_f64.powf(elapsed_days / stability_days);
    if r.is_finite() {
        r.clamp(f64::MIN_POSITIVE, 1.0)
    } else {
        1.0
    }
}

/// Resolve the content anchor of one indexed memory (design OQ-4/F1).
///
/// Precedence: journal-uri date (the AUTHORED date parsed from a
/// `journal-YYYY-MM-DD` frame uri) > source-file mtime (via the frame's
/// `extra_metadata.source_path`) > frame `created_at` (the INDEX time — wrong
/// for old content indexed later, hence last). Legacy frames without
/// provenance metadata fall through to `frame_created_at` with no error.
pub fn resolve_content_anchor(
    journal_uri_date: Option<DateTime<Utc>>,
    source_file_mtime: Option<DateTime<Utc>>,
    frame_created_at: DateTime<Utc>,
) -> DateTime<Utc> {
    journal_uri_date
        .or(source_file_mtime)
        .unwrap_or(frame_created_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn ts(days_from_epoch: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(days_from_epoch * 86_400, 0).unwrap()
    }

    // ── Idempotency (MANDATORY, T153 lesson) ─────────────────────────

    #[test]
    fn repeated_evaluation_at_fixed_now_is_identical() {
        let (anchor, now) = (ts(100), ts(200));
        let first = retention_strength(anchor, None, 3, now, 30.0);
        for _ in 0..10 {
            assert_eq!(
                retention_strength(anchor, None, 3, now, 30.0).to_bits(),
                first.to_bits()
            );
        }
    }

    #[test]
    fn stepped_now_advances_equal_single_evaluation_no_compounding() {
        // N "cycles" of advancing now by Δ and re-evaluating must equal ONE
        // evaluation at now + NΔ — a stored/compounded decay (the T153 bug
        // class) would diverge geometrically.
        let anchor = ts(0);
        let now = ts(1000);
        let delta = TimeDelta::try_days(7).unwrap();

        let mut stepped_now = now;
        let mut stepped = retention_strength(anchor, None, 2, stepped_now, 30.0);
        for _ in 0..5 {
            stepped_now += delta;
            stepped = retention_strength(anchor, None, 2, stepped_now, 30.0);
        }
        let single = retention_strength(anchor, None, 2, now + delta * 5, 30.0);
        assert_eq!(stepped.to_bits(), single.to_bits());
    }

    // ── Monotonicity ─────────────────────────────────────────────────

    #[test]
    fn strength_strictly_decreases_with_age_at_zero_events() {
        let anchor = ts(0);
        let hl = 30.0;
        let mut prev = retention_strength(anchor, None, 0, ts(10), hl);
        for age_days in [20, 40, 80, 160, 400, 1000] {
            let r = retention_strength(anchor, None, 0, ts(age_days), hl);
            assert!(r < prev, "age {age_days}: {r} >= {prev}");
            prev = r;
        }
    }

    #[test]
    fn strength_increases_with_events_at_fixed_age() {
        let (anchor, now) = (ts(0), ts(90));
        let mut prev = retention_strength(anchor, None, 0, now, 30.0);
        for events in [1u32, 3, 7, 15, 100] {
            let r = retention_strength(anchor, None, events, now, 30.0);
            assert!(r > prev, "events {events}: {r} <= {prev}");
            prev = r;
        }
    }

    // ── Formula anchors (reuse of the recorded 30-day half-life) ─────

    #[test]
    fn one_half_life_at_zero_events_is_exactly_one_half() {
        let r = retention_strength(ts(0), None, 0, ts(30), 30.0);
        assert!((r - 0.5).abs() < 1e-12);
        let r2 = retention_strength(ts(0), None, 0, ts(60), 30.0);
        assert!((r2 - 0.25).abs() < 1e-12);
    }

    #[test]
    fn stability_multiplier_follows_log2_shape() {
        // events=1 ⇒ stability 2×HL ⇒ age HL gives 0.5^(1/2).
        let r = retention_strength(ts(0), None, 1, ts(30), 30.0);
        assert!((r - 0.5_f64.powf(0.5)).abs() < 1e-12);
        // events=3 ⇒ stability 3×HL ⇒ age 3×HL gives 0.5.
        let r3 = retention_strength(ts(0), None, 3, ts(90), 30.0);
        assert!((r3 - 0.5).abs() < 1e-12);
    }

    // ── Bounds ───────────────────────────────────────────────────────

    #[test]
    fn result_always_in_open_lower_closed_upper_bounds() {
        let cases = [
            (ts(0), None, 0u32, ts(0)),
            (ts(0), None, 0, ts(40_000)), // ~109 years ⇒ underflow path
            (ts(0), Some(ts(10)), 7, ts(500)), // reinforced
            (ts(100), None, 0, ts(0)),    // future anchor (clock skew)
            (ts(0), Some(ts(500)), 0, ts(100)), // last_event in the future
            (ts(0), None, u32::MAX, ts(36_500)), // max events, a century old
        ];
        for (anchor, last, events, now) in cases {
            for hl in [1.0, 30.0, 365.0] {
                let r = retention_strength(anchor, last, events, now, hl);
                assert!(r > 0.0 && r <= 1.0, "R={r} out of (0,1]");
                assert!(r.is_finite());
            }
        }
    }

    #[test]
    fn future_anchor_clamps_to_one() {
        let r = retention_strength(ts(100), None, 0, ts(0), 30.0);
        assert_eq!(r, 1.0);
    }

    // ── Anchor semantics ─────────────────────────────────────────────

    #[test]
    fn last_event_newer_than_anchor_moves_the_anchor() {
        // Reinforcing an old memory restarts decay from the reinforcement.
        let with_event = retention_strength(ts(0), Some(ts(90)), 5, ts(100), 30.0);
        let anchored_at_event = retention_strength(ts(90), None, 5, ts(100), 30.0);
        assert_eq!(with_event.to_bits(), anchored_at_event.to_bits());
        assert!(with_event > retention_strength(ts(0), None, 5, ts(100), 30.0));
    }

    #[test]
    fn last_event_older_than_anchor_is_ignored_max_semantics() {
        let with_old_event = retention_strength(ts(100), Some(ts(10)), 5, ts(200), 30.0);
        let anchor_only = retention_strength(ts(100), None, 5, ts(200), 30.0);
        assert_eq!(with_old_event.to_bits(), anchor_only.to_bits());
    }

    #[test]
    fn resolve_content_anchor_precedence() {
        let (uri_date, mtime, created) = (ts(10), ts(20), ts(30));
        assert_eq!(
            resolve_content_anchor(Some(uri_date), Some(mtime), created),
            uri_date
        );
        assert_eq!(resolve_content_anchor(None, Some(mtime), created), mtime);
        assert_eq!(resolve_content_anchor(None, None, created), created);
    }

    // ── Defensive half-life guard ────────────────────────────────────

    #[test]
    fn non_positive_or_nan_half_life_never_panics_or_returns_nan() {
        let now = ts(100);
        for hl in [0.0, -30.0, f64::NAN] {
            let r = retention_strength(ts(0), None, 0, now, hl);
            assert!(r.is_finite(), "hl={hl} produced {r}");
            assert!(r > 0.0 && r <= 1.0, "hl={hl} produced {r}");
        }
        // An infinite half-life means "never forget" — the exponent goes to 0.
        assert_eq!(retention_strength(ts(0), None, 0, now, f64::INFINITY), 1.0);
    }
}
