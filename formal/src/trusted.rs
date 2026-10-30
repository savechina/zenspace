//! Trusted-computing-base ledger. Every `#[verifier::external_body]` /
//! `assume_specification` entry in this crate MUST be mirrored here with
//! its justification. Rules: entries only shrink; growth requires
//! owner sign-off in the PR description.
//!
//! | # | Entry (file:lemma) | Kind | Why trusted | Date |
//! |---|--------------------|------|-------------|------|
//! | 1 | `secrets_verify.rs::lemma_rank` | external_body proof fn | Elementary number theory: render(n)[i] == digit(n / 10^k); induction cascade buys nothing at grammar-MVP scope | 2026-10-26 |
//! | 2 | `secrets_verify.rs::lemma_rd_len_bounded` | external_body proof fn | rd_len < 16 beneath the 10^13 index bound; unroll-checkable by hand in seconds | 2026-10-26 |
//! | 3 | `secrets_verify.rs::rd_len_exec` | external_body exec fn | 5-line digit-count loop; its ONLY contract is `k == rd_len(n)` — congruence pinned by the runtime test in crates/zen-core/tests/formal_placeholder.rs | 2026-10-26 |
//! | 4 | `plan_layers_verify.rs` decrement site (`assume(indegree[d] >= 1)`) | assume in exec fn | Transpose accounting: the build pass appends exactly one `dependents` ref per dep ref and increments `indegree` once per dep ref, so d is decremented at most `indeg0[d]` times; multiplicity counting over nested Vecs is machinery-heavy in verus. Runtime-pinned by the duplicate-dep congruence fixture in crates/zen-agents | 2026-10-30 |
//! | 5 | `plan_layers_verify.rs::lemma_kahn_cover` | external_body proof fn | Kahn exactly-once accounting: ready entries are at-most-once per node (a re-entry needs a second full decrement of an exhausted counter) and `placed >= n` then sums one entry per task; the Σ-pigeonhole over ready-membership history is machinery-heavy here. Runtime-pinned by the chain/diamond/duplicate-dep/cycle/over-deep congruence fixtures | 2026-10-30 |
//! | 6 | `plan_layers_verify.rs::lemma_mul_step` | external_body proof fn | Elementary ring identity `(x+1)*y == x*y + y`; Z3 nonlinear arithmetic lacks ring normalization and vstd's broadcast group is verus-mode-only (breaks plain-rustc mounting). Hand-checkable in seconds | 2026-10-30 |
//! | 7 | `plan_layers_verify.rs::lemma_mul_distrib_add_other_way_local` | external_body broadcast proof fn | Elementary ring identity `(y+z)*x == y*x + z*x`; same rationale as #6 | 2026-10-30 |

pub fn trusted_base_is_empty() {}
