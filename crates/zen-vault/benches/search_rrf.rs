//! Hot-path micro-benchmarks for the pure search seams (divan, harness=false).
//!
//! PURPOSE: track RRF fusion cost (the shared merge of all 5 search tiers) and
//! tier-selector routing as data shapes grow — the two CPU-pure fns a bench can
//! reach without a SQLite server or LLM. I/O tiers (FTS5/vec0/graph/rg) stay
//! covered by crates/zen/tests/load_harness.rs; this file measures what fusion
//! adds on top of tier output.
//! USAGE:   cargo bench -p zen-vault --bench search_rrf     (full)
//!          cargo bench -p zen-vault --bench search_rrf -- --test   (smoke, 1 iter)
//! EXPECTED: table output (fastest/median/samples); --test exits success fast.
//! ERRORS:  compile error ⇒ a pub seam below changed name/signature — fix the
//!          bench to the new surface, never hide it.
#![allow(clippy::print_stdout)]

use divan::{Bencher, black_box};
use std::path::PathBuf;

use zen_vault::search::{RRF_K, RankedList, SearchResult, TierSelector, reciprocal_rank_fusion};

/// One SearchService tier slice: 4 ranked lists (fts5/vec0/graph/ppr) whose
/// ids are generated so `overlap` fraction of them collide across all lists —
/// the structural escalation fusion exists to exploit (R1 hostile).
fn tier_fixture(tier: &'static str, n: usize, overlap: f64, run: usize) -> RankedList {
    let unique = n.saturating_sub((n as f64 * overlap) as usize);
    let mut results = Vec::with_capacity(n);
    for i in 0..n {
        let (file, line) = if i < unique {
            (format!("bench/{tier}/note-{i}.md"), 10 + (i % 40) as u32)
        } else {
            (format!("bench/shared/note-{i}.md"), 10 + (i % 40) as u32)
        };
        results.push(SearchResult {
            file: PathBuf::from(&file),
            line,
            content: format!("the quick brown fox jumps {i} over the lazy dog {run}"),
        });
    }
    RankedList {
        source: tier,
        weight: 1.0,
        results,
    }
}

// 4-list fused shape at production scale (~50 hits/list): the default TUI
// /search autopilot — every /search call pays fusion once. Fixtures build
// OUTSIDE the loop: this measures fusion, not fixture construction.
#[divan::bench(name = "rrf_4lists_x50_overlap30")]
fn rrf_4lists_x50_overlap30(b: Bencher) {
    const N: usize = 50;
    let lists = [
        tier_fixture("fts5", N, 0.3, 0),
        tier_fixture("vec0", N, 0.3, 0),
        tier_fixture("graph", N, 0.3, 0),
        tier_fixture("ppr", N, 0.3, 1),
    ];
    b.bench(|| reciprocal_rank_fusion(&lists, black_box(RRF_K), black_box(N)))
}

// union bound: zero overlap (worst case for HashMap insert cost) and full
// overlap (worst case for provenance accumulation) — the two cliff edges.
#[divan::bench(name = "rrf_4lists_x200_zero_overlap")]
fn rrf_zero_overlap(b: Bencher) {
    const N: usize = 200;
    let lists = [
        tier_fixture("fts5", N, 0.0, 0),
        tier_fixture("vec0", N, 0.0, 0),
        tier_fixture("graph", N, 0.0, 0),
        tier_fixture("ppr", N, 0.0, 1),
    ];
    b.bench(|| reciprocal_rank_fusion(&lists, black_box(RRF_K), black_box(N)))
}

#[divan::bench(name = "rrf_4lists_x200_full_overlap")]
fn rrf_full_overlap(b: Bencher) {
    const N: usize = 200;
    let lists = [
        tier_fixture("fts5", N, 1.0, 0),
        tier_fixture("vec0", N, 1.0, 0),
        tier_fixture("graph", N, 1.0, 0),
        tier_fixture("ppr", N, 1.0, 1),
    ];
    b.bench(|| reciprocal_rank_fusion(&lists, black_box(RRF_K), black_box(N)))
}

// Query-word routing ladder (L0 heuristic): runs on EVERY turn before any
// tier spins — an accidental O(query^2) here would tax all turns.
#[divan::bench(name = "tier_selector_routing")]
fn tier_selector(b: Bencher) {
    b.bench(|| TierSelector::select_tier(black_box("how do I wire vec0 seam queries fast")))
}

fn main() {
    divan::main();
}
