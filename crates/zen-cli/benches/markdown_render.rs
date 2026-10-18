//! Hot-path micro-benchmarks for markdown rendering (divan, harness=false).
//!
//! PURPOSE: track the two render entry points a streaming chat session hammers
//! — one-shot `render_markdown` (committed-block path) and
//! `StreamingMarkdown::append` (the live drain/commit pipeline). These are the
//! costs every turn pays while tokens arrive; a regression here is user-visible
//! keystroke-to-render jank (NFR-008's sibling).
//! USAGE:   cargo bench -p zen-cli --features zen-cli/bench --bench markdown_render
//!          ... append `-- --test` for the smoke form (portable_pty doors).
//! EXPECTED: table output; --test exits success without measuring.
//! ERRORS:  compile error ⇒ markdown.rs pub seam moved — retarget the bench.

use divan::Bencher;
use zen_cli::tui::markdown::{StreamingMarkdown, render_markdown};

/// ~120-line doc covering every block kind render_markdown dispatches on:
/// paragraphs, a CJK-heavy section, a rust code fence (syntect highlight is
/// the dominant per-line cost), and a diff fence (per-line +/- gate path).
fn doc_fixture() -> String {
    let mut s = String::with_capacity(8_192);
    for i in 0..10 {
        s.push_str(&format!("## Section {i}\n\n"));
        for j in 0..4 {
            s.push_str(&format!(
                "Paragraph {j} with a longer wrapper line about 知识检索 seam {i} — \
                 second sentence to exercise wrapping width overflow behaviour.\n\n"
            ));
        }
        s.push_str("( o.o )\n( >^< ) 面包屑行\n\n");
        s.push_str("| 键 | 值 | 备注 |\n|---|---|----|\n| k1 | v1 | CJK 列宽锚点 |\n\n");
    }
    s.push_str("```rust\n");
    s.push_str("fn bench_target(b: Bencher) {\n");
    for i in 0..60 {
        s.push_str(&format!(
            "    let x{i} = black_box(x{i}) + x{i} * 2; // line {i:02}\n"
        ));
    }
    s.push_str("}\n");
    s.push_str("```\n\n```diff\n");
    for i in 0..10 {
        s.push_str(&format!(
            "@@ -1,3 +1,4 @@\n+added line {i:02}\n-removed line {i:02}\n"
        ));
    }
    s.push_str("```\n");
    s
}

/// One-shot committed-block render (every /help, /model ack, block commit pays
/// this once).
#[divan::bench(name = "render_markdown_commits_one_shot")]
fn render_markdown_one_shot(b: Bencher) {
    let doc = doc_fixture();
    b.bench(|| render_markdown(&doc))
}

/// The live pipeline: 60 token chunks (~tea) — each append re-drains committed
/// cache + re-renders the pending tail, the path every streaming turn pays.
#[divan::bench(name = "streaming_append_60_chunks")]
fn streaming_append(b: Bencher) {
    // Fixture build + char split live OUTSIDE the timed closure — only the
    // StreamingMarkdown state must be fresh per iteration.
    let doc = doc_fixture();
    // 60 char-aligned token bursts — the real stream arrives on char
    // boundaries; chunks() would slice into CJK multibyte chars (UB).
    let chars: Vec<char> = doc.chars().collect();
    let per = chars.len().div_ceil(60);
    b.bench(|| {
        let mut sm = StreamingMarkdown::new();
        for group in chars.chunks(per) {
            let tail: String = group.iter().collect();
            let _ = sm.append(&tail);
        }
    })
}

fn main() {
    divan::main();
}
