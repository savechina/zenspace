# Workflow Induction — Reusable DAG Templates from Plan History (2026-11-18)

**Status:** Revised per independent review 2026-11-18 (P1-A/B/C, P2-1..13 resolved). Provenance: spec 006-agentic-orchestration D15 (spec.md:507-512), R8 research entry (spec.md:405, ADOPT-WITH-GATES), tasks.md:21 trigger fired by owner 2026-11-18. **Constitution XI note:** every design element reuses existing zen machinery (trigram_jaccard, skill-precipitation queue pattern, discovery-replay advisory discipline, MAP-elites archive, retention-strength recency). Zero new dependencies, zero new config keys (D22), zero auto-application to live orchestration (D15/R8 hard constraint).

## 1. Goal & Non-Goals

**Goal.** Induce reusable workflow templates from repeated manual `plan.execute` DAG shapes, surfaced advisively through `zen discover report` as a `workflow_induction` section. An induced template is a DRAFT the human confirms and may cite in a later `plan.execute` invocation; the executor never auto-runs it.

**Non-Goals (verbatim-constrained by D15/R8):**
- **No weights training.** R8 explicitly forbids Uno-Orchestra RL / AREX / SearchSwarm class approaches (spec.md:512). Induction is data-derived pattern matching, not learned.
- **No auto-application to live orchestration.** An induced template is advisory-only (discovery_replay.rs:263 precedent — `advisory_only: true` hard-coded). The plan executor never reads an induced template to alter execution.
- **No LLM clustering.** Shape similarity is deterministic trigram_jaccard (merge.rs:75), not embedding-space. LLM-free = reproducible, zero-cost, no provider routing.
- **No topology mutation.** Induced templates are read-only references; they do not rewrite the live plan DAG or the model's drafting behavior.

## 2. Data Sources & Window

**Primary topology source.** `workflow_plans.spec_json` (migration 006, line 9) — for `plan.execute` fresh mints, `mint_fresh_plan` (plan_task.rs:856-892) clones the FULL tool args verbatim: every task's `id`/`agent`/`prompt`/`depends_on`/`on_failure`/`artifact`/`fork_mode`/`model` + `sensitivity` + `session_id` (plan_task.rs:870-879). This IS the DAG topology source: agent assignment + dependency edges + per-task metadata.

**Deviation from D15/R8 raw material.** D15/R8 name `logs/audit.jsonl` + `discovery-tree.jsonl` as the raw material; this design uses `workflow_plans.spec_json` + `workflow_tasks` instead. **Rationale:** `spec_json` is the machine-readable DAG topology (per-task agent + depends_on + prompt); `audit.jsonl` carries counts only (`loop.plan.completed` audit lines have no per-task shape). The discovery-tree is a policy-level scorer input, not a plan-topology source.

**Outcome join.** `workflow_tasks` rows (migration 006:17-26 + migration 014:20-22 additive columns `model`/`duration_ms`/`cost_usd`) — per-task outcome (`status`: `completed`/`failed`/`skipped`), resolved agent, wall time, cost. Used for success-weighting: only **fully-completed plans** (all tasks `completed` or `skipped`; zero `failed`) contribute to the corpus; plans with any failed task are excluded (they signal a broken shape, not a reusable one).

**Corpus window.** 90 days — DB rows within this retention window (daemon.rs:529-531 GC at `now - 90 * 24 * 3600`). **Archived plan pages are NOT part of the corpus** — they are summary-only (no edges, no per-task prompts) and serve as human-readable digests only. The induction window is live DB rows within 90d of the induction run.

