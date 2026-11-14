//! Verus-verified LoopBudget accounting (T278 integer-proof trio, goal 3;
//! bbtree Guard-3 pattern: verified core + trusted periphery).
//!
//! **Verified here:** the u32 throughput gates of
//! `zen_vault::distill::types::LoopBudget` —
//! `consume_step` (refuse-only-at-exactly-the-ceiling, never overshoots),
//! `consume_tokens` (saturating acceptance stays ≤ the token ceiling,
//! refusal leaves the counter byte-identical), `over_budget`
//! (spec-equivalence of the gate predicate), and the cycle-level
//! composition theorem: a run of requested step consumptions grants
//! exactly `requested` or stalls at exactly `max_steps` with the counter
//! never exceeding either bound.
//!
//! **Out of scope (trusted periphery — see trusted.rs):** the f64 cost
//! ledger (`charge_cost`) — Verus floating-point support is too thin for a
//! meaningful monotonicity proof, and the cost field is accountability-only
//! by design (it gates nothing; the monthly cap reads it, never the
//! throughput path). The production↔mirror congruence of every exec fn
//! here — including the `saturating_add` overflow edge
//! (`consumed + tokens > u32::MAX` ⇒ accept iff `max_tokens == u32::MAX`)
//! — is pinned by the plain-cargo fixture grid in
//! `crates/zen-vault/tests/formal_loop_budget.rs`.
//!
//! Under plain rustc every ghost item here is cfg-erased
//! (`verus_keep_ghost`), so this module compiles byte-neutrally in the
//! normal build; only `cargo verus verify` exercises the proofs.

#![allow(unused_imports)]
use vstd::prelude::*;

verus! {

/// Mirrors `LoopBudget::consume_step` (the `(bool, u32)` split makes the
/// next-counter value a first-class result so callers can compose without
/// a mutable struct).
///
/// Contract = the production docstring, made mathematical:
/// - granted ⇒ `next == consumed + 1` **and** `next <= max_steps`
///   (the counter can never pass the ceiling);
/// - refused ⇒ the counter is **exactly** `max_steps` and unchanged
///   (refusal happens only at the ceiling, never past it — a stuck gate
///   is always recoverable by raising the ceiling, never by internal
///   overshoot).
pub exec fn consume_step_exec(max_steps: u32, consumed: u32) -> (r: (bool, u32))
    requires
        consumed <= max_steps,
    ensures
        r.0 ==> (r.1 == consumed + 1 && r.1 <= max_steps),
        !r.0 ==> (r.1 == consumed && consumed == max_steps),
{
    if consumed >= max_steps {
        (false, consumed)
    } else {
        // consumed < max_steps ≤ u32::MAX ⇒ consumed + 1 ≤ max_steps, no overflow
        (true, consumed + 1)
    }
}

/// Mirrors `LoopBudget::consume_tokens` including the saturation edge:
/// production computes `consumed.saturating_add(tokens)` and refuses when
/// the saturated sum exceeds the ceiling — so `max_tokens == u32::MAX`
/// with an overflowing sum *accepts* at `u32::MAX`. The mirror reproduces
/// that byte-identically via an overflow-guarded add (congruence-pinned
/// on the overflow fixture).
///
/// Contract: granted ⇒ ceiling respected and the counter is monotone
/// (`consumed <= next <= max_tokens`); refused ⇒ byte-identical state.
pub exec fn consume_tokens_exec(max_tokens: u32, consumed: u32, tokens: u32) -> (r: (bool, u32))
    ensures
        r.0 ==> (consumed <= r.1 && r.1 <= max_tokens),
        !r.0 ==> r.1 == consumed,
{
    // saturating_add mirror: `u32::MAX - tokens` never overflows
    // (tokens ≤ u32::MAX), and the guard makes the true-add overflow-free.
    let next: u32 = if consumed <= u32::MAX - tokens {
        consumed + tokens
    } else {
        u32::MAX
    };
    if next > max_tokens {
        (false, consumed)
    } else {
        (true, next)
    }
}

/// Mirrors `LoopBudget::over_budget`: spec-equivalence of the gate
/// predicate (steps OR tokens at/over ceiling). `>=` on both sides —
/// production semantics preserved verbatim.
pub exec fn over_budget_exec(max_steps: u32, max_tokens: u32, cs: u32, ct: u32) -> (b: bool)
    ensures
        b == (cs >= max_steps || ct >= max_tokens),
{
    cs >= max_steps || ct >= max_tokens
}

/// Cycle-level composition theorem (the state-machine property the
/// pending-pool handoff relies on): driving `requested` step consumptions
/// from a zeroed counter grants either exactly `requested` steps or
/// stalls at exactly `max_steps`; the granted count never exceeds the
/// ceiling regardless of `requested`.
///
/// This is the loop-level consequence the per-step contracts alone do not
/// give: the refusal latch engages exactly once, at the ceiling.
pub exec fn budget_steps_exec(max_steps: u32, requested: u32) -> (granted: u32)
    ensures
        granted <= max_steps,
        granted == requested || granted == max_steps,
    decreases requested,
{
    let mut consumed: u32 = 0;
    let mut granted: u32 = 0;
    let mut stalled = false;
    while granted < requested && !stalled
        invariant
            consumed == granted,
            granted <= requested,
            consumed <= max_steps,
            stalled ==> granted == max_steps,
        decreases (requested - granted) as int + (if stalled { 0int } else { 1int }),
    {
        let (ok, next) = consume_step_exec(max_steps, consumed);
        if ok {
            consumed = next;
            granted += 1;
        } else {
            // contract: refusal only at the ceiling; the invariant folds
            // `!ok ⇒ granted == max_steps` out of the loop (break-path
            // facts don't survive verus loop summarization — the flag does)
            stalled = true;
        }
    }
    granted
}

} // verus!
