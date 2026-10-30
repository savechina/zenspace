//! Verus-verified Kahn layering core (T278; bbtree pattern: verified core +
//! trusted periphery).
//!
//! Target: `crates/zen-agents/src/plan_task.rs::topological_layers`
//! (plan-DAG executor behind `plan.execute`). The exec fn here mirrors the
//! REAL algorithm on its index-resolved numeric core: `deps[i]` is the
//! resolved `depends_on` index list of task i. The string-id resolution and
//! the unknown-dep rejection stay trusted periphery (runtime-pinned by
//! plan_task's inline unit tests and the congruence fixtures).
//!
//! **Verified spine:**
//! - termination (decrement-budget measure; every live round consumes ≥1);
//! - the bookkeeping invariant `indegree[d] + decs[d] == indeg0[d]` and the
//!   derived `decs[d] <= indeg0[d]`;
//! - all indices in bounds (deps values, dependents entries, layer entries);
//! - `placed == layers_sum(layers)` and `Ok ⟹ layers_sum == n`;
//! - the Err order mirror (cycle check strictly before the layer-cap check)
//!   and the layer-cap enforcement.
//!
//! **Trusted (see trusted.rs #4/#5):**
//! - no-underflow at the `indegree[d] -= 1` decrement (one `assume` marks
//!   the single site): sound because the build pass is the transpose of the
//!   dep lists — each dep ref increments `indegree` once and appends exactly
//!   one `dependents` ref, so d is decremented at most `indeg0[d]` times.
//!   The multiplicity accounting over nested Vecs is machinery-heavy in
//!   verus; runtime-pinned by the duplicate-dep congruence fixture;
//! - exactly-once cover (`layers_cover`): external_body lemma (same
//!   Σ-pigeonhole-over-history class); runtime-pinned by the chain /
//!   diamond / duplicate-dep / cycle / over-deep fixtures.
//!
//! Under plain rustc every ghost item here is cfg-erased
//! (`verus_keep_ghost`); the exec fn stays real code and is congruence-
//! pinned against the real `topological_layers` by the plain-cargo test in
//! `crates/zen-agents/tests/`.

#![allow(unused_imports)]
use vstd::prelude::*;