**Excluded source: `plan.todos` mints.** `plan_todos.rs:405-413` stores items as `{id, title, status}` only — NO agent, NO `depends_on` at mint time. The agent is assigned at dispatch (plan_task.rs:595-624 `checkpoint` method writes the resolved agent into `workflow_tasks.agent` via `workflow_repo.rs:84` `checkpoint_task`). **Decision: EXCLUDE `plan.todos` mints from topology induction.** Rationale: the spec_json at mint time is incomplete (titles-only); the resolved topology only exists post-dispatch in `workflow_tasks`, but by then the plan_id is the same and the checkpoint rows carry the agent. However, `depends_on` is NEVER written for `plan.todos` plans (the ledger has no dependency concept — items are dispatched in registration order, plan_todos.rs:414-421). A `plan.todos` plan is a flat list, not a DAG; including it would pollute the shape corpus with topology-less entries. **Discriminator:** induction parses `spec_json` only when it contains a `"tasks"` array of objects with `agent` + `depends_on` keys; `{items:[...]}` shapes (the `plan.todos` mint shape) are skipped with a counted reason (`skipped_titles_only`) in the report. **Alternative considered:** include `plan.todos` plans as "flat shape" (zero edges) — rejected because the induction target is *reusable DAG patterns*, and a flat list is not a DAG pattern worth inducing.

**Archived plan pages are NOT a topology source.** `record_plan_archive` (zen-vault/src/distill/reflection.rs:97-177) writes summary-only pages (frontmatter + `id`/`agent`/`status`/`summary` table + verdict) — NO dependency edges, NO per-task prompts. The archive is a human-readable digest; the DB `spec_json` is the machine-readable topology.

## 3. Shape Representation

**Canonical shape signature.** A plan's shape is a deterministic serialization of its DAG topology + agent assignment:

```
shape(plan) = sort_lexicographically([ (task.agent, in_degree(task), out_degree(task)) for task in plan.tasks ])
```

- **In/out degrees** from Kahn's algorithm (plan_task.rs:547-588 `topological_layers` precedent) — the layering is the semantics source for computing in-degree (number of dependencies) and out-degree (number of dependents) per task.
- **Per-task tuple:** `(agent, in_degree, out_degree)` — captures agent assignment + structural role (source/sink/intermediate).
- **Serialization order:** tuples are sorted **lexicographically** (agent name ascending, then in_degree ascending, then out_degree ascending) — NOT by declaration order or topological order. This ensures two structurally identical plans declared in different task order produce byte-identical signatures.
- **Serialized form:** `"agent1:in1,out1;agent2:in2,out2;..."` — semicolon-separated tuples, sorted lexicographically.

**Rationale.** This representation is:
- **Deterministic:** same DAG ⇒ same signature (lexicographic sort is declaration-order independent).
- **LLM-free:** derived from `spec_json` alone (parse tasks, compute degrees, sort).
- **Topology-aware:** captures dependency structure (in/out degree) + agent assignment.
- **Compact:** ≤12 tasks (MAX_PLAN_TASKS, plan_task.rs:39) ⇒ ≤12 tuples, typically <200 chars.

**Alternatives considered:**
1. **Multiset of `(agent, degree)` pairs (unordered).** Rejected: loses topological ordering — two plans with identical agent-degree multisets but different flow (A→B→C vs A→C→B) would collide.
2. **Full task-prompt embedding + cosine similarity.** Rejected: violates D15/R8 "no weights training" + "no LLM clustering"; embedding cost per plan is non-trivial; prompts are high-variance (same shape, different wording).
3. **Agent-sequence only (ignore edges).** Rejected: loses dependency structure — a sequential chain A→B→C and a fan-out A→{B,C} both serialize to `[A, B, C]` but are structurally distinct.
4. **Adjacency matrix.** Rejected: over-represents sparse DAGs (12×12 matrix for 3 edges); harder to compare via trigram_jaccard (matrix serialization is not human-readable).
5. **Topological-order serialization.** Rejected: `topological_layers` emits ascending task INDICES (plan_task.rs:547-588 returns `Vec<Vec<usize>>`), not id-sorted — two structurally identical plans declared in different order get different topological orderings. Lexicographic sort is declaration-order independent.

**Chosen representation wins** on compactness + topology-faithfulness + trigram_jaccard compatibility + declaration-order independence.

## 4. Similarity & Clustering

**Reuse trigram_jaccard** (merge.rs:59-84) on the serialized shape signature. No new similarity function, no new dependency (Constitution XI).

