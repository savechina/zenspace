//! W12.27 — per-session token-budget fair share over the ONE global pool.
//!
//! [`FairShareTokenBudget`] wraps the process-global
//! [`AtomicTokenBudget`](rig_compose::budget::AtomicTokenBudget) with a
//! per-session OUTSTANDING-HOLDS ledger: at every reserve it enforces
//! `outstanding + est ≤ share` with `share = max(capacity / live_holders,
//! MIN_SHARE)` recomputed per call. Only ARMED holds count against the
//! share — consumed tokens never do (this is a hold ceiling, not a
//! cumulative quota), which makes the accounting churn-proof and gives a
//! lone session the whole bucket by construction.
//!
//! Session identity: `zen_plugin::task_registry::current_session_scope()`
//! read at RESERVE time only (fallback `"local"`). The hold's release
//! carries the admission-time key snapshot inside the reservation's own
//! destructor path (T6) — reconcile never re-derives the key from ambient
//! scope. CLI cold processes and tests outside any session scope always
//! resolve `"local"` ⇒ exactly one live holder ⇒ `share == capacity` ⇒
//! decisions are mathematically identical to the raw
//! [`rig_compose::budget::AtomicTokenBudget`] (the outstanding ledger ≡
//! `capacity − available` because holds are the only thing removing
//! availability). No config keys (Constitution D22).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rig_compose::budget::TokenReservation;
use rig_compose::budget::{AtomicTokenBudget, BudgetError, TokenBudget, TokenRefund};
use zen_plugin::task_registry::current_session_scope;

/// Floor on any single session's share of the global token pool (W12.27).
///
/// Rationale (in-code per Constitution D22): the largest legitimate single
/// reserve is ≈ (32k-char delegate gate + 32k-char inherited knowledge)/4
/// plus the 1024 constant ≈ 17k tokens, so 20k guarantees one max-size
/// admission always fits its share even when the pool is crowded.
pub const MIN_SHARE: u64 = 20_000;

/// Session-keyed outstanding-holds ledger (`session key → armed estimate`).
///
/// Entries are removed at zero so the map only ever holds LIVE holders —
/// `len()` is the live-holder count with no churn drift across
/// reserve/reconcile cycles.
type Outstanding = Arc<Mutex<HashMap<String, u64>>>;

/// `rig_compose::budget::TokenBudget` fair-share wrapper (W12.27).
///
/// One global pool ([`AtomicTokenBudget`]) + per-session outstanding-holds
/// ledger. Enforces a per-session HOLD ceiling at each reserve; the global
/// pool remains the single source of truth for availability/consumption
/// (the orchestrator's `budget_available`/`budget_consumed` accessors read
/// it unchanged).
pub struct FairShareTokenBudget {
    inner: Arc<AtomicTokenBudget>,
    outstanding: Outstanding,
}

