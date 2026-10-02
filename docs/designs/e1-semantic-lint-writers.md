---
status: ACCEPTED
---
# E1 Decision Record: Semantic Lint Writers Analysis

Compile-hygiene workstream E1 (docs/designs/openkb-compile-hygiene.md) requires,
BEFORE any code: a grep-verified writer inventory of the wiki surfaces plus a
decision record proving the semantic lint agent introduces no second-writer
conflict (the T129/T130 precedent, Anti-Pattern Guardrail #2).

## Writer inventory (grep-verified 2026-09-28; re-verified 2026-10-02)

### wiki/wisdom/* (M4 wisdom surfaces)

| Writer | File:line | Target |
|---|---|---|
| memory_curator | zen-agents/src/scheduler/workers/memory_curator.rs:370,376 | wiki/wisdom/decisions/ |
| | :389 | wiki/wisdom/corrections/ |
| | :402 | wiki/wisdom/feedback/ |
| | :418,424 | wiki/wisdom/beliefs/ |
| | :436 | wiki/wisdom/facts/ |
| | :448,452 | memories/commitments/ |
| | :475,479 | wiki/wisdom/anti-patterns/ |
| | :499,503 | wiki/wisdom/models/ |
| | :509,540-545 | memories/virtue_logs/ |
| session_journaler_signals | session_journaler/session_journaler_signals.rs:772,787,802,821,832 | wiki/wisdom/{decisions,corrections,feedback,beliefs,facts}/ |
| | :857-862 (AtomicWikiWriter :858) | wiki/wisdom/preferences/ |
| | :742-746 | wiki/wisdom/anti-patterns/ |
| wisdom_synth | wisdom_synth.rs:84 | wiki/wisdom/beliefs/ (decay re-save) |
| | :265-281 | wiki/wisdom/suggestions/ |
| | :382-419 (fs::write :418) | wiki/wisdom/models/ |
| | :429-432 | wiki/wisdom/mental-model-signals/ |
| | :438-467 (fs::write :466) | wiki/wisdom/anti-patterns/ |
| | :475+ | wiki/wisdom/positive-patterns/ |
| reflection (worker) | reflection.rs:159 | wiki/wisdom/reflections/ |
| | :100 | wiki/wisdom/reflection-signals/ |
| | :518,553 / :617,658 | wiki/wisdom/{anti,positive}-patterns/ |
| reflection (vault) | zen-vault/src/distill/reflection.rs:32-48 (write_atomic :46) | wiki/wisdom/reflections/ |
| decision_tracker | decision_tracker.rs:94,125-126 | wiki/wisdom/anti-patterns/ |
| dream | zen-memory/src/dream.rs:703-719 | wiki/wisdom/rejected/ |
| zen_loop | zen_loop.rs:1499-1511 (save :1508) | wiki/wisdom/hypotheses/ |
| | :1020-1037 | wiki/wisdom/{rejected,reflections}/ |
| promotion_worker | promotion_worker.rs:406-408 (AtomicWikiWriter) | promoted wiki pages |
| seed (one-time) | zen-memory/src/seed.rs:24 | wiki/wisdom/{models,anti-patterns}/ |

### wiki/ (compiled + adjacent)

| Writer | File:line | Target |
|---|---|---|
| WikiCompiler | zen-vault/src/tindy/wiki_compiler.rs:219 | compiled wiki pages (two-pass) |
| distill pipeline | pipeline.rs:390-392, :1494 | vault/archive/ provenance |
| | :1522, :1538 | REWRITES wiki pages (merge/provenance) |
| E3 cascade | zen-vault/src/distill/cascade.rs | REWRITES/DELETES pages (txn-tracked) |
| zen wiki rollback (E5) | zen-vault/src/wiki/iterations.rs:156 (write_atomic) | REWRITES a wiki page from a stored iteration |
| communities | zen-vault/src/communities.rs:59-72 | wiki/communities/ |
| graph_router | graph_router.rs:483, :616 | vault/{para_target}/host-*.md, vault/raw/ |
| note | note.rs:267,344 | write_note |
| source_ingest | source_ingest.rs:195 | vault/inbox/ promoted notes |
| express | express.rs:205-220 | vault/output/ |
| retention (DELETER only) | retention.rs:198-213 | age-deletes suggestions/output |

## Decision

**The semantic lint agent is READ-ONLY over wiki pages.** Its findings flow
through the existing lint reporting path (`LintResult.semantic_findings` →
`LintReportGenerator` → `reports/lint-YYYY-MM-DD.md`, an existing writer) and
are never written back to wiki pages. Therefore:

1. **No new wiki file surface is created** — the second-writer prohibition is
   satisfied vacuously: the only thing the semantic pass persists through is
   the lint report writer that already exists.
2. **The E2 invariant is preserved** — page frontmatter (`sources:` and the
   rest) stays code-managed; an LLM never annotates pages (the E2 guardrail:
   "the LLM never writes it").
3. **The gate ships closed** — `[agentic.lint] semantic` defaults to `false`
   (env `ZEN_LINT_SEMANTIC`); flipping it on is an explicit operator act.
   This mirrors the T168 shadow-mode decision: no LLM call happens at all
   unless the operator opens the gate.
4. **Fail-open everywhere** — unconfigured provider, unreachable model, or
   unparsable LLM output yield zero findings plus a warn, never an error and
   never a fabricated finding.
5. **Bounded input** — the audit digest caps pages (40) and per-page content
   (2,000 chars) so a large wiki cannot produce an unbounded prompt.

### Inventory completeness note (2026-10-02)

The inventory above is a point-in-time snapshot; the load-bearing output of
this record is the read-only decision, not the enumeration. A re-verification
found one writer missing: `zen wiki rollback`'s `PageIterations::restore`, added
by E5 (`e8ef9e2`) after this inventory was taken. It changes no conclusion —
`restore` is a user-invoked CLI action that captures the clobbered version
before overwriting (so a rollback is itself reversible), and it is not a
concurrent writer against the lint pass. Note that the "13 writers" figure
recorded in `openkb-compile-hygiene.md` never matched this table, which lists 20
entries (19 content writers + `retention`, the delete-only one); that count is
corrected there.