**Threshold & MIN_OBSERVATIONS.** Skill precipitation uses `OBSERVATION_SIMILARITY=0.72` + `MIN_OBSERVATIONS=2` (skill_precipitation.rs:29/:32). **Decision: adopt the same constants.** Rationale:
- **Shape space is small.** ≤12 tasks, ≤3 layers (MAX_PLAN_LAYERS, plan_task.rs:101), agent vocabulary is the 13-agent registry (AGENTS.md "13 Agents" table). The number of distinct shapes is bounded; 0.72 trigram_jaccard is a reasonable "structurally similar but not byte-identical" threshold.
- **MIN_OBSERVATIONS=2** matches the skill precedent (Hybrid C: "≥2 similar tool successes", skill_precipitation.rs:28-29). A shape seen once is an anecdote; seen twice is a pattern.
- **No tuning.** The constants are IN-CODE (D22 — no config keys); if production evidence shows 0.72 is too loose/tight, adjust the constant, not a config knob.

**Clustering algorithm.** Greedy cluster-around-first-qualifying-plan (skill_precipitation.rs:260-294 `detect_cluster` precedent):
1. Filter corpus to fully-completed plans (all tasks `completed` or `skipped`; zero `failed`).
2. For each plan (sorted by `created_at` DESC), compute trigram_jaccard against the seed plan's shape.
3. Collect plans with similarity ≥ 0.72 into a cluster.
4. If cluster size ≥ MIN_OBSERVATIONS (2), emit a draft.
5. Mark clustered plans as "consumed" (do not re-seed).

**Deterministic:** identical corpus ⇒ identical drafts (sort order is deterministic, greedy selection is first-match).

## 5. Induced Artifact

**Storage location.** `logs/workflow-induction-drafts.json` — follows the `logs/skill-confirmations.json` queue-file pattern (skill_precipitation.rs:54 `DRAFT_QUEUE_FILE`). **Decision: same directory (`logs/`), same JSON-array format, same atomic-write discipline** (atomic_file::write_atomic, skill_precipitation.rs:255 precedent).

**Rejected-signatures ledger.** `logs/workflow-induction-rejected.json` — a second JSON file (array of `{shape_signature, rejected_at, source_plan_ids}`) written atomically on reject, read on every induction run to exclude rejected signatures. **Single-writer discipline:** same flock contract as the draft queue (section 5a below). The skill precedent (skill_precipitation.rs:228-241 `reject`) has NO rejected-set ledger — it just removes from pending; this design adds the ledger because shape signatures are reusable (a rejected shape should never re-appear even if the source plans differ).

**Draft content.** Each draft is a JSON object:

```json
{
  "name": "research-then-synthesize",
  "shape_signature": "analyst:1,1;researcher:0,1;synthesizer:1,0",
  "source_plan_ids": ["plan-abc-123", "plan-def-456"],
  "source_plan_dates": ["2026-11-10", "2026-11-15"],
  "tasks": [
    {
      "id": "task-1",
      "agent": "researcher",
      "role": "source",
      "typical_prompt_skeleton": "Research the topic and extract key findings from at least 5 sources...",
      "depends_on": []
    },
    {
      "id": "task-2",
      "agent": "analyst",
      "role": "intermediate",
      "typical_prompt_skeleton": "Analyze the research findings and identify patterns in the data...",
      "depends_on": ["task-1"]
    },
    {
      "id": "task-3",
      "agent": "synthesizer",
      "role": "sink",
      "typical_prompt_skeleton": "Synthesize a deliverable from the analysis and format as a report...",
      "depends_on": ["task-2"]
    }
  ],
  "success_stats": {
    "cluster_size": 2,
    "avg_duration_ms": 45000,
    "avg_cost_usd": 0.12,
    "success_rate": 1.0
  },
  "induced_at": "2026-11-18T10:30:00Z"
}
```

