//! zen-formal — Verus verification sidecar (bbtree Guard-3 Wave B).
//!
//! NOT a workspace member: no normal build path (`bin/test`, `cargo build
//! --workspace`, `bin/lint`, Linux CI toolchain) ever compiles this crate.
//! It is processed exclusively by `cargo verus verify --manifest-path
//! formal/Cargo.toml` (see .github/workflows/verify.yml) and honored by
//! `bin/verify` locally.
//!
//! Mount discipline (anti-stale-copy): every verified module is the REAL
//! source file mounted via `#[path]` — there is no copy to drift. New
//! proof obligations must be registered in `trusted.rs` (TCB budget).
//!
//! Trusted base (see TRUSTED.md): the digit-rank number-theory lemma and
//! the rd_len bridge are verifier-bypassing trusted entries — both are
//! elementary arithmetic, documented with rationale.

#![cfg_attr(verus_only, allow(unused_imports))]
use vstd::prelude::*;

pub mod trusted;

/// Placehholder grammar proofs — the canonical proof file BOTH mounted
/// here and by `crates/zen-core/tests/formal_placeholder.rs` (runtime pin):
/// one physical source, two consumers.
pub mod secrets_verify;
