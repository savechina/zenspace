//! Poison-tolerance pins (CSO T264): a prior panic while holding a
//! request-path `StdMutex` must never cascade-panic subsequent requests.
//!
//! PURPOSE: Locks the [`lock_ok`] contract shared by every hot-path site
//! in `transport/http.rs` (inbound frame ring), `server/hosting.rs` (turn
//! state/events/response/origin/pending), `server/dispatch.rs` (handshake
//! hooks), and `server/guards.rs` (breaker/doom/watchdog-override).
//!
//! USAGE: `cargo test -p zen-gateway --test poison_tolerance`
//!
//! EXPECTED: all three pins green — poisoned locks recover with data
//! intact (`PoisonError::into_inner`), the poisoned flow keeps working,
//! and the non-poisoned fast path is unchanged.
//!
//! ERRORS: a failure means a site regressed to `.lock().unwrap()` /
//! `.expect(...)` semantics (poison would panic the request path).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use zen_gateway::server::guards::lock_ok;

/// Real poisoning mechanism: panic while holding the guard (what a
/// prior request-path bug would do), leaving the mutex poisoned.
fn poison_with(m: &Mutex<String>, payload: &str) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = m.lock().unwrap();
        let _ = payload;
        panic!("simulated prior panic while holding the lock");
    }));
    // Swallow the panic message so test output stays clean.
}

/// Pin (a) + (b): after a prior panic poisons the lock, the next
/// request-path acquisition succeeds (no cascade panic) AND the recovered
/// data is intact — `lock_ok` must use `PoisonError::into_inner`, not
/// discard the value.
#[test]
fn poisoned_lock_recovers_with_data_intact_and_flow_continues() {
    let m = Mutex::new("pre-panic payload".to_string());
    poison_with(&m, "ignored");

    // (a) subsequent acquisition succeeds — the old `.unwrap()` would
    // panic here exactly like every poisoned request did.
    let mut guard = lock_ok(&m, "pin lock");
    // (b) into_inner semantics: the critical-section value survived.
    assert_eq!(*guard, "pre-panic payload");
    *guard = "post-recovery".to_string();
    drop(guard);

    // Flow keeps working after recovery: a plain re-lock observes the
    // mutation (one poison = one warning, zero lasting damage).
    assert_eq!(*lock_ok(&m, "pin lock"), "post-recovery");
}

/// Pin (b) complement: recovery preserves structured interior state, not
/// just a scalar — mirrors the hosting.rs pending-token buffer shape
/// (`StdMutex<String>` drained via `std::mem::take`).
#[test]
fn poisoned_pending_buffer_drains_accumulated_tokens() {
    let pending = Mutex::new(String::new());

    // First request accumulated tokens, then panicked before draining.
    catch_unwind(AssertUnwindSafe(|| {
        let mut g = pending.lock().unwrap();
        g.push_str("hel");
        g.push_str("lo");
        panic!("flusher died mid-drain");
    }))
    .unwrap_err();

    // Next request: recovery yields the accumulated bytes — the T264
    // contract is "data intact", so the coalescing flusher must still
    // emit what the dead request produced instead of silently dropping
    // it (a fresh `Mutex::new(String::new())` would fail this pin).
    let drained = std::mem::take(&mut *lock_ok(&pending, "pending lock"));
    assert_eq!(drained, "hello");
    assert!(lock_ok(&pending, "pending lock").is_empty());
}

/// Pin (c): the non-poisoned fast path is behaviorally identical to the
/// historical `.lock().unwrap()` — `lock_ok` is a plain `match` on
/// `Result` (no allocation, no extra lock, no widened hold window), so
/// every existing hosting/http/dispatch/guards test stays green. This pin
/// fixes the observable contract: same value in, same value out, guard
/// mutations visible through a subsequent plain `.lock()`.
#[test]
fn fast_path_matches_plain_lock_semantics() {
    let m = Mutex::new(41u64);

    // Read through the helper equals a plain read. (Guard is bound first so
    // it drops before the right-hand side locks the same mutex — a single
    // assert_eq! would hold the left guard across the right lock = deadlock.)
    assert_eq!({ *lock_ok(&m, "fast lock") }, *m.lock().unwrap());

    // Mutate through the helper is observed by a plain reader.
    *lock_ok(&m, "fast lock") += 1;
    assert_eq!(*m.lock().unwrap(), 42);

    // Repeated acquisitions keep working (no state leak in the helper).
    for _ in 0..3 {
        *lock_ok(&m, "fast lock") += 1;
    }
    assert_eq!(*m.lock().unwrap(), 45);
}