- **`name`:** human-readable slug (derived from the most common plan `name` in the cluster, or `shape-<hash>` if names diverge). **`shape-<hash>` definition:** `sha256` of `shape_signature`, first 8 hex (sha8, repo convention — mirrors `plan_archive_slug` collision suffix, zen-vault/src/distill/reflection.rs:165-172).
- **`shape_signature`:** the canonical shape (section 3).
- **`source_plan_ids` + `source_plan_dates`:** evidence trail — which plans were clustered.
- **`tasks`:** per-task template — agent + role (source/intermediate/sink from in/out degree) + typical prompt skeleton (literal truncated median prompt from the cluster, truncated to 200 chars + `...` marker). **Mechanism:** sort the cluster's prompts for this task slot by length, take the median, truncate to 200 chars at a char boundary. No placeholder extraction (that would be new machinery); the honest default is a literal truncated prompt.
- **`success_stats`:** cluster aggregate — size, avg duration, avg cost, success rate (fully-completed / total in cluster).
- **`induced_at`:** timestamp of induction run.

**Format.** JSON array of drafts, atomic-write on every induction run (overwrite, not append — the file is the current queue, not a log).

### 5a. Single-Writer Flock Contract

**Flock discipline.** The draft queue file (`logs/workflow-induction-drafts.json`) and the rejected-signatures ledger (`logs/workflow-induction-rejected.json`) are both written under an exclusive cross-process flock, mirroring the `placeholders_store` precedent (zen-vault/src/distill/placeholders_store.rs:8-83). **Helper:** `acquire_lock` pattern (placeholders_store.rs:70-83) — `std::fs::File::lock` on `<logs_dir>/workflow-induction.lock`, `LockGuard` unlocks on drop (kernel-released if holder dies). **Single-writer:** the induction worker holds the flock during the entire read-merge-write cycle (load drafts → compute new drafts → atomic-write). Confirm/reject CLI commands also take the flock before mutating either file. **Race pin:** confirm/reject concurrent with an induction run never loses an update — the flock serializes them (induction pauses while confirm/reject holds the lock; confirm/reject pauses while induction holds it).

## 6. Human Gate

**Draft → explicit confirm.** Reuse the `zen skill confirm` / `zen discover confirm` precedent (skill_precipitation.rs:203-225 `confirm` method; discover_command.rs has `confirm`/`reject` subcommands for Hybrid-C promotion gate).

**Decision: additive `zen discover confirm-workflow <draft-name>` subcommand.** Rationale:
- **Consistency:** `zen discover` already hosts the replay scorer (discover_command.rs:299/:342), baselines, calibration — workflow induction is another advisory signal in the same surface.
- **First occurrence never auto-promotes.** A draft is quarantined until explicit confirm (Hybrid C discipline, skill_precipitation.rs:1-12 "only a confirmed draft becomes `~/.zen/skills/<name>/SKILL.md`").
- **Confirm action:** remove draft from `logs/workflow-induction-drafts.json`, write promoted template to `vault/wiki/wisdom/workflow-templates/<name>.md` (frontmatter + shape + tasks + evidence, mirrors `render_skill_md` format, skill_precipitation.rs:337-382). **Template-name collision:** if a same-name template already exists, append a deterministic collision suffix (`<name>-<sha8 of shape_signature>.md`) — mirrors the `record_plan_archive` collision-suffix precedent (zen-vault/src/distill/reflection.rs:159-172 — collision suffix is write-time only, depends on what is already on disk). State confirm writes are exists-check-gated (confirming an already-promoted template is a no-op).
- **Reject action:** `zen discover reject-workflow <draft-name>` removes the draft from the queue file AND appends the `shape_signature` to `logs/workflow-induction-rejected.json` (with `rejected_at` timestamp + `source_plan_ids`); the draft is never re-surfaced as auto-promoted (quarantine is permanent for that shape signature — the induction worker reads the rejected ledger on every run and excludes rejected signatures).

**Idempotency.** Confirm/reject are idempotent: confirming an already-promoted template is a no-op (warn + exit 0); rejecting an already-rejected draft is a no-op (the signature is already in the rejected ledger).

## 7. Scoring & Surfacing

**Discovery-replay discipline.** Mirror `discovery_replay.rs` (T140) — advisory-only, fail-open, named insufficiency states:

