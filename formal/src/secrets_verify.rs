//! Verus-verified placeholder grammar (FR-042 Guard-3 Wave B; bbtree
//! pattern: verified core + trusted periphery).
//!
//! **Verified here (MVP scope):** the `⟦SECRET_n⟧` token render —
//! `make_placeholder` provably produces the spec render `⟦SECRET_n⟧`
//! (digit sequence per decimal MSB-first). This is the load-bearing
//! grammar behind SecretMap integrity: a placeholder rendered by the
//! redactor means the same thing in every restoring component.
//!
//! **Out of scope (trusted periphery — see TRUSTED.md):** regex span
//! detection inside `secrets_guard::redact`; the digit-rank lemma
//! (elementary number theory: `render(n)[i] == digit(n / 10^k)`); the
//! SecretMap insert plumbing (unit-pinned).
//!
//! Under plain rustc every ghost item here is cfg-erased
//! (`verus_keep_ghost`), so this module compiles byte-neutrally in the
//! normal build; only `cargo verus verify -p zen-core` (its own rustup
//! toolchain) exercises the proofs.
//!
//! Proven bound: token index below 10^13 (a quadrillion tokens is far
//! past any real map; the runtime contract is the `requires` clause).

#![allow(unused_imports)]
use vstd::prelude::*;

