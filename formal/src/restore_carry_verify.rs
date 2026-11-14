//! Verus-verified restore_incremental carry state machine (T278
//! integer-proof trio, goal 2; bbtree Guard-3 pattern: verified core +
//! trusted periphery).
//!
//! **Verified here:** the two integer invariants of the streaming-restore
//! carry behind `zen_core::secrets_guard::restore_incremental` —
//! 1. the withheld suffix is BOUNDED: a `Some` carry is at most
//!    `MAX_LOOKBACK` (26) bytes, so the incremental forwarder can never
//!    stall a stream by accumulating unbounded pending text; and
//! 2. LENGTH CONSERVATION: `emitted_len + carry_len == total_len` — the
//!    split defers text, it never drops it (the whole-string
//!    `restore_finalize` join sees every byte).
//!
//! The scan mirrors `partial_placeholder_suffix` char-for-char: reverse
//! walk accumulating `len_utf8`, break past the lookback BEFORE testing
//! the predicate, predicate = opener char-prefix OR opener+digit-run.
//! Candidate tails are always char-aligned slices in production
//! (`&text[char_start..end]`), so the char model is exact there.
//!
//! **Out of scope (trusted periphery — see trusted.rs):** the `len_utf8`
//! bridge (entry #8 — a one-line std lookup table); the STRING semantics
//! of the predicate itself (an `is_ascii_digit`-vs-scalar-range nuance
//! and the byte-vs-char prefix equivalence, both pinned by the plain-cargo
//! fixture grid in `crates/zen-core/tests/formal_restore_carry.rs`,
//! including the ⟦-boundary multi-byte and lookback-exhaustion edges);
//! the production `split_off` split arithmetic (pinned by the same
//! fixtures). The theorem needs the predicate only as a black-box
//! trigger — the bound and conservation hold for ANY predicate.
//!
//! Under plain rustc every ghost item here is cfg-erased
//! (`verus_keep_ghost`), so this module compiles byte-neutrally in the
//! normal build; only `cargo verus verify` exercises the proofs.

#![allow(unused_imports)]
use vstd::prelude::*;