| State | `insufficient` | `missing` |
|-------|----------------|-----------|
| No plans in 90d window | `true` | `no workflow_plans in 90d window` |
| < MIN_OBSERVATIONS (2) fully-completed plans | `true` | `insufficient completed plans (< 2)` |
| No cluster meets 0.72 threshold | `true` | `no similar shape cluster (threshold 0.72)` |
| All plans have ≥1 failed task | `true` | `no fully-completed plans` |

**Report surface.** Additive `workflow_induction` section in `zen discover report` (discover_command.rs:291-349), same fail-open shape as `memory_strength` (discover_command.rs:317-325 — missing/corrupt ⇒ `{"recorded": false}`, never an error). **Report is side-effect-free:** the report READS the drafts file and renders it; it never writes to the drafts file or the rejected ledger.

**Report content.**
```json
{
  "workflow_induction": {
    "advisory_only": true,
    "insufficient": false,
    "drafts_count": 2,
    "skipped_corrupt": 0,
    "skipped_titles_only": 1,
    "drafts": [
      {
        "name": "research-then-synthesize",
        "shape_signature": "analyst:1,1;researcher:0,1;synthesizer:1,0",
        "cluster_size": 2,
        "success_rate": 1.0
      }
    ]
  }
}
```

**Report-only.** No code path reads an induced template to alter plan execution. The report is a human-readable signal; the human decides whether to cite a template in a future `plan.execute` invocation (by manually copying the shape or using it as a reference).

**Amendment (2026-11-19, W13.8):** the report's per-run counters (`skipped_corrupt`/`skipped_titles_only`/`insufficient`) come from a minimal last-run sidecar `logs/workflow-induction-last-run.json`, written atomically under the same flock on a non-empty-corpus run (the zero-corpus path stays fully side-effect-free per AC#2, so it writes neither drafts nor sidecar — the report then renders `{"recorded": false}`); a missing/corrupt sidecar is fail-open, never fabricated.

## 8. Execution Surface

**Induction runs as an additive scheduler worker** (ZenWorker trait, zen-agents/src/scheduler/mod.rs:75-87; register via `scheduler.register(worker)` at scheduler/mod.rs:338). **NOT inside `zen discover report`** — the report is side-effect-free (section 7). The worker is registered in `scheduler/mod.rs` alongside the existing 17 workers (session-journaler, dream, memory-curator, etc.). **Schedule:** once per day (e.g., `"0 0 3 * * *"` — 3 AM, mirrors the existing daily log schedule pattern). **Worker id:** `"workflow-induction"`. **Single-writer:** the worker holds the flock (section 5a) during the entire induction run.

## 9. Reuse Map

| Design Element | Existing Machinery | File:Line |
|----------------|-------------------|-----------|
| Shape similarity (trigram_jaccard) | Wiki merge trigram Jaccard | `crates/zen-vault/src/distill/merge.rs:75-84` |
| Clustering (greedy detect_cluster) | Skill precipitation clustering | `crates/zen-agents/src/skill_precipitation.rs:260-294` |
| Threshold (0.72) + MIN_OBSERVATIONS (2) | Skill precipitation constants | `crates/zen-agents/src/skill_precipitation.rs:29/:32` |
| Draft queue file pattern | Skill confirmations queue | `crates/zen-agents/src/skill_precipitation.rs:54` (`DRAFT_QUEUE_FILE`) |
| Confirm/reject human gate | Skill confirm/reject | `crates/zen-agents/src/skill_precipitation.rs:203-241` |
| Advisory-only discipline | Discovery replay scorer | `crates/zen-vault/src/distill/discovery_replay.rs:263` (`advisory_only: true`) |
| Insufficiency states | Discovery replay honest missing | `crates/zen-vault/src/distill/discovery_replay.rs:45-61` |
| Report surface (additive section) | `zen discover report` | `crates/zen-cli/src/cmd/discover_command.rs:291-349` |
| Topological sort (Kahn) | Plan executor layering | `crates/zen-agents/src/plan_task.rs:547-588` |
| Retention (90d window) | Daemon GC + retention policy | `crates/zen-gateway/src/daemon.rs:529-531` + `crates/zen-vault/src/distill/retention.rs:352-360` |
| Atomic write | Shared atomic_file | `crates/zen-core/src/atomic_file.rs` (used at skill_precipitation.rs:255) |
| Recency weighting (if needed) | Retention strength Ebbinghaus | `crates/zen-memory/src/strength.rs:67-80` |
| Flock contract | Placeholders store flock | `crates/zen-vault/src/distill/placeholders_store.rs:70-83` (`acquire_lock` + `LockGuard`) |
| Scheduler registration | ZenWorker trait + register | `crates/zen-agents/src/scheduler/mod.rs:75-87` (trait) + `:338` (register) |
| Template collision suffix | Plan archive collision | `crates/zen-vault/src/distill/reflection.rs:159-172` (write-time sha8 suffix) |