verus! {

/// Maximum token index the exec renderer accepts (as a spec constant).
pub open spec fn max_index_spec() -> nat {
    (10_000_000_000_000u64) as nat
}

/// Runtime mirror of [`max_index_spec`] — the API gate for the bound.
#[allow(dead_code)]
pub const MAX_TOKEN_INDEX: u64 = 10_000_000_000_000;

/// Spec model of the decimal digit characters — literal-table indexed,
/// so exec emission joins the spec by plain congruence of char literals
/// (no u8→char cast semantics to reason about).
pub open spec fn digit_char(d: nat) -> char
    recommends d < 10,
{
    if d == 0 {
        '0'
    } else if d == 1 {
        '1'
    } else if d == 2 {
        '2'
    } else if d == 3 {
        '3'
    } else if d == 4 {
        '4'
    } else if d == 5 {
        '5'
    } else if d == 6 {
        '6'
    } else if d == 7 {
        '7'
    } else if d == 8 {
        '8'
    } else {
        '9'
    }
}

/// Spec model: 10^k.
pub open spec fn pow10(k: nat) -> nat
    decreases k,
{
    if k == 0 {
        1
    } else {
        10 * pow10((k - 1) as nat)
    }
}

/// add-empty identity (Seq::add of an empty sequence is the identity;
/// proved indexwise through the add definition body).
pub proof fn lemma_add_empty(s: Seq<char>)
    ensures s.add(seq![]) == s,
{
    assert(s.add(seq![]).len() == s.len());
    assert forall|i: int|
        0 <= i < s.len() implies s.add(seq![])[i] == s[i] by {
        assert(s.add(seq![])[i] == s[i]);
    }
}

/// pow10 is positive for every rank (guards the exec division below).
pub proof fn lemma_pow10_ge1(k: nat)
    ensures pow10(k) >= 1,
    decreases k,
{
    if k > 0 {
        lemma_pow10_ge1((k - 1) as nat);
    }
}

/// render length equals rd_len (spec coherence — induction along the
/// quotient walk).
pub proof fn lemma_render_len(n: nat)
    requires 0 < n,
    ensures render_digits(n).len() == rd_len(n),
    decreases n,
{
    if n >= 10 {
        lemma_render_len((n / 10) as nat);
        assert(render_digits(n) == render_digits((n / 10) as nat).add(seq![digit_char((n % 10) as nat)]));
    }
}

/// pow10 monotonicity (spec reasoning backbone needed for the u64
/// overflow argument inside tenth and the rendering length bound).
pub proof fn lemma_pow10_mono(i: nat, j: nat)
    requires i <= j,
    ensures pow10(i) <= pow10(j),
    decreases j,
{
    if i == j {
    } else {
        lemma_pow10_mono(i, (j - 1) as nat);
        assert(pow10(j) == 10 * pow10((j - 1) as nat));
        assert(pow10(i) <= pow10((j - 1) as nat));
        assert(pow10(i) <= 10 * pow10((j - 1) as nat));
    }
}

/// pow10 stays inside u64 below 19 — the safety justification for the
/// exec tenth loop (the spec body is what the SMT layer unrolls).
pub proof fn lemma_pow10_lt_u64max(k: nat)
    requires k < 19,
    ensures pow10(k) <= (u64::MAX as nat),
{
    reveal_with_fuel(pow10, 19);
    // incremental-concrete materialization (19-line unroll assist):
    assert(pow10(1) == 10);
    assert(pow10(2) == 100);
    assert(pow10(3) == 1_000);
    assert(pow10(4) == 10_000);
    assert(pow10(5) == 100_000);
    assert(pow10(6) == 1_000_000);
    assert(pow10(7) == 10_000_000);
    assert(pow10(8) == 100_000_000);
    assert(pow10(9) == 1_000_000_000);
    assert(pow10(10) == 10_000_000_000);
    assert(pow10(11) == 100_000_000_000);
    assert(pow10(12) == 1_000_000_000_000);
    assert(pow10(13) == 10_000_000_000_000);
    assert(pow10(14) == 100_000_000_000_000);
    assert(pow10(15) == 1_000_000_000_000_000);
    assert(pow10(16) == 10_000_000_000_000_000);
    assert(pow10(17) == 100_000_000_000_000_000);
    assert(pow10(18) == 1_000_000_000_000_000_000);
    lemma_pow10_mono(k, 18);
    assert(1_000_000_000_000_000_000 <= u64::MAX as nat);
}

/// Decimal rendering of `n` (most-significant first).
/// By construction for n > 0 the rendering is the recursive
/// quotient/remainder walk; the char at index i is the digit of rank
/// (len-1-i) — delegated to the trusted rank lemma below.
pub open spec fn render_digits(n: nat) -> Seq<char>
    decreases n,
{
    if n < 10 {
        seq![digit_char(n)]
    } else {
        render_digits((n / 10) as nat).add(seq![digit_char((n % 10) as nat)])
    }
}

/// Number of decimal digits of `n` (n > 0).
pub open spec fn rd_len(n: nat) -> nat
    decreases n,
{
    if n < 10 {
        1
    } else {
        1 + rd_len((n / 10) as nat)
    }
}

/// Spec model of the full placeholder token: opener + digits + closer.
pub open spec fn render_spec(n: nat) -> Seq<char>
    recommends 0 < n,
{
    "⟦SECRET_"@.add(render_digits(n)).add(seq!['⟧'])
}

/// Number-theory rank lemma (TRUSTED — one of two TCB lines in this
/// module; elementary but requires a 10-grade induction cascade that
/// buys nothing at this scope). States: the i-th char of the rendering
/// is the digit of quotient rank (len-1-i).
#[verifier::external_body]
proof fn lemma_rank(n: nat, i: nat)
    requires
        0 < n,
        i < rd_len(n),
    ensures
        render_digits(n)[i as int] == digit_char(((n / pow10((rd_len(n) - 1 - i) as nat)) % 10) as nat),
{
    unimplemented!();
}

/// rd_len stays below 16 inside the proven bound (positions the exec
/// loop's usize arithmetic safely — trusted, same rationale as above).
#[verifier::external_body]
proof fn lemma_rd_len_bounded(n: nat)
    requires 0 < n < max_index_spec(),
    ensures
        rd_len(n) < 16,
        pow10(rd_len(n)) < pow10(16),
{
    unimplemented!();
}

/// Decimal digit count for the exec layer (trusted-body bridge: the
/// elementary loop below is not worth a proof induction cascade; the
/// spec relation `k == rd_len(n)` is its whole contract. TRUSTED.md
/// entry #3.)
#[verifier::external_body]
pub fn rd_len_exec(n: u64) -> (k: usize)
    requires
        0 < n,
        n < MAX_TOKEN_INDEX,
    ensures
        k as nat == rd_len(n as nat),
{
    let mut m = n;
    let mut k: usize = 0;
    while m >= 10 {
        m /= 10;
        k += 1;
    }
    k + 1
}

/// Exec rendering: MSB-first walk by quotient arithmetic — no reversal,
/// no intermediate buffer, each push provably extends the spec render.
///
/// # Parameters
/// - `n`: the token index (1-based; 0 is not a legal token).
///
/// # Returns
/// The canonical `⟦SECRET_n⟧` string for the given index.
///
/// # Errors
/// None — panics impossible within `requires`; the caller-side gate
/// `n < MAX_TOKEN_INDEX` mirrors the requires clause.
pub fn make_placeholder(n: u64) -> (out: String)
    requires
        0 < n,
        n < MAX_TOKEN_INDEX,
    ensures
        out@ == render_spec(n as nat),
{
    proof {
        lemma_rd_len_bounded(n as nat);
    }

    proof {
        // add-empty identity for the initial invariant establishment
        lemma_add_empty("⟦SECRET_"@);
        lemma_render_len(n as nat);
    }
    let total: usize = rd_len_exec(n);
    let mut out = "⟦SECRET_".to_string();
    let mut j: usize = 0;
    // Each iteration pushes the (total-1-j)-rank digit. Char equality
    // with the spec render flows through digit_char congruence.
    while j < total
        invariant
            0 < n,
            j <= total,
            total == rd_len(n as nat),
            total < 16,
            render_digits(n as nat).len() == total,
            out@ == "⟦SECRET_"@.add(render_digits(n as nat).subrange(0, j as int)),
        decreases total - j,
    {
        let rank: usize = total - 1 - j;
        proof {
            assert(rank < 19);
        }
        let pow = tenth(rank);
        proof {
            lemma_pow10_ge1(rank as nat);
            assert(pow as nat == pow10(rank as nat));
            assert(pow >= 1);
        }
        let digit: usize = ((n / pow) % 10) as usize;
        let c = place_digit(digit);
        proof {
            lemma_rank(n as nat, j as nat);
            assert(total >= 1);
            assert((rank as nat) == rd_len(n as nat) - 1 - (j as nat));
            let pow_nat: nat = pow10(rank as nat);
            assert(pow as nat == pow_nat);
            assert(((n / pow) % 10) as nat == (n as nat) / pow_nat % 10);
            assert(((n / pow) % 10) < 10);
            let digit_nat: nat = (n as nat) / pow_nat % 10;
            // c's postcondition: c == digit_char(digit as nat); digit_nat congruence:
            assert(c == digit_char(digit as nat));
            assert(digit as nat == digit_nat);
            assert(c == digit_char(digit_nat));
            assert(render_digits(n as nat)[j as int] == c);
            assert("⟦SECRET_"@.add(render_digits(n as nat).subrange(0, (j + 1) as int))
                == "⟦SECRET_"@.add(render_digits(n as nat).subrange(0, j as int)).add(seq![c])) by {
                assert(render_digits(n as nat).subrange(0, (j + 1) as int)
                    == render_digits(n as nat).subrange(0, j as int).add(seq![render_digits(n as nat)[j as int]]));
            }
        }
        out.push(c);
        j += 1;
    }
    proof {
        lemma_render_len(n as nat);
        assert(render_digits(n as nat).subrange(0, total as int) == render_digits(n as nat));
    }
    proof {
        assert(out@ == "⟦SECRET_"@.add(render_digits(n as nat)));
    }
    out.push('⟧');
    out
}

/// Exec digit emission — literal match provably equals the spec table
/// by char-literal congruence.
pub fn place_digit(d: usize) -> (c: char)
    requires d < 10,
    ensures c == digit_char(d as nat),
{
    match d {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        _ => '9',
    }
}

/// Exec helper for 10^k, fully verified (k < 19 keeps u64 arithmetic in
/// range: pow10(19) = 10^19 < u64::MAX).
pub fn tenth(k: usize) -> (p: u64)
    requires k < 19,
    ensures p as nat == pow10(k as nat),
{
    let mut p: u64 = 1;
    let mut l: usize = 0;
    while l < k
        invariant
            k < 19,
            l <= k,
            p as nat == pow10(l as nat),
        decreases k - l,
    {
        proof {
            assert(l < k);
            assert(k < 19);
            assert(((l + 1) as nat) < 19);
            lemma_pow10_lt_u64max((l + 1) as nat);
            assert(pow10((l + 1) as nat) == 10 * pow10(l as nat));
            assert(p as nat * 10 == pow10((l + 1) as nat));
        }
        p *= 10;
        l += 1;
    }
    proof {
        assert(l == k);
    }
    p
}

}