impl FairShareTokenBudget {
    /// Wrap a fresh global pool with the given capacity.
    pub fn new(capacity: u64) -> Self {
        Self {
            inner: Arc::new(AtomicTokenBudget::new(capacity)),
            outstanding: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Total pool capacity (unchanged accessor surface).
    #[must_use]
    pub fn capacity(&self) -> u64 {
        self.inner.capacity()
    }

    /// Currently available pool tokens (unchanged accessor surface).
    #[must_use]
    pub fn available(&self) -> u64 {
        self.inner.available()
    }

    /// Outstanding armed estimate held by `key` right now (pin/telemetry
    /// accessor; entries are removed at zero, so 0 == no live hold).
    #[must_use]
    pub fn outstanding_of(&self, key: &str) -> u64 {
        self.outstanding
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .copied()
            .unwrap_or(0)
    }
}

/// T6 — one armed hold's self-releasing identity.
///
/// Captured BY VALUE inside the reservation's refund closure, so it
/// travels wherever the reservation travels. Whichever path rig takes —
/// invoking the closure body (armed Drop) or discarding it inside
/// `record_usage` — the release fires exactly once under the ADMISSION
/// key: the body claims the flag first, the destructor claims it
/// otherwise. Reconcile therefore never re-derives the session key from
/// ambient task-local scope (the old shape would book the release under
/// a wrong key if a future spawn path forgot the scope wrap, leaking a
/// phantom live holder that dilutes every session's share forever).
struct HoldState {
    ledger: Outstanding,
    key: String,
    est: u64,
    released: AtomicBool,
}

impl HoldState {
    /// Release the ledger hold under the admission key (single subtract
    /// site; shared by the closure body and the destructor).
    fn release(&self) {
        let mut map = self
            .ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = map.get_mut(&self.key) {
            *entry = entry.saturating_sub(self.est);
            if *entry == 0 {
                map.remove(&self.key);
            }
        }
    }

    /// Claim the exactly-once right to release (closure body wins over
    /// Drop by construction: the body runs while the capture is alive).
    fn claim_release(&self) -> bool {
        !self.released.swap(true, Ordering::AcqRel)
    }
}

impl Drop for HoldState {
    fn drop(&mut self) {
        // Destructor path: the sole releaser when rig discards the
        // closure without invoking it (the record_usage reconcile path).
        if !self.released.load(Ordering::Acquire) {
            self.release();
        }
    }
}

#[async_trait]
impl TokenBudget for FairShareTokenBudget {
    /// Reserve `est` tokens for ONE session's hold: `outstanding + est ≤
    /// share` (share recomputed per call from live holders), then the
    /// global pool. The returned reservation carries a COMBINED refund
    /// closure — global refund AND ledger release — so Drop (early
    /// failure / kill / abort) restores both exactly once.
    async fn try_reserve_tokens(&self, est: u64) -> Result<Option<TokenReservation>, BudgetError> {
        let key = current_session_scope();
        let (my_outstanding, live_others) = {
            let map = self
                .outstanding
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let my = map.get(&key).copied().unwrap_or(0);
            let others = map.values().filter(|v| **v > 0).count();
            (my, others)
        };
        // Entries are removed at zero: my==0 ⇒ absent from the live set.
        let live = (live_others + usize::from(my_outstanding == 0)) as u64;
        let capacity = self.inner.capacity();
        let share = (capacity / live).max(MIN_SHARE);
        if my_outstanding.saturating_add(est) > share {
            return Ok(None);
        }
        let Some(mut res) = self.inner.try_reserve_tokens(est).await? else {
            return Ok(None);
        };
        let est_armed = res.estimate();
        let global_refund: TokenRefund = res
            .disarm()
            .expect("armed reservation from try_reserve_tokens holds its refund closure");
        {
            let mut map = self
                .outstanding
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            map.entry(key.clone())
                .and_modify(|entry| *entry = entry.saturating_add(est_armed))
                .or_insert(est_armed);
        }
        let ledger = Arc::clone(&self.outstanding);
        let hold = HoldState {
            ledger,
            key,
            est: est_armed,
            released: AtomicBool::new(false),
        };
        let combined: TokenRefund = Box::new(move |amount| {
            global_refund(amount);
            if hold.claim_release() {
                hold.release();
            }
        });
        Ok(Some(TokenReservation::new(est_armed, combined)))
    }

    /// Reconcile one reservation against actual usage. The ledger hold
    /// releases via the reservation's own destructor path (T6): rig
    /// discards the refund closure inside its `record_usage`, the
    /// captured [`HoldState`] drops, and its Drop releases under the
    /// ADMISSION key — ambient session scope is never consulted.
    async fn record_usage(&self, reservation: TokenReservation, prompt: u64, completion: u64) {
        self.inner
            .record_usage(reservation, prompt, completion)
            .await;
    }

    /// Cumulative consumption — the global pool's counter (consumed
    /// tokens never count against any session's share).
    async fn tokens_consumed(&self) -> u64 {
        self.inner.tokens_consumed().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn reserve(
        budget: &FairShareTokenBudget,
        key: &str,
        est: u64,
    ) -> Option<TokenReservation> {
        zen_plugin::task_registry::SESSION_SCOPE
            .scope(key.to_string(), budget.try_reserve_tokens(est))
            .await
            .expect("reserve")
    }

    async fn reconcile(
        budget: &FairShareTokenBudget,
        key: &str,
        reservation: TokenReservation,
        prompt: u64,
        completion: u64,
    ) {
        zen_plugin::task_registry::SESSION_SCOPE
            .scope(
                key.to_string(),
                budget.record_usage(reservation, prompt, completion),
            )
            .await;
    }

    /// W12.27 pin 1: a lone session's accept/deny/available decisions are
    /// IDENTICAL to the raw pool — the ledger ≡ `capacity − available`
    /// (holds are the only thing removing availability), so `share ==
    /// capacity` collapses the fair-share check onto the raw check.
    #[tokio::test]
    async fn lone_session_decides_identically_to_raw_budget() {
        let raw = AtomicTokenBudget::new(1_000);
        let fair = FairShareTokenBudget::new(1_000);
        let steps: [u64; 5] = [600, 500, 700, 400, 1_000];
        for (i, est) in steps.iter().enumerate() {
            let raw_res = raw.try_reserve_tokens(*est).await.unwrap();
            let fair_res = reserve(&fair, "solo", *est).await;
            assert_eq!(
                raw_res.is_some(),
                fair_res.is_some(),
                "step {i}: accept/deny must match the raw pool (est={est})"
            );
            assert_eq!(
                raw.available(),
                fair.available(),
                "step {i}: availability must match the raw pool"
            );
            if let (Some(r), Some(f)) = (raw_res, fair_res) {
                raw.record_usage(r, est / 2, est / 4).await;
                reconcile(&fair, "solo", f, est / 2, est / 4).await;
                assert_eq!(
                    raw.available(),
                    fair.available(),
                    "step {i}: reconcile must leave identical availability"
                );
            }
        }
        assert_eq!(fair.outstanding_of("solo"), 0, "no hold survives reconcile");
    }

    /// W12.27 pin 2: a second session can never be locked out — A holds
    /// up to (over) its recomputed share, B still acquires its own share;
    /// B beyond its share is denied; A reconciling frees the whole bucket
    /// for B's retry.
    #[tokio::test]
    async fn second_session_cannot_be_locked_out() {
        let fair = FairShareTokenBudget::new(100_000);
        // A holds 30k while alone (share == capacity == 100k).
        let a = reserve(&fair, "sess-a", 30_000)
            .await
            .expect("A first hold");
        // B arrives: live = 2 ⇒ share = max(50k, 20k) = 50k. B acquires
        // exactly its share despite A's active hold.
        let b = reserve(&fair, "sess-b", 50_000)
            .await
            .expect("B gets its own share");
        assert_eq!(fair.outstanding_of("sess-b"), 50_000);
        // B beyond its share: structured soft-deny.
        let extra = reserve(&fair, "sess-b", 10_000).await;
        assert!(extra.is_none(), "beyond-share hold must soft-deny");
        // A reconciles ⇒ ledger entry removed ⇒ B's retry sees share=100k.
        reconcile(&fair, "sess-a", a, 20_000, 0).await;
        assert_eq!(fair.outstanding_of("sess-a"), 0, "ledger self-cleans");
        let retry = reserve(&fair, "sess-b", 10_000).await;
        assert!(retry.is_some(), "A's reconcile must unblock B's retry");
        drop(retry);
        drop(b);
    }

    /// W12.27 pin 3: reserve/reconcile cycles leave NO ledger entry — a
    /// fresh reserve after churn gets the whole-bucket share (a leaked
    /// entry would turn the 98k reserve below into a fair-share denial).
    #[tokio::test]
    async fn outstanding_ledger_self_cleans_no_churn_drift() {
        let fair = FairShareTokenBudget::new(100_000);
        for _ in 0..5 {
            let res = reserve(&fair, "churn", 5_000).await.expect("hold");
            reconcile(&fair, "churn", res, 2_000, 1_000).await;
            assert_eq!(
                fair.outstanding_of("churn"),
                0,
                "reconcile must release the ledger entry"
            );
        }
        // Whole bucket still available: capacity − consumed (3k × 5).
        let big = reserve(&fair, "churn", 85_000).await;
        assert!(
            big.is_some(),
            "a clean ledger must grant the whole-bucket share: available={}",
            fair.available()
        );
    }

    /// W12.27 pin 4: dropping an armed reservation (early failure path)
    /// refunds the global pool AND removes the ledger entry — both sides
    /// of the combined closure fire exactly once.
    #[tokio::test]
    async fn refund_closure_adjusts_global_and_ledger() {
        let fair = FairShareTokenBudget::new(100_000);
        {
            let res = reserve(&fair, "drop-sess", 5_000).await.expect("hold");
            assert_eq!(fair.available(), 95_000);
            assert_eq!(fair.outstanding_of("drop-sess"), 5_000);
            drop(res);
        }
        assert_eq!(fair.available(), 100_000, "Drop refunds the global pool");
        assert_eq!(
            fair.outstanding_of("drop-sess"),
            0,
            "Drop removes the ledger entry"
        );
    }

    /// W12.27 pin 5: the share is a HOLD ceiling, not a cumulative quota —
    /// a session that consumed and reconciled immediately re-reserves its
    /// full share; consumed tokens never count against it.
    #[tokio::test]
    async fn hold_ceiling_not_cumulative_quota() {
        let fair = FairShareTokenBudget::new(100_000);
        for round in 0..3 {
            let res = reserve(&fair, "quota", 10_000)
                .await
                .unwrap_or_else(|| panic!("round {round}: consumed tokens must not gate holds"));
            reconcile(&fair, "quota", res, 10_000, 0).await;
        }
        assert_eq!(fair.inner.tokens_consumed().await, 30_000);
        let again = reserve(&fair, "quota", 10_000).await;
        assert!(again.is_some(), "hold ceiling only — never a quota");
    }

    /// W12.27 pin 6: abort path — killing a task whose future holds an
    /// armed reservation refunds the global pool AND the ledger; the
    /// refund is asynchronous (lands when tokio drops the aborted
    /// future), so the pin bounded-waits (≤1s, reap-wait discipline).
    #[tokio::test]
    async fn abort_path_restores_global_and_ledger_with_bounded_wait() {
        let fair = Arc::new(FairShareTokenBudget::new(100_000));
        let task_fair = Arc::clone(&fair);
        let handle = tokio::spawn(zen_plugin::task_registry::SESSION_SCOPE.scope(
            "abort-sess".to_string(),
            async move {
                let _armed = task_fair
                    .try_reserve_tokens(7_000)
                    .await
                    .expect("reserve")
                    .expect("armed");
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            },
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(fair.available(), 93_000, "the armed hold pins the pool");
        assert_eq!(fair.outstanding_of("abort-sess"), 7_000);
        handle.abort();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while fair.available() != 100_000 || fair.outstanding_of("abort-sess") != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "abort must restore global pool + ledger (async future Drop)"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// D4 (eng-review 2026-11-20, pin 1 of 2): rig's `record_usage`
    /// consumes the estimate via its own bookkeeping and DISCARDS the
    /// reservation's refund closure without invoking it — the combined
    /// closure must fire exactly once per hold (Drop path only). If a rig
    /// upgrade ever makes `record_usage` invoke the closure, availability
    /// here would bounce above `capacity − consumed` and this pin goes red.
    #[tokio::test]
    async fn record_usage_never_invokes_the_reservation_refund_closure() {
        let fair = FairShareTokenBudget::new(100_000);
        let res = reserve(&fair, "closure-sess", 5_000).await.expect("hold");
        assert_eq!(fair.available(), 95_000);
        // Consume less than the estimate: rig refunds the 2k remainder
        // through its internal counter; the discarded closure must NOT add
        // the full 5k estimate back on top.
        reconcile(&fair, "closure-sess", res, 3_000, 0).await;
        assert_eq!(
            fair.available(),
            97_000,
            "over-reservation refunded once (rig internal), never twice (closure)"
        );
        assert_eq!(fair.outstanding_of("closure-sess"), 0);
        assert_eq!(fair.inner.tokens_consumed().await, 3_000);
        // Est == actual: availability returns to exactly `capacity −
        // consumed`; a closure double-fire would push it past capacity.
        let res = reserve(&fair, "closure-sess", 4_000).await.expect("hold 2");
        assert_eq!(fair.available(), 93_000);
        reconcile(&fair, "closure-sess", res, 2_000, 2_000).await;
        assert_eq!(
            fair.available(),
            93_000,
            "estimate == actual: no refund due"
        );
        assert_eq!(fair.outstanding_of("closure-sess"), 0);
    }

    /// D4 (eng-review 2026-11-20, pin 2 of 2): holds are per-reservation —
    /// reconciling ONE hold of a session must leave that session's OTHER
    /// live hold fully armed (the aggregate ledger decrements by exactly
    /// the reconciled estimate, never more).
    #[tokio::test]
    async fn multi_hold_session_releases_each_hold_independently() {
        let fair = FairShareTokenBudget::new(100_000);
        let hold_a = reserve(&fair, "multi", 5_000).await.expect("hold A");
        let hold_b = reserve(&fair, "multi", 3_000).await.expect("hold B");
        assert_eq!(fair.outstanding_of("multi"), 8_000);
        reconcile(&fair, "multi", hold_a, 2_500, 2_500).await;
        assert_eq!(
            fair.outstanding_of("multi"),
            3_000,
            "hold B stays fully armed after hold A reconciles"
        );
        assert_eq!(fair.available(), 92_000);
        reconcile(&fair, "multi", hold_b, 3_000, 0).await;
        assert_eq!(fair.outstanding_of("multi"), 0);
        assert_eq!(fair.available(), 92_000);
        assert_eq!(fair.inner.tokens_consumed().await, 8_000);
    }

    /// T6 (eng-review 2026-11-20, D4 leftover): the ledger releases
    /// under the ADMISSION key even when reconcile runs OUTSIDE any
    /// session scope — the old shape re-derived the key from ambient
    /// task-local scope here and leaked the hold forever (phantom live
    /// holder ⇒ every later session's share diluted). The hold travels
    /// inside the reservation, so a future spawn path that forgets the
    /// SESSION_SCOPE wrap can no longer corrupt the ledger.
    #[tokio::test]
    async fn reconcile_without_admission_scope_releases_under_admission_key() {
        let fair = FairShareTokenBudget::new(100_000);
        let res = reserve(&fair, "sess-x", 5_000).await.expect("hold");
        assert_eq!(fair.outstanding_of("sess-x"), 5_000);
        // Reconcile with NO session scope in scope: ambient resolves to
        // the "local" fallback — the release must still land on sess-x.
        fair.record_usage(res, 3_000, 0).await;
        assert_eq!(
            fair.outstanding_of("sess-x"),
            0,
            "release must ride the admission key snapshot, not ambient scope"
        );
        assert_eq!(
            fair.available(),
            97_000,
            "rig internal reconcile unaffected: capacity − consumed"
        );
        // No phantom live holder: a fresh lone-session reserve sees the
        // whole-bucket share (a leaked entry would cap share at 50k).
        let big = reserve(&fair, "fresh", 80_000).await;
        assert!(
            big.is_some(),
            "leaked phantom holder would dilute the fresh session's share"
        );
        drop(big);
    }
}