## 10. Acceptance Criteria (Falsifiable, Testable)

1. **Topology + similarity ⇒ draft.** Two plans with identical agent-topology (`shape_signature` byte-identical) and ≥0.72 trigram_jaccard similarity, both fully completed (zero failed tasks), produce exactly one draft listing both `plan_ids` in `source_plan_ids`. **Second required test:** two DIFFERENT-but-similar signatures (e.g., same topology, one task's agent differs) at/above and below the 0.72 boundary to pin the threshold behavior (one cluster forms at 0.72, two separate clusters at 0.71).
2. **Zero plans ⇒ insufficient.** Zero plans in the 90d window ⇒ report renders `{"insufficient": true, "missing": "no workflow_plans in 90d window"}`, no draft written to `logs/workflow-induction-drafts.json` (the worker's zero-corpus run writes nothing; the report is side-effect-free).
3. **Confirm is idempotent.** Confirming an already-promoted template is a no-op (warn + exit 0); the template file is not rewritten.
4. **Reject removes draft + permanent quarantine.** Rejecting a draft removes it from `logs/workflow-induction-drafts.json` AND appends the `shape_signature` to `logs/workflow-induction-rejected.json`; the draft never reappears in subsequent induction runs (the worker reads the rejected ledger on every run and excludes rejected signatures — quarantine is permanent for that `shape_signature`).
5. **Report-only invariant (grep gate).** No `crates/**` source file may reference the drafts/template path (`logs/workflow-induction-drafts.json` or `vault/wiki/wisdom/workflow-templates/`) except the induction worker + discover report renderer. **String scan** (mirrors `config_documented_keys` sentinel pattern): grep for the path strings; zero hits outside the induction worker module + discover_command.rs report section.
6. **Deterministic induction.** Identical corpus (same DB rows within 90d window) ⇒ identical drafts (same `shape_signature`, same `source_plan_ids`, same `name`). **Required pin:** `shuffled_declaration_order_produces_identical_signature` — same DAG, tasks declared in different order ⇒ byte-identical `shape_signature` (lexicographic sort is declaration-order independent).
7. **Fail-open report.** Missing or corrupt `logs/workflow-induction-drafts.json` ⇒ report renders `{"recorded": false}`, never an error (mirrors `memory_strength` discipline, discover_command.rs:317-325).
8. **Corrupt-row handling.** Induction reader is fail-open — malformed `spec_json` (missing keys, unparseable JSON, DAG failing basic sanity) ⇒ skip the row, count it as `skipped_corrupt` in the report, never abort the run.
9. **Flock race pin.** Confirm/reject concurrent with an induction run never loses an update — the flock serializes them (test: spawn induction + confirm in parallel; both complete; final state is consistent).

## 11. Open Questions for Owner

1. **Template storage location.** Proposed: `vault/wiki/wisdom/workflow-templates/<name>.md` (mirrors `wisdom/plans/` and `wisdom/rewinds/`). Alternative: `~/.zen/skills/workflows/<name>/SKILL.md` (unify with skill surface). **Recommendation:** `vault/wiki/wisdom/workflow-templates/` — workflows are plan-shaped, not skill-shaped (skills are tool-success patterns; workflows are DAG templates). Owner decision needed.
2. **`plan.todos` participation.** Proposed: EXCLUDE (section 2 rationale — titles-only at mint, no `depends_on`). Alternative: include as "flat shape" (zero edges) — rejected because flat lists are not DAG patterns. Owner confirmation needed.
3. **Confirm-command surface.** Proposed: `zen discover confirm-workflow` / `zen discover reject-workflow` (additive to existing `zen discover` surface). Alternative: `zen skill confirm-workflow` (unify with skill confirm). **Recommendation:** `zen discover` — workflows are an RSI/discovery signal, not a skill. Owner decision needed.
4. **Template retention.** Draft queue (`logs/workflow-induction-drafts.json`) is overwritten per run (self-limiting); rejected ledger (`logs/workflow-induction-rejected.json`) grows monotonically (no retention row — rejected signatures are permanent by design). Promoted templates under `vault/wiki/wisdom/workflow-templates/` are permanent wisdom by design (no retention row). **Open question:** does the owner prefer a retention row for promoted templates (e.g., age-delete@365d) or are they permanent wisdom?

## 12. Deviations

1. **Shape representation (section 3).** The skill precedent clusters on *text summaries* (skill_precipitation.rs:282 `trigram_jaccard(&seed_text, &text)`); this design clusters on *structural signatures* (agent + degree tuples). **Reason:** plan shapes are topological, not textual — two plans with identical agent flow but different prompts are the same shape; two plans with identical prompts but different agent flow are different shapes.
2. **Prompt skeleton in draft (section 5).** The draft includes a `typical_prompt_skeleton` (literal truncated median prompt from cluster). The skill precedent does NOT include prompt text in the draft (skill_precipitation.rs:60-71 `SkillDraft` has `prompt: String` but it is the *distilled* prompt, not a source excerpt). **Reason:** workflow templates are meant to be cited as references; a prompt skeleton gives the human a starting point. This is additive — the skeleton is not auto-injected into any execution. **Mechanism:** literal truncated median (no placeholder extraction — that would be new machinery).
3. **Success-weighting (section 2).** Only fully-completed plans (zero failed tasks) contribute to the corpus. The skill precedent includes both successes and corrections (skill_precipitation.rs:269 `for is_success in [true, false]`). **Reason:** a failed plan signals a broken shape, not a reusable pattern; a corrected skill signals a pitfall worth recording. The induction target is *successful DAG patterns*, not failure modes (failure modes are captured by the rewind path, zen-vault/src/distill/reflection.rs:97-177).
4. **Data sources (section 2).** D15/R8 name `logs/audit.jsonl` + `discovery-tree.jsonl` as raw material; this design uses `workflow_plans.spec_json` + `workflow_tasks` instead. **Reason:** `spec_json` is the machine-readable DAG topology; `audit.jsonl` carries counts only, no per-task shape.
5. **Corpus filter (section 2).** Only fully-completed plans (all tasks `completed` or `skipped`; zero `failed`). **Reason:** a plan with any failed task signals a broken shape, not a reusable pattern. Matches AC #1.

## 13. Alternatives Considered

1. **Draft storage format.** Overwrite-array (chosen) vs JSONL append. **Decision:** overwrite-array — the file is the current queue, not a log. Overwrite semantics match the skill-precipitation precedent (skill_precipitation.rs:255 `write_pending` overwrites the entire array). JSONL append would require compaction logic (remove rejected drafts) and is harder to reason about (queue vs log).
2. **Clustering algorithm.** Greedy (chosen) vs full-linkage hierarchical clustering. **Decision:** greedy — mirrors the skill-precipitation `detect_cluster` precedent (skill_precipitation.rs:260-294). Greedy is deterministic, O(n²) in the corpus size (acceptable for ≤1000 plans/90d), and produces the same clusters as full-linkage when the similarity threshold is high (0.72). Full-linkage is more expensive and harder to explain.

## 14. Trigger & Provenance

**Trigger fired.** tasks.md:21 recorded "T140 replay accumulation *plus* a measured repetition of manual plan shapes" as the deferral trigger. Owner declared this triggered 2026-11-18. **Evidence of trigger:** production `workflow_plans` table has accumulated ≥2 plans with similar agent-topology (the owner's manual observation — the induction machinery will make this measurable). **Trigger conjuncts:** (1) T140 replay accumulation — the replay-accumulation conjunct remains an observation-time check (induction's own corpus insufficiency states keep honesty independent of replay accumulation); (2) measured manual-plan-shape repetition — the owner fired this conjunct 2026-11-18 citing measured repetition.

**Design-first (Constitution XI).** This document is the design of record. No code lands until this design is owner-approved. Reuse-before-novel: every design element cites existing machinery (section 9 reuse map). Zero new dependencies, zero new config keys, zero auto-application.

**Next step (not in this task).** Owner approval → implementation tasks (not designed here — this is design-only per the task brief). Implementation would land in `crates/zen-agents/src/workflow_induction.rs` (new module, ZenWorker trait impl) + `crates/zen-agents/src/scheduler/mod.rs` (register the worker) + `crates/zen-cli/src/cmd/discover_command.rs` (additive `confirm-workflow`/`reject-workflow` subcommands + additive `workflow_induction` section in report, mirrors `memory_strength` pattern).

## 15. Corpus-observability amendment (2026-11-20, eng-review D2/T5, owner ultrawork authorization)

**Problem (verified).** The canonical D21 flow cannot produce a DAG-shaped
corpus row: `plan.todos` mints `spec_json = {items:[...], sensitivity,
session_id}`, `plan.execute` ATTACHES to that row (plan_task.rs
Some(lid) arm — "a live session ledger record wins over a fresh mint") and
`spec_json` is never rewritten. Production probe (2026-11-20): 1 plan
total, DAG-shaped but `failed` — a test artifact. The induction corpus is
therefore structurally starved by the very flow the orchestrator
mandates.

**Decision (T5).** At ledger-attach, `plan.execute` merges the CURRENT
batch's DAG into the plan row's `spec_json` via one additive
`WorkflowRepo::update_plan_spec_json(plan_id, spec_json)` UPDATE:

- Read-modify-write preserving `items` + `sensitivity` + `session_id` +
  `name`; the `tasks` array is REPLACED with the current batch's
  `{id, agent, prompt, depends_on}` — the row observes the
  last-dispatched shape. Rationale: one row = one plan (mint-once
  contract intact — the rewrite is an UPDATE, not a mint); per-batch
  history already lives in `workflow_tasks`.
- The §2 discriminator is unchanged: the parser prefers `tasks` when
  present (`items` coexist harmlessly for the ledger UI); pre-attach
  rows remain `TitlesOnly`; fresh DAG mints unchanged.
- Constraints: the attach path already holds the DB claim fence — the
  UPDATE adds no new lock; W12.17 marker keys preserved verbatim; the
  write is idempotent per batch (same batch re-attach writes the same
  DAG).

**AC#10 (new).** Attach writes an observable DAG: e2e —
`plan.todos` mint → `plan.execute` ledger-attach →
`shape_signature_from_spec_json(row.spec_json)` classifies `Dag`.

**Hardening amendments (T4, same authorization).** §2 parser: a MISSING
`depends_on` key is tolerated as `[]` (mirror of the tool contract
`plan_task.rs:457 depends_on?`); present-but-non-array stays corrupt.
§2 corpus: admission requires plan `status == 'completed'` AND
settled-row count ≥ expected (fresh mints: spec tasks; attached:
`items`), so partial and cancelled plans never enter the corpus. §5a:
clustering computes OUTSIDE the flock (the lock covers only
drafts-queue + sidecar writes); `induction_corpus` gains an in-code
LIMIT. §7: a zero-corpus run writes the LAST-RUN SIDECAR (recorded:
true, `NoPlansInWindow`) but no drafts file — "never ran"
(recorded:false) and "ran, nothing to do" become distinguishable; a
titles-only-dominant empty corpus reports its own honest reason. §8:
worker registration is gated by `[agentic.workflow_induction].enabled`
(default true; delegate `[agentic.delegate].enabled` precedent — a D22
exception ratified by the eng-review #9 finding and the owner's
2026-11-20 authorization).