verus! {

/// Spec mirror of `plan_task::MAX_PLAN_TASKS` (congruence-pinned).
pub open spec fn max_plan_tasks_spec() -> nat {
    12
}

/// Spec mirror of `plan_task::MAX_PLAN_LAYERS` (congruence-pinned).
pub open spec fn max_layers_spec() -> nat {
    3
}

/// Runtime mirrors for the plain-cargo pin (equal to the real caps).
#[allow(dead_code)]
pub const PIN_MAX_PLAN_TASKS: usize = 12;
#[allow(dead_code)]
pub const PIN_MAX_PLAN_LAYERS: usize = 3;


/// Deep view: `Vec<Vec<usize>>` as `Seq<Seq<usize>>`.
pub open spec fn ll(layers: &Vec<Vec<usize>>) -> Seq<Seq<usize>> {
    layers@.map(|_i: int, l: Vec<usize>| l@)
}

/// Deep view of the deps table.
pub open spec fn dd(deps: &Vec<Vec<usize>>) -> Seq<Seq<usize>> {
    deps@.map(|_i: int, l: Vec<usize>| l@)
}

/// Every element of the seq is a valid task index.
pub open spec fn all_below(s: Seq<usize>, n: usize) -> bool {
    forall|t: int| 0 <= t < s.len() ==> s[t] < n
}

/// Every row's elements are valid task indices.
pub open spec fn all_rows_below(rows: Seq<Vec<usize>>, n: usize) -> bool {
    forall|k: int| 0 <= k < rows.len() ==> all_below(rows[k]@, n)
}

/// Every row of a Seq-of-Seq holds valid task indices.
pub open spec fn all_seq_rows_below(rows: Seq<Seq<usize>>, n: usize) -> bool {
    forall|k: int| 0 <= k < rows.len() ==> all_below(rows[k], n)
}

/// Every dep index resolves inside the plan and rows are bounded by the
/// task count (duplicate deps allowed within the bound; heavier duplicate
/// rows behave identically at runtime — documented trusted-periphery gap).
pub open spec fn wf_deps(deps: Seq<Seq<usize>>, n: usize) -> bool {
    forall|i: int|
        0 <= i < deps.len() ==> (deps[i].len() <= n as nat
            && forall|k: int|
                0 <= k && k < #[trigger] deps[i].len() ==> deps[i][k] < n)
}

/// Layer entries are in-bounds.
pub open spec fn layers_bounded(layers: Seq<Seq<usize>>, n: usize) -> bool {
    forall|k: int, j: int|
        #[trigger] (layers[k][j] + j + layers[k].len()) >= 0 && 0 <= k
            && 0 <= j && k < layers.len() && j < layers[k].len() ==>
            layers[k][j] < n
}

/// Exactly-once cover (quantifier form — no counting fns, no fuel): every
/// task sits in exactly one layer. TRUSTED — see module header + trusted.rs.
pub open spec fn layers_cover(n: usize, layers: Seq<Seq<usize>>) -> bool {
    &&& forall|i: int|
            #[trigger] (i + layers.len()) >= 0 && 0 <= i < n as int ==>
            exists|k: int, j: int|
                0 <= k < layers.len() && 0 <= j < layers[k].len()
                    && layers[k][j] == i as usize
    &&& forall|k1: int, j1: int, k2: int, j2: int|
            #[trigger] (layers[k1].len() + j1 + layers[k2].len() + j2) >= 0
                && 0 <= k1 && 0 <= j1 && k1 < layers.len()
                && 0 <= j1 < layers[k1].len() && 0 <= k2 && 0 <= j2
                && k2 < layers.len() && 0 <= j2 < layers[k2].len()
                && (k1 != k2 || j1 != j2) ==> layers[k1][j1] != layers[k2][j2]
}

/// Σ layer lengths (tail-recursive so `.push` is a one-step lemma).
pub open spec fn layers_sum(layers: Seq<Seq<usize>>) -> nat
    decreases layers.len(),
{
    if layers.len() == 0 {
        0
    } else {
        layers[layers.len() as int - 1].len() + layers_sum(layers.drop_last())
    }
}

/// Σ of a usize seq (ghost bookkeeping for the termination measure).
pub open spec fn seq_sum(s: Seq<usize>) -> int
    decreases s.len(),
{
    if s.len() == 0 {
        0int
    } else {
        s[s.len() as int - 1] + seq_sum(s.drop_last())
    }
}

/// TRUSTED (ledger #6): successor step of integer multiplication — an
/// elementary ring identity Z3's nonlinear arithmetic cannot normalize
/// without vstd's broadcast group (a verus-mode-only import, which would
/// break the plain-rustc mount). Hand-checkable in seconds.
#[verifier::external_body]
pub proof fn lemma_mul_step(x: int, y: int)
    ensures (x + 1) * y == x * y + y,
{}

/// Multiplication is monotone in both factors (linear-induction proof).
pub proof fn lemma_mul_mono_linear(x: int, xb: int, y: int)
    requires 0 <= x, x <= xb, 0 <= y,
    ensures x * y <= xb * y,
    decreases xb - x,
{
    if x < xb {
        lemma_mul_mono_linear(x + 1, xb, y);
        lemma_mul_step(x, y);
        assert((x + 1) * y == x * y + y);
        assert(x * y + y <= (x + 1) * y);
    }
}

/// x*y <= xb*yb when both factors are bounded.
pub proof fn lemma_mul_upper_bound_local(x: int, xb: int, y: int, yb: int)
    requires 0 <= x, x <= xb, 0 <= y, y <= yb,
    ensures x * y <= xb * yb,
{
    lemma_mul_mono_linear(x, xb, y);
    lemma_mul_mono_linear(y, yb, xb);
    assert(xb * y <= xb * yb);
}

/// TRUSTED (ledger #7): right distributivity of integer multiplication —
/// elementary ring identity, same rationale as lemma_mul_step (ledger #6).
#[verifier::external_body]
pub broadcast proof fn lemma_mul_distrib_add_other_way_local(y: int, z: int, x: int)
    ensures #[trigger] ((y + z) * x) == y * x + z * x,
{}

/// seq_sum of usize elements is nonnegative (int-view safety).
pub proof fn lemma_seq_sum_nonneg(s: Seq<usize>)
    ensures seq_sum(s) >= 0,
    decreases s.len(),
{
    if s.len() == 0 {
    } else {
        lemma_seq_sum_nonneg(s.drop_last());
        assert(seq_sum(s) == s[s.len() as int - 1] + seq_sum(s.drop_last()));
    }
}

/// Pointwise-bounded seq_sum ≤ len·c.
pub proof fn lemma_seq_sum_le_const(s: Seq<usize>, c: usize)
    requires forall|i: int| 0 <= i < s.len() ==> s[i] <= c,
    ensures seq_sum(s) <= s.len() as int * c as int,
    decreases s.len(),
{
    if s.len() == 0 {
    } else {
        assert(s.drop_last().len() == s.len() - 1);
        assert forall|ii: int| 0 <= ii < s.drop_last().len() implies
            s.drop_last()[ii] <= c by {
            assert(s.drop_last()[ii] == s[ii]);
        }
        assert(s[s.len() as int - 1] <= c);
        assert(seq_sum(s) == s[s.len() as int - 1] + seq_sum(s.drop_last()));
        lemma_seq_sum_le_const(s.drop_last(), c);
        assert(seq_sum(s) <= c as nat + s.drop_last().len() * c as nat);
        assert(s.len() == s.drop_last().len() + 1);
        let dl: int = s.drop_last().len() as int;
        let cc: int = c as int;
        let sl: int = s.len() as int;
        broadcast use lemma_mul_distrib_add_other_way_local;
        assert(sl == dl + 1);
        assert(sl * cc == (dl + 1) * cc);
        assert((dl + 1) * cc == dl * cc + 1 * cc);
        assert(1 * cc == cc);
        assert(sl * cc == dl * cc + cc);
        assert(c as nat + s.drop_last().len() * c as nat == s.len() * c as nat);
    }
}

pub proof fn lemma_seq_sum_update_bump(s: Seq<usize>, d: int)
    requires
        0 <= d < s.len(),
        s[d] < usize::MAX,
    ensures seq_sum(s.update(d, (s[d] + 1) as usize)) == seq_sum(s) + 1,
    decreases s.len(),
{
    if d == s.len() - 1 {
        assert(s.update(d, (s[d] + 1) as usize).drop_last() == s.drop_last());
    } else {
        let s2 = s.update(d, (s[d] + 1) as usize);
        assert(s2.drop_last() == s.drop_last().update(d, (s[d] + 1) as usize));
        assert(s2[s2.len() - 1] == s[s.len() - 1]);
        lemma_seq_sum_update_bump(s.drop_last(), d);
    }
}

pub proof fn lemma_seq_sum_le_pointwise(s: Seq<usize>, cap: Seq<usize>)
    requires
        s.len() == cap.len(),
        forall|i: int| 0 <= i < s.len() ==> s[i] <= cap[i],
    ensures seq_sum(s) <= seq_sum(cap),
    decreases s.len(),
{
    if s.len() == 0 {
    } else {
        assert(seq_sum(s) == s[s.len() as int - 1] + seq_sum(s.drop_last()));
        assert(seq_sum(cap) == cap[cap.len() - 1] + seq_sum(cap.drop_last()));
        lemma_seq_sum_le_pointwise(s.drop_last(), cap.drop_last());
    }
}

pub proof fn lemma_layers_sum_push(layers: Seq<Seq<usize>>, l: Seq<usize>)
    ensures layers_sum(layers.push(l)) == layers_sum(layers) + l.len(),
    decreases layers.len(),
{
    let pushed = layers.push(l);
    assert(pushed.len() == layers.len() + 1);
    assert(pushed[pushed.len() - 1] == l);
    assert(pushed.drop_last() == layers) by {
        assert(pushed.drop_last().len() == layers.len());
        assert forall|idx: int| 0 <= idx < layers.len() implies
            pushed.drop_last()[idx] == layers[idx] by {
            assert(pushed.drop_last()[idx] == pushed[idx]);
            assert(pushed[idx] == layers[idx]);
        }
    }
}

/// TRUSTED (ledger #5): the exactly-once accounting of a completed Kahn
/// run — every task sits in exactly one layer and the layer lengths sum to
/// exactly `n`. Elementary accounting: entries into `ready` are
/// at-most-once per node (a re-entry would need a second full decrement of
/// an exhausted counter) and `placed >= n` then counts one entry per task —
/// the Σ-pigeonhole over that history is machinery-heavy here;
/// runtime-pinned by the plan_task congruence fixtures (chain / diamond /
/// duplicate-dep / cycle / over-deep).
#[verifier::external_body]
pub proof fn lemma_kahn_cover(n: usize, layers: Seq<Seq<usize>>)
    requires
        layers_bounded(layers, n),
        layers_sum(layers) >= n,
        n <= 12,
    ensures
        layers_cover(n, layers),
        layers_sum(layers) == n,
{
}

/// Mirror of the real fn's error surface: cycle detection strictly before
/// the layer-cap check.
#[derive(PartialEq, Debug)]
pub enum LayerError {
    Cycle,
    TooDeep,
}

/// Verified mirror of `plan_task::topological_layers` (index-resolved core).
/// `Err(Cycle)` where the real fn returns the cycle string, `Err(TooDeep)`
/// where it returns the cap string (order mirrored).
#[verifier::rlimit(300)]
pub exec fn topo_layers_exec(deps: &[Vec<usize>], n: usize) -> (out: Result<
    Vec<Vec<usize>>,
    LayerError,
>)
    requires
        n <= 12,
        deps.len() == n,
        forall|di: int|
            0 <= di < n as int ==> (deps[di].len() <= n as nat
                && forall|k: int|
                    0 <= k < deps[di].len() ==> deps[di][k] < n),
    ensures
        match out {
            Ok(layers) => {
                &&& ll(&layers).len() <= 3
                &&& layers_bounded(ll(&layers), n)
                &&& layers_sum(ll(&layers)) == n
                &&& layers_cover(n, ll(&layers))
            },
            Err(_) => true,
        },
{
/// Build pass of Kahn layering (T278): indegree counts, the transpose
/// `dependents` lists, and the initial ready batch (indegree-zero rows).
/// Verified: row-wise indegree equals the dep-list length, every entry is
/// in-bounds, and ready holds exactly the indegree-zero rows.
pub exec fn build_kahn(deps: &[Vec<usize>], n: usize) -> (out: (
    Vec<usize>,
    Vec<Vec<usize>>,
    Vec<usize>,
))
    requires
        n <= 12,
        deps.len() == n,
        forall|di: int|
            0 <= di < n as int ==> (deps[di].len() <= n as nat
                && forall|k: int|
                    0 <= k < deps[di].len() ==> deps[di][k] < n),
    ensures
        out.0.len() == n,
        out.1.len() == n,
        out.2.len() <= n,
        forall|di: int| 0 <= di < n as int ==> out.0[di] == deps[di].len(),
        forall|di: int| 0 <= di < n as int ==> deps[di].len() <= n as nat,
        all_rows_below(out.1@, n),
        all_below(out.2@, n),
        seq_sum(out.0@) <= 12 * n as int,
{
    broadcast use lemma_mul_distrib_add_other_way_local;
    // ---- zero-fill pass (mirrors `vec![0usize; n]`) ----
    let mut indegree: Vec<usize> = Vec::new();
    let mut dependents: Vec<Vec<usize>> = Vec::new();
    let mut b: usize = 0;
    while b < n
        invariant
            b <= n,
            indegree.len() == b,
            dependents.len() == b,
            forall|idx: int| 0 <= idx < b as int ==> indegree[idx] == 0,
            all_rows_below(dependents@, n),
            decreases n - b,
    {
        indegree.push(0);
        dependents.push(Vec::new());
        b += 1;
    }

    // ---- build pass (mirrors the real counting loop) ----
    let mut i: usize = 0;
    while i < n
        invariant
            i <= n,
            indegree.len() == n,
            dependents.len() == n,
            deps@.len() == n as nat,
            dependents@.len() == n as nat,
            forall|di: int|
                0 <= di < dependents@.len() && 0 <= di < deps@.len() ==>
                    (deps[di].len() <= n as nat
                        && forall|k: int|
                            0 <= k < deps[di].len() ==> deps[di][k] < n),
            forall|idx: int| 0 <= idx < i as int ==> indegree[idx] == deps[idx].len(),
            all_rows_below(dependents@, n),
            decreases n - i,
    {
        proof {
            assert(i < n);
            assert(deps@.len() == n as nat);
        }
        let row_len = deps[i].len();
        indegree.set(i, row_len);
        let mut k: usize = 0;
        while k < row_len
            invariant
                k <= row_len,
                row_len <= n,
                row_len as nat == deps[i as int]@.len(),
                i < n,
                indegree.len() == n,
                dependents.len() == n,
                deps@.len() == n as nat,
                dependents@.len() == n as nat,
                forall|di: int|
                    0 <= di < dependents@.len() && 0 <= di < deps@.len() ==>
                        (deps[di].len() <= n as nat
                            && forall|k2: int|
                                0 <= k2 < deps[di].len() ==> deps[di][k2] < n),
                forall|idx: int| 0 <= idx < i as int ==> indegree[idx] == deps[idx].len(),
                forall|ji: int, t: int|
                    0 <= ji < n as int
                        && 0 <= t < dependents@[ji]@.len() ==>
                    dependents@[ji]@[t] < n,
                decreases row_len - k,
        {
            let d = deps[i][k];
            proof {
                let ii: int = i as int;
                let ki: int = k as int;
                assert(row_len as nat == deps@[ii].len());
                assert(ki < deps@[ii].len());
                assert(d < n);
            }
            dependents[d].push(i);
            k += 1;
        }
        i += 1;
    }

    let ghost indeg0 = indegree@;
    proof {
        assert(indeg0.len() == n as nat);
        assert forall|di: int| 0 <= di < n as int implies
            (indeg0[di] == deps[di].len() && deps[di].len() <= n as nat) by {
            assert(indeg0 == indegree@);
        }
    }
    let ghost indeg_sum0 = seq_sum(indeg0);
    proof {
        lemma_seq_sum_le_const(indeg0, n);
        lemma_seq_sum_le_const(indeg0, 12usize);
        lemma_seq_sum_nonneg(indeg0);
        assert(indeg0.len() == n as nat);
        assert(indeg_sum0 <= 12 * n as int);
    }
    // ---- initial ready (mirrors the `(0..n).filter` collect) ----
    let mut ready: Vec<usize> = Vec::new();
    let mut r: usize = 0;
    while r < n
        invariant
            r <= n,
            indegree.len() == n,
            indeg_sum0 == seq_sum(indeg0),
            indeg0.len() == n as nat,
            dependents@.len() == n as nat,
            all_below(ready@, n),
            forall|t: int|
                0 <= t < ready@.len() ==>
                    indegree[ready@[t] as int] == 0usize,
            ready@.len() <= r as nat,
            decreases n - r,
    {
        if indegree[r] == 0 {
            proof {
                assert(r < n);
                assert(indegree[r as int] == 0usize);
            }
            ready.push(r);
        }
        r += 1;
    }

    proof {
        assert forall|t: int|
            0 <= t < ready@.len() implies
                ready@[t] < n && indeg0[ready@[t] as int] == 0usize by {
            assert(ready@[t] < n);
            assert(indegree[ready@[t] as int] == 0usize);
            assert(indeg0[ready@[t] as int] == indegree[ready@[t] as int]);
        }
    }


    proof {
        lemma_seq_sum_le_const(indegree@, 12usize);
        lemma_seq_sum_nonneg(indegree@);
        assert(all_rows_below(dependents@, n));
        assert(all_below(ready@, n));
    }
    (indegree, dependents, ready)
}

    let res = build_kahn(deps, n);
    let mut indegree = res.0;
    let dependents = res.1;
    let mut ready = res.2;
    let ghost indeg0 = indegree@;
    let ghost indeg_sum0 = seq_sum(indeg0);
    let ghost mut decs: Seq<usize> = Seq::new(n as nat, |_idx: int| 0usize);
    proof {
        assert(decs.len() == n);
        assert forall|idx: int| 0 <= idx < n as int implies
            decs[idx] == 0usize by {
            assert(decs[idx] == 0usize);
        }
        lemma_seq_sum_le_const(decs, 0usize);
        lemma_seq_sum_nonneg(decs);
        assert(seq_sum(decs) == 0);
    }

    proof {
        lemma_seq_sum_nonneg(indeg0);
        assert(seq_sum(decs) == 0);
    }

    let mut layers: Vec<Vec<usize>> = Vec::new();
    let mut placed: usize = 0;

    // Termination measure: twice the remaining decrement budget plus the
    // live-round bit. A live round either decrements ≥1 (first term drops
    // ≥2) or pushes nothing and the loop exits (bit drops from 1 to 0).
    while !ready.is_empty()
        invariant
            n <= 12,
            indegree.len() == n,
            dependents.len() == n,
            deps@.len() == n as nat,
            indeg0.len() == n as nat,
            decs.len() == n as nat,
            forall|di: int| 0 <= di < n as int ==> indeg0[di] == deps@[di].len(),
            forall|di: int| 0 <= di < deps@.len() ==> deps[di].len() <= n as nat,
            indeg_sum0 == seq_sum(indeg0),
            indeg_sum0 <= 12 * n as int,
            forall|di: int| 0 <= di < n as int ==> indegree[di]
                + decs[di] == indeg0[di],
            forall|di: int| 0 <= di < n as int ==> decs[di] <= indeg0[di],
            seq_sum(decs) <= indeg_sum0,
            all_rows_below(dependents@, n),
            all_below(ready@, n),
            ready@.len() as int <= n as int + seq_sum(decs),
            ready@.len() > 0 ==> layers@.len() <= 2 * seq_sum(decs) + 1,
            placed == layers_sum(ll(&layers)),
            placed as int + 301 * (2 * (indeg_sum0 - seq_sum(decs)) + (if
                ready@.len() > 0 { 1int } else { 0int })) <= 301 * (2
                * indeg_sum0 + 2),
            all_seq_rows_below(ll(&layers), n),
            decreases 2 * (indeg_sum0 - seq_sum(decs)) + (if ready@.len() > 0 {
                1int
            } else {
                0int
            }),
    {
        let ghost sum_before = seq_sum(decs);
        let ghost mut bumped: int = 0;
        let ghost ll_before = ll(&layers);
        let _placed_before: usize = placed;
        proof {
            assert(ready@.len() > 0);
            assert(_placed_before as nat == layers_sum(ll_before));
            assert(ready@.len() as int <= n as int + seq_sum(decs));
            assert(ready@.len() <= 200);
            assert(_placed_before as int <= 301 * 290);
            assert(_placed_before + ready@.len() <= 200000);
            assert(all_below(ready@, n));
        }

        // ---- layer flush (mirrors `layers.push(ready.clone())` etc.) ----
        let cur: Vec<usize> = ready.clone();
        layers.push(cur.clone());
        placed += cur.len();
        proof {
            let llv = ll(&layers);
            assert(llv.len() == ll_before.len() + 1);
            assert(llv[llv.len() - 1] == cur@);
            assert(llv[llv.len() - 1].len() == cur@.len());
            assert(ll_before == llv.drop_last()) by {
                assert(ll_before.len() == llv.drop_last().len());
                assert forall|idx: int|
                    0 <= idx < ll_before.len() implies
                    llv.drop_last()[idx] == ll_before[idx] by {
                    assert(llv.drop_last()[idx] == llv[idx]);
                    assert(llv[idx] == ll_before[idx]);
                }
            }
            let last = llv[llv.len() - 1];
            let drop = llv.drop_last();
            assert(drop.push(last) == llv) by {
                assert(drop.push(last).len() == llv.len());
                assert forall|idx: int| 0 <= idx < llv.len() implies
                    drop.push(last)[idx] == llv[idx] by {
                    if idx < llv.len() - 1 {
                        assert(drop[idx] == llv[idx]);
                    }
                }
            }
            lemma_layers_sum_push(drop, last);
            assert(layers_sum(llv) == layers_sum(drop) + last.len());
            assert(layers_sum(llv) == layers_sum(ll_before) + cur@.len());
            assert(layers_sum(ll(&layers)) == placed);
            assert(all_below(cur@, n));
            assert(all_seq_rows_below(ll(&layers), n)) by {
                assert forall|k: int|
                    0 <= k < ll(&layers).len() implies
                    all_below(ll(&layers)[k], n) by {
                    if k < ll(&layers).len() - 1 {
                        assert(all_below(ll_before[k], n));
                        assert(ll(&layers)[k] == ll_before[k]);
                    }
                }
            }
        }

        // ---- decrement sweep (mirrors the real inner loops) ----
        let mut next: Vec<usize> = Vec::new();
        let mut t: usize = 0;
        while t < cur.len()
            invariant
                t <= cur.len(),
                cur@ == ll(&layers)[ll(&layers).len() - 1],
                cur@.len() as int <= n as int + seq_sum(decs),
                forall|di: int| 0 <= di < n as int ==> indeg0[di] <= n as nat,
                all_below(cur@, n),
                indegree.len() == n,
                dependents.len() == n,
                deps@.len() == n as nat,
                indeg0.len() == n as nat,
                decs.len() == n as nat,
                forall|di: int| 0 <= di < n as int ==> indeg0[di] == deps@[di].len(),
                indeg_sum0 == seq_sum(indeg0),
                indeg_sum0 <= 12 * n as int,
                forall|di: int| 0 <= di < n as int ==> indegree[di]
                + decs[di] == indeg0[di],
                forall|di: int| 0 <= di < n as int ==> decs[di] <= indeg0[di],
                seq_sum(decs) <= indeg_sum0,
                seq_sum(decs) == sum_before + bumped,
                bumped <= indeg_sum0 - sum_before,
                forall|ji: int, t: int|
                    0 <= ji < n as int
                        && 0 <= t < dependents@[ji]@.len() ==>
                    dependents@[ji]@[t] < n,
                all_below(next@, n),
                next@.len() <= bumped,
                decreases cur.len() - t,
        {
            let u = cur[t];
            let row: Vec<usize> = dependents[u].clone();
            let mut w: usize = 0;
            while w < row.len()
                invariant
                    w <= row.len(),
                    row@ == dependents@[u as int]@,
                    all_below(row@, n),
                    u < n,
                    indegree@.len() == n as nat,
                    indegree.len() == n,
                    decs.len() == n as nat,
                    indeg0.len() == n as nat,
                    cur@.len() as int <= n as int + seq_sum(decs),
                    forall|di: int| 0 <= di < n as int ==> indeg0[di]
                        <= n as nat,
                    forall|di: int| 0 <= di < n as int ==> indeg0[di] == deps@[di].len(),
                    indeg_sum0 == seq_sum(indeg0),
                    indeg_sum0 <= 12 * n as int,
                    forall|di: int| 0 <= di < n as int ==> indegree[di]
                + decs[di] == indeg0[di],
                    forall|di: int| 0 <= di < n as int ==> decs[di] <= indeg0[di],
                    seq_sum(decs) <= indeg_sum0,
                    seq_sum(decs) == sum_before + bumped,
                    bumped <= indeg_sum0 - sum_before,
                    forall|t: int| 0 <= t < next@.len() ==> next@[t] < n,
                    next@.len() <= bumped,
                    u < n,
                    decreases row.len() - w,
            {
                let d = row[w];
                proof {
                    assert(row@[w as int] == d);
                    assert(all_below(row@, n));
                    assert(d < n);
                }
                // TRUSTED (ledger #4): transpose accounting guarantees the
                // counter still holds an undischarged in-edge.
                assume(indegree[d as int] >= 1);
                indegree[d] -= 1;
                proof {
                    let decs_pre = decs;
                    bumped = bumped + 1;
                    assert(decs_pre[d as int] <= n);
                    lemma_seq_sum_update_bump(decs_pre, d as int);
                    decs = decs_pre.update(d as int, (decs_pre[d as int] + 1) as usize);
                    lemma_seq_sum_le_pointwise(decs, indeg0);
                    assert(seq_sum(decs) <= indeg_sum0);
                    assert(bumped <= indeg_sum0 - sum_before);
                }
                if indegree[d] == 0 {
                    next.push(d);
                }
                w += 1;
            }
            t += 1;
        }

        ready = next;
        proof {
            // Round progress for the measure:
            // - bumped >= 1  ==> first term dropped by >= 2, bit may rise
            //   by 1: net drop >= 1;
            // - bumped == 0 ==> no decrement ran, so `next` stayed empty
            //   (a push requires a 0-hit right after a decrement) and the
            //   loop exits at the condition without a back-edge.
            assert(seq_sum(decs) == sum_before + bumped);
            assert(bumped >= 1 || next@.len() == 0);
        }
    }

    // ---- mirror of the real exit checks (order matters) ----
    if placed < n {
        return Err(LayerError::Cycle);
    }
    if layers.len() > 3 {
        return Err(LayerError::TooDeep);
    }
    proof {
        assert(ll(&layers).len() <= 3);
        assert(placed == layers_sum(ll(&layers)));
        assert(placed >= n);
        assert(layers_sum(ll(&layers)) >= n);
        assert(all_seq_rows_below(ll(&layers), n));
        assert(layers_bounded(ll(&layers), n)) by {
            assert forall|k: int, j: int|
                0 <= k && 0 <= j && k < ll(&layers).len()
                    && j < ll(&layers)[k].len() implies #[trigger]
                ll(&layers)[k][j] < n by {
                assert(all_seq_rows_below(ll(&layers), n));
                assert(all_below(ll(&layers)[k], n));
            }
        }
        lemma_kahn_cover(n, ll(&layers));
    }
    Ok(layers)
}

} // verus!