verus! {

/// Spec byte-length of a char — the std `char::len_utf8` table
/// (1..=4). Opaque by design; its meaning is fixed by the trusted bridge
/// lemma (trusted.rs entry #8).
pub uninterp spec fn len_utf8_spec(c: char) -> nat;

/// Bridges the opaque `len_utf8_spec` to std semantics — trusted.rs
/// entry #8 (one-line std lookup table, mechanically checkable).
#[allow(dead_code)]
#[verifier::external_body]
pub proof fn lemma_len_utf8_bridge(c: char)
    ensures
        len_utf8_spec(c) == c.len_utf8() as nat,
        c.len_utf8() >= 1,
        c.len_utf8() <= 4,
{}

/// Sum of `len_utf8_spec` over `s[lo..hi)` — the spec model of a
/// char-aligned byte length.
pub closed spec fn sb(s: Seq<char>, lo: nat, hi: nat) -> nat
    decreases hi - lo,
{
    if lo >= hi {
        0
    } else {
        len_utf8_spec(s[(hi - 1) as int]) + sb(s, lo, (hi - 1) as nat)
    }
}

/// Total spec byte length of a char sequence.
pub open spec fn total_bytes(s: Seq<char>) -> nat {
    sb(s, 0, s.len())
}

/// Split of the range sum at an interior boundary: the whole is the
/// prefix plus the suffix (induction down `hi`).
proof fn lemma_sb_split(s: Seq<char>, lo: nat, mid: nat, hi: nat)
    requires
        lo <= mid,
        mid <= hi,
    ensures
        sb(s, lo, hi) == sb(s, lo, mid) + sb(s, mid, hi),
    decreases hi - mid,
{
    if mid == hi {
    } else {
        lemma_sb_split(s, lo, mid, (hi - 1) as nat);
        // sb(lo,hi) = f(s[hi-1]) + sb(lo,hi-1)
        //            = f(s[hi-1]) + sb(lo,mid) + sb(mid,hi-1)
        //            = sb(lo,mid) + sb(mid,hi)   by sb's own defn at mid<hi
        if mid < (hi - 1) as nat {
            assert(sb(s, mid, hi) == len_utf8_spec(s[(hi - 1) as int])
                + sb(s, mid, (hi - 1) as nat));
        } else {
            assert(sb(s, mid, hi) == len_utf8_spec(s[(hi - 1) as int]) + 0);
        }
    }
}

/// A suffix never out-measures the whole (via the split lemma + nat
/// non-negativity).
proof fn lemma_sb_suffix_le(s: Seq<char>, mid: nat)
    requires
        mid <= s.len(),
    ensures
        sb(s, mid, s.len()) <= sb(s, 0, s.len()),
{
    lemma_sb_split(s, 0, mid, s.len());
}

/// Every range sum is at most 4 per char (bridge gives len_utf8 ≤ 4).
proof fn lemma_sb_bounded(s: Seq<char>, lo: nat, hi: nat)
    requires
        lo <= hi,
    ensures
        sb(s, lo, hi) <= 4 * (hi - lo),
    decreases hi - lo,
{
    if lo == hi {
    } else {
        lemma_sb_bounded(s, lo, (hi - 1) as nat);
        assert(sb(s, lo, hi) == len_utf8_spec(s[(hi - 1) as int])
            + sb(s, lo, (hi - 1) as nat));
        lemma_len_utf8_bridge(s[(hi - 1) as int]);
    }
}

/// Appending one char at the LOW end extends the range sum by exactly
/// that char's spec length (sb recurses at the high end, so this needs
/// the split lemma to re-anchor at lo+1).
proof fn lemma_sb_extend_lo(s: Seq<char>, lo: nat, hi: nat)
    requires
        lo < hi,
    ensures
        sb(s, lo, hi) == len_utf8_spec(s[lo as int]) + sb(s, (lo + 1) as nat, hi),
{
    lemma_sb_split(s, lo, (lo + 1) as nat, hi);
    assert(sb(s, lo, lo) == 0);
}

/// The opener `⟦SECRET_` as chars (8 chars = 10 bytes in UTF-8).
pub const OPENER: [char; 8] = ['⟦', 'S', 'E', 'C', 'R', 'E', 'T', '_'];

/// Mirrors the production lookback: `PLACEHOLDER_PREFIX.len() + 16`
/// (10 + 16 = 26 bytes).
pub const MAX_LOOKBACK: usize = 26;

/// ASCII-digit test in scalar space — `0x30..=0x39`, exactly
/// `char::is_ascii_digit` (fixture-pinned on non-digit + non-ASCII chars).
pub exec fn is_digit_exec(c: char) -> (b: bool) {
    (0x30..=0x39).contains(&(c as u32))
}

/// Mirrors the production predicate over `chars[start..n]`:
/// clause A — the tail is a char-prefix of the opener;
/// clause B — the tail starts with the opener and the rest is an ASCII
/// digit run (byte-vs-char equivalence documented in the header).
pub exec fn qualifies_exec(chars: &[char], start: usize) -> (b: bool)
    requires
        start <= chars.len(),
{
    let n = chars.len();
    let tlen = n - start;
    let mut q: usize = 0;
    let mut prefix_ok = tlen <= 8;
    while q < tlen && prefix_ok
        invariant
            start + tlen == n,
            n == chars.len(),
            q <= tlen,
            tlen <= 8 ==> q <= 8,
        decreases tlen - q,
    {
        if q < 8 && chars[start + q] != OPENER[q] {
            prefix_ok = false;
        }
        q += 1;
    }
    // Clause A: full tail (≤ 8 chars) matched the opener prefix-wise.
    if tlen <= 8 && prefix_ok {
        return true;
    }
    // Clause B: opener then digit run. Re-check the 8-char opener
    // (prefix_ok above also covered it when tlen > 8 — every iteration
    // kept matching until q == 8).
    if tlen > 8 {
        let mut r: usize = 0;
        let mut opener_ok = true;
        while r < 8
            invariant
                tlen > 8,
                start + tlen == n,
                n == chars.len(),
                r <= 8,
                opener_ok ==> (forall|i: int| 0 <= i < r ==> chars@[start + i as int] == OPENER[i]),
            decreases 8 - r,
        {
            if chars[start + r] != OPENER[r] {
                opener_ok = false;
            }
            r += 1;
        }
        if opener_ok {
            let mut d: usize = start + 8;
            while d < n
                invariant
                    start + 8 <= d,
                    d <= n,
                    n == chars.len(),
                decreases n - d,
            {
                if !is_digit_exec(chars[d]) {
                    return false;
                }
                d += 1;
            }
            return true;
        }
    }
    false
}

/// The verified mirror of `partial_placeholder_suffix` + the
/// `restore_incremental` split, over the char model of the restored
/// text. Returns `(emitted_len, carry_len)` in bytes.
///
/// Contract = the two carry theorems: a `Some` carry is ≤ 26 bytes and
/// the split conserves the total length; `None` forwards everything.
#[verifier::rlimit(100)]
pub exec fn carry_split_exec(chars: &[char]) -> (r: (usize, Option<usize>))
    requires
        // overflow headroom for the ≤4-per-char accumulators: a
        // 400 MB char slice bound — real restored buffers are KB-scale
        chars.len() <= 100_000_000,
    ensures
        match r.1 {
            Some(k) => {
                &&& k <= MAX_LOOKBACK
                &&& r.0 + k == total_bytes(chars@)
            },
            None => r.0 == total_bytes(chars@),
        },
{
    proof {
        assert(chars.len() <= 100_000_000);
        assert(chars@.len() == chars.len() as nat);
    }
    let n = chars.len();
    proof {
        assert(chars@.len() == n as nat);
    }

    // forward pass: total = Σ len_utf8_spec over all chars
    let mut total: usize = 0;
    let mut t: usize = 0;
    while t < n
        invariant
            n == chars.len(),
            n <= 100_000_000,
            t <= n,
            total == sb(chars@, 0, t as nat),
            total <= 4 * t,
        decreases n - t,
    {
        proof {
            lemma_sb_extend_lo(chars@, t as nat, (t + 1) as nat);
            lemma_len_utf8_bridge(chars@[t as int]);
            assert(sb(chars@, 0, (t + 1) as nat)
                == len_utf8_spec(chars@[t as int]) + sb(chars@, 0, t as nat));
        }
        proof {
            let l = chars@[t as int].len_utf8();
            assert(total + l <= 4 * t + 4);
            assert(t <= 100_000_000);
            assert(4 * t + 4 <= usize::MAX);
            assert(total + l <= usize::MAX);
        }
        total += chars[t].len_utf8();
        t += 1;
    }
    proof {
        lemma_sb_bounded(chars@, 0, n as nat);
    }

    // reverse scan mirroring the production loop
    let mut k: usize = 0;
    let mut j: usize = n;
    let mut stopped = false;
    let mut found = false;
    while j > 0 && !stopped
        invariant
            j <= n,
            n == chars.len(),
            n <= 100_000_000,
            k == sb(chars@, j as nat, n as nat),
            k <= 4 * (n - j),
            total == sb(chars@, 0, n as nat),
            stopped ==> (found ==> (k <= MAX_LOOKBACK)),
            stopped || !found,
        decreases j + (if stopped { 0int } else { 1int }),
    {
        proof {
            lemma_sb_extend_lo(chars@, (j - 1) as nat, n as nat);
            lemma_len_utf8_bridge(chars@[j as int - 1]);
            assert(sb(chars@, (j - 1) as nat, n as nat)
                == len_utf8_spec(chars@[j as int - 1]) + sb(chars@, j as nat, n as nat));
            let l = chars@[j as int - 1].len_utf8();
            assert(k + l <= 4 * (n - j) + 4);
        }
        k += chars[j - 1].len_utf8();
        j -= 1;
        if k > MAX_LOOKBACK {
            stopped = true;
        } else if qualifies_exec(chars, j) {
            stopped = true;
            found = true;
        }
    }

    if found {
        proof {
            // conservation: k is a suffix measure of the total
            lemma_sb_suffix_le(chars@, j as nat);
            assert(k <= total);
        }
        (total - k, Some(k))
    } else {
        (total, None)
    }
}

} // verus!
