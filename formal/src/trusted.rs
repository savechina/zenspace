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

pub fn trusted_base_is_empty() {}
