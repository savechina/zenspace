# TODOS

## Review

### Nightly memvid-indexer duplicates the entire corpus every run (found by 2026-10-11 /sdd-review GAN adjudication, P1 URGENT — compounds nightly at 01:00) — RESOLVED 2026-10-11 (Phase 32 T219)

**What:** The memvid-indexer worker calls `index_all` (memvid_index.rs:141-185 — NO checksum skip; checksums exist only in `index_incremental` :193-241 which has ZERO production callers), and `persist_structured_turn_meta → put_text → put_bytes_with_options` passes default `PutOptions` with `dedup=false` (memvid-core's BLAKE3 content-dedup exists at mutation.rs:3316-3330 but zen never enables it). ⇒ Every nightly run re-appends the FULL M2/M3/M4 corpus as new frames PLUS a new MemoryCard per chunk. Latent no-op until T195 (`4529f1c`, 2026-10-10) fixed the indexer paths — now live against real data, compounding every night: store bloat + duplicate retrieval hits crowding top-K + duplicate card populations polluting `entity_memories` (the enriched path's source).

**Why P1:** Fires again tonight 01:00 (daemon Full profile). Not gated by any Phase-31 config — indexing itself is unconditional. Degradation is user-visible (memory recall quality) regardless of memory_strength gates.

**Fix (oracle-adjudicated):** worker switches to `index_incremental` — machinery complete incl. checksum persistence (:233) and index_all fallback on missing sidecar; this gives it its first production caller (inverts the FND-9 dormant-primitive finding). **Do NOT fix via `dedup(true)`** — content-hash dedup would block the T210 provenance flip from ever backfilling existing frames on re-index. Race safety verified: worker and daemon share the same-process singleton `Arc<Mutex<Memvid>>` (memvid.rs:129-141) — interleaving possible, corruption not. Tracked as tasks.md Phase 32 T219.

**Effort:** S
**Priority:** P1
**Depends on:** none — one-line call switch + idempotency tests (second run adds zero frames)

**Resolution (2026-10-11, T219):** worker switched to `index_incremental` (`memvid_indexer_worker.rs` — its first production caller; call-site comment records the R1 rationale + the `dedup(true)` prohibition). Proof: `two_worker_runs_over_unchanged_corpus_add_zero_frames` (zen-agents integration — discriminating: pre-fix `index_all` doubles frames on run 2) + 3 zen-memory inline idempotency tests (second-run zero frames / changed-file re-index / missing-sidecar full fallback). Gate bin/test 3109/0/25.

### Contentless FTS5 serves structurally-empty tier-2 content (found by Phase 31 T214, 2026-10-11) — RESOLVED 2026-10-11 (Phase 32 T220+T222)

**What:** `notes_fts` is a contentless FTS5 table (`content=''`, migration 001:27-31) ⇒ `NotesRepo::search`'s `SUBSTR(nf.content,…)` snippet returns `''` for EVERY tier-2 hit. Gateway `knowledge/search` RPC and TUI Fast-mode context injection serve empty `content` to consumers today. T214's `inject_knowledge_context` works around it by reading the canonical markdown file (256KiB cap + sanitize, wake-up-brief precedent) — the workaround should be generalized or the defect fixed at the source.

**Why:** Tier-2 is the default search tier (`KnowledgeSearchMode::Fast` pins `tiers=["fts"]`); every surface that renders snippet content shows nothing. Silent since 001 — consumers treat empty content as "no match detail" rather than a defect.

**Context:** Found empirically by the wave-3 agent (`019861f`, orchestrator.rs `inject_knowledge_context`). Options: (a) external-content FTS5 (`content='notes_meta'` + rebuild triggers — migration is additive-adjacent but touches FTS semantics, Principle XIII #8 rebuild discipline applies); (b) join the canonical file read into `NotesRepo::search` results; (c) leave the per-consumer file-read workaround and document it. Start by reading migration 001's FTS block, `notes_repo.rs` search, and the gateway `knowledge/search` handler.

**Effort:** M
**Priority:** P2
**Depends on:** none — pre-existing defect, independent of Phase 31 (which routed around it)

**Review verdict (2026-10-11 /sdd-review, oracle-adjudicated):** severity escalated by a NEW conjunction — TUI Fast mode passes 5 empty-content notes into `session.knowledge`; Phase-31's `pre_supplied=!is_empty` predicate trusts them ⇒ the default-ON knowledge_inject gate (D-F) is structurally skipped on the primary interactive surface AND M1 renders empty bodies. Adjudicated fix split: **(1) P1 now** — predicate requires ≥1 non-empty trimmed-content entry (orchestrator.rs:1202/1971, T220); **(2) P2 root** — option (b) canonical-file read in `NotesRepo::search` (capped, fail-open; one fix serves gateway+TUI+tools+fusion, T222). Option (a) external-content FTS migration **REJECTED** (heaviest; FTS index remains the ranking structure either way — read-layer fix buys the same).

**Resolution (2026-10-11):** both halves of the adjudicated split landed — **T220**: single-source predicate `has_substantive_knowledge` (orchestrator.rs:469) at both capture sites; the 5-empty TUI vec no longer suppresses the default-ON gate (injection-runs/skip-honored/off-mode byte-identity tests). **T222**: `NotesRepo::search` populates snippets from the canonical file (`read_canonical_snippet`, 256 KiB precedent cap, fail-open empty; MATCH/bm25/row-shape unchanged; 3 tests incl. file-sourced marker + ranking invariance). Option (a) remains rejected.

### D1-class Confidential async routing hazard — audit `enforce_sensitivity → reqwest::blocking` chain (re-observed by Phase 31 T215, 2026-10-11) — RESOLVED 2026-10-13 (leaf fix: health_check dedicated-thread)

**What:** Sensitivity routing in async contexts reaches `enforce_sensitivity → is_local_llm_available → OllamaProvider::health_check → reqwest::blocking` — the nested-runtime drop-panic class (D1, previously fixed at specific call sites via `spawn_blocking` guards: intent.rs, review_pipeline, semantic_lint). `DelegateTaskTool::run_single` has routed under mirrored sensitivity since 006 WITHOUT a spawn_blocking guard — re-observed (not introduced) by the wave-3 agent; async tests avoided Confidential for this reason.

**Why:** A Confidential delegate/plan sub-turn on a live async runtime can hit the blocking-in-async panic path. The 2026-09-30 "false alarm" record proved the sync `complete()` sites are safe (dedicated-thread pattern), but `is_local_llm_available`'s health check is a DIFFERENT blocking call site — its guard coverage was never audited end-to-end.

**Context:** Wave-3 report deviation #10 (`019861f`). Audit task: enumerate every `enforce_sensitivity`/`is_local_llm_available` caller reachable from async contexts, verify each is behind `spawn_blocking` or a dedicated thread, add guards + regression pins where missing. Start by reading `zen-provider/src/router.rs` (enforce_sensitivity), `providers/ollama.rs` (health_check), and grepping callers in zen-agents.

**Effort:** S
**Priority:** P2
**Depends on:** none — audit + targeted guards

**Resolution (2026-10-13):** root-cause LEAF fix — `OllamaProvider::health_check` (ollama.rs, the only calling-thread `reqwest::blocking` site in the `enforce_sensitivity → is_local_llm_available` chain) now runs its probe on a dedicated thread (the `complete()` pattern; join panic ⇒ `false` + warn, fail-safe: Confidential refuses cloud and surfaces the typed local-unavailable error). One leaf change covers every async-reachable chain end-to-end: delegate `run_single` (primary site; `plan.execute` inherits via the shared seam), orchestrator `execute`/redraft, zen_loop `resolve_model_tier`, plus out-of-scope-crate sites zen-vault `llm_distill::call_llm` and zen-cli `provider_command` (debug-build `zen provider route/test/list` panic also cured). No zen-agents/zen-gateway production change; no routing-decision change. Audit inventory: 7 UNGUARDED async-reachable sites (all covered by the leaf), 11 already-guarded verified (intent/deanchored/semantic_lint/5 worker complete_metered/sync complete+embed), gateway `.route(` hits are axum not LlmRouter. Pre-fix empirical panic verified ("Cannot drop a runtime in a context where blocking is not allowed"). Pins: `sync_complete_no_panic.rs` health_check ×2 runtime flavors + `confidential_delegate_routing_on_async_runtime_does_not_panic` (real run_single, current_thread, port-1 loopback no_proxy per T217) + `local_rung_probes_inside_runtime_abstain_without_panic`. Gate: bin/lint 0; bin/test 3113/0/25 (baseline 3109, +4). Recorded in 006 spec.md Convergence Record C4. **Spin-off finding filed below (trait-default shadowing, P3 owner decision).**

### L1 local rungs never probe — `is_local_llm_available` trait-default shadowing (found by the 2026-10-13 D1 audit, P3 owner decision) — RESOLVED 2026-10-13 (owner approved activation)

**What:** The free function `zen_provider::is_local_llm_available(&dyn LlmRouter)` hits the TRAIT-DEFAULT `false` (router.rs:404) — `DefaultRouter`'s probing inherent method (router.rs:1243) is not in `impl LlmRouter`, reachable only from inside the router (`enforce_sensitivity`/`route_with_preferences`). Consequence: `orchestrator.rs:1090` (T170 review local-hop), `decision/mod.rs:541` (LocalClassifierRung), `decision/mod.rs:831` (LocalBinaryClassifier) NEVER probe — the L1 local rungs and the de-anchored review's local phase structurally always abstain/escalate-frontier. Proven empirically: the in-runtime rung test passed PRE-fix while direct `health_check` panicked.

**Why:** Dormant local-first capability — every L1 rung decision falls through to L2/frontier, defeating the T168-T171 decision ladder's local-first intent on these paths. Wiring the override into the trait impl is ~1 method, but it ACTIVATES the rungs (real probes, real local classifications) = behavior change ⇒ owner decision, not an audit side-fix. The runtime-safety pin (`local_rung_probes_inside_runtime_abstain_without_panic`) already guards the activation against the panic regression.

**Context:** crates/zen-provider/src/router.rs (:404 trait default, :1243 inherent), crates/zen-agents/src/decision/mod.rs, crates/zen-agents/src/orchestrator.rs:1090. Related latent (same audit): four dead cloud-provider `health_check`s (anthropic.rs:184, mistral.rs:141, cohere.rs:141, gemini.rs:141) use `reqwest::blocking`, zero callers — need the dedicated-thread guard (ollama.rs precedent) if ever exposed via `ProviderInstance`.

**Effort:** S
**Priority:** P3 (owner decision)
**Depends on:** none

**Resolution (2026-10-13, owner approved activation):** `impl LlmRouter for DefaultRouter` (router.rs:1541-1548) now overrides `is_local_llm_available` — pure delegation to the inherent probe (:1243, which reaches ONLY `provider_instance("ollama")` + the dedicated-threaded `health_check`; the 4 dead cloud-provider health_checks stay unreachable/dormant). The three dyn consumers (`orchestrator.rs:1090` T170 local-hop, `decision/mod.rs:541` LocalClassifierRung, `:831` LocalBinaryClassifier) now probe for real. Byte-identity holds: all three sit behind calibration τ gates that default closed (`None`) — the T169/T170/T171 no-τ pins passed with UNTOUCHED assertions. Pins: `zen-provider/tests/local_llm_probe_dyn.rs` (inherent + dyn path both probe-false for unreachable 127.0.0.1:1 loopback no_proxy) + `local_rung_probes_inside_runtime_abstain_without_panic` (doc-comment updated, assertion untouched). Gate: bin/lint 0; bin/test 3115/0/25 (baseline 3113, +2). Activation effect timeline: today zero behavioral change; once calibration thresholds land (`logs/decision-audit/thresholds.json`), the local-first L1 layer serves without further wiring.

### Memory-strength enriched-path symmetry — RECLASSIFIED P1 premise-break-under-enable (Phase 31 T209/T211; adjudicated 2026-10-11 /sdd-review) — RESOLVED 2026-10-11 (Phase 32 T221)

**What:** Both the strength READ side (`card_retention_strength` in `retrieve_memories_structured`, T209) and the reward WRITE side (`increment_memvid_injection_rewards`, T211) live ONLY in the `structured` retrieve path — but the chain prefers `retrieve_memories_enriched`. Decisive mechanical detail (oracle-verified): after a session's first persisted turn, `entity_memories(session_id)` is non-empty ⇒ enriched wins EVERY subsequent turn ⇒ the increment point runs ≈once per session lifetime ⇒ access events n≈0 ⇒ the recorded success criterion "排序反映年龄×使用" has its ×使用 half structurally dead when enabled.

**Why:** No longer an open owner decision — the review reclassified this from "recorded limitation" to **premise-break under enable**: it is a REQUIRED FIX BEFORE flipping `[agentic.memory_strength].enabled`. Owner choice remains only on fix scope (increment-only vs full read/write symmetry on enriched).

**Context:** Fix seam (oracle-shaped): the filtered cards are already in hand at memvid.rs:514-517 inside `retrieve_with_entity_context` — additively return them (or a sibling method) and run the same gated `increment_memvid_injection_rewards` (already takes caller-fetched cards). Gate stays `memory_strength.enabled` (provably no-op when closed). Tracked as tasks.md Phase 32 T221.

**Effort:** S-M
**Priority:** P1 (fix-before-enable)
**Depends on:** adjudicated — no longer blocked on owner decision to WHETHER; owner may still scope

**Resolution (2026-10-11, T221):** increment-only scope (the minimal oracle-shaped seam): sibling `retrieve_with_entity_context_and_cards` (memvid.rs) returns the filtered cards already in hand — positionally aligned; legacy method delegates unchanged; the enriched path runs the same `increment_memvid_injection_rewards` under the same fail-closed `memory_strength.enabled` gate (zen_agent.rs:1080-1084), flock + dedup-by-source_path identical to the structured path. Proof: gate-open ⇒ sidecar written with dedup (2 cards same source ⇒ access_count 1); gate-closed ⇒ zero writes. Read/write symmetry on the preferred path restored — the `enabled` flip precondition is met.

### QQBot outbox drainer (morning-brief push completion) — RESOLVED 2026-09-04 (005 T101)

**What:** Gateway-side task draining `logs/outbox/morning-brief-*.json` via qqbot active send (`msg_id=None`), deleting each file on successful send, leaving failures for retry with warn log.

**Why:** *(historical)* Completes FR-038's push face; without it the 9am brief never reaches chat — the outbox was write-only.
**Resolved:** `crates/zen-gateway/src/channel/qqbot/outbox_drainer.rs` drains staged briefs on a tick (Public-only fail-closed, group-first/C2C-fallback, never deletes undelivered).

**Pros:** True proactive push; reuses `QqBotApi::send_group/c2c_message`; fail-soft seam already tested producer-side.

**Cons:** New gateway runtime surface (poll interval, recipient resolution from `chat_hint`, retry/backoff policy); touches the daemon tick.

**Context:** Producer done 2026-09-03 (005-agentic-loop T079): `MorningBriefWorker` stages `{date, lines, chat_hint}` JSON; seam documented in `crates/zen-agents/src/scheduler/workers/morning_brief.rs`. Active-send API at `crates/zen-gateway/src/channel/qqbot/api.rs:46,61`. Constraint: zen-agents must not depend on zen-gateway — drainer lives gateway-side. Start by reading the qqbot adapter's active-send fallback (`adapter.rs:~528`).

**Effort:** M
**Priority:** P1
**Depends on:** 005-agentic-loop T079 (done, `morning-brief` worker + outbox contract) — **IMPLEMENTED per 005-agentic-loop T101 [X] 2026-09-04: `crates/zen-gateway/src/channel/qqbot/outbox_drainer.rs` (`drain_once` + tick spawn, Public-only fail-closed, never deletes undelivered; recipients via `QqBindingRepo::list_chat_ids`, group-first/C2C-fallback)**

### Harden dispatch hold-transport contract (shutdown_write EOF test) — RESOLVED 2026-08-25 (004 T066/T071)

**What:** Document `shutdown_write` as the only EOF path for detached SPAWNED tasks and add test that `abort + shutdown_write → EOF` while `abort` alone does not hang.

**Why:** Detached `session/turn` handlers hold `Arc<DispatchServer>` + transport; aborting `run()` never closes client — silent hang on cancel, leaks connection.

**Context:** Prior learning `dispatch-spawned-methods-hold-transport` (2026-08-23, 9/10). Fix: keep `tokio::spawn` for responsiveness but make close contract explicit. Files: `crates/zen-gateway/src/server/dispatch.rs:46,188`, `crates/zen-gateway/src/transport/uds.rs:72`, `crates/zen-gateway/src/client/surface.rs:728`. Test location: `crates/zen-gateway/tests/spawn_race.rs` or new `dispatch_shutdown.rs`. Start by reading `DispatchServer::run` and `SPAWNED_METHODS`.

**Effort:** S
**Priority:** P1
**Depends on:** Lane PR1 (gateway-core) — **Aligned to tasks.md T066/T071 (Phase 13.1) — IMPLEMENTED per 004-agentic-gateway tasks.md [X] 2026-08-25**

### Turn-affinity approval fix (thread turn_id through callback) — RESOLVED 2026-09-04 (005 T103)

**What:** Thread `turn_id` / `ConnectionHandle` affinity through sandbox `ApprovalCallback` so `ApprovalBroker::decide` binds to originating turn, not first unclaimed route.

**Why:** *(historical)* `decide()` scanned for the first `!claimed` route — concurrent turns misrouted approvals, letting surface B approve surface A's `shell.exec` (SC-007 privilege escalation, P0).
**Resolved:** `ApprovalCallback` now carries `Option<String>` turn id; `AskApprovalHook` reads the task-local `APPROVAL_TURN`; the broker exact-matches on `decide_for`/`route_for` (unknown or claimed → Deny). Pinned by `turn_bound_routing_survives_inverted_firing_order`.

**Context:** Prior learning `approval-broker-claim-misroutes` (2026-08-23, 9/10, cross-model). Files: `crates/zen-gateway/src/server/approval.rs:158-169`, `crates/zen-gateway/src/daemon.rs:387`. Requires threading `turn_id` via `zen_core::sandbox::ApprovalCallback` (inference-typed). Test: concurrent `route()` E2E pinning approval to correct surface (see `approval.rs:route`).

**Effort:** M
**Priority:** P0
**Depends on:** Lane PR1 (gateway-core) — **Aligned to tasks.md T067 (Phase 13.1) — IMPLEMENTED per 004-agentic-gateway tasks.md [X] 2026-08-25**

## Completed

**Synced to specs current 2026-08-27:** `docs/specs/004-agentic-gateway` — spec.md FR-001..021 + SC-001..007, plan.md 4 crates + contracts/00-05, tasks.md T001..T073 (Phases 1-13.1) all `[X]` — 0 unchecked remaining. Convergence check 2026-08-27: 0 actionable findings, tasks.md byte-for-byte unchanged. Review TODOs above now trace to completed tasks.md entries (T066/T067/T071).


### [Analyze C2] FR-001 sole-owner scope narrowed to .mv2 (spec clarification, no code rewire)

**What:** Clarify FR-001 to cover `.mv2` memvid store only; internal `scheduler::workers::{dream,wiki_compiler,notion_extractor}` are co-located in `GatewayService` sole-owner process and may share `SqliteClient` handle per `daemon.rs:run_uds_foreground`.

**Why:** Analyze gap C2 flagged workers `SqliteClient::open` direct as bypassing gateway sole-owner. Workers run inside daemon, not external clients — narrowing prevents over-migration.

**Context:** `docs/specs/004-agentic-gateway/spec.md:FR-001` patched 2026-08-27; workers at `crates/zen-agents/src/scheduler/workers/dream.rs:52`, `wiki_compiler.rs:229`, `notion_extractor_worker.rs:108` remain direct but scoped as internal. Principle XII via `SqliteClient` repos still holds for `state.db` (knowledge). Alternative wiring via `GatewayService` handle injection deferred — spec now matches code.

**Effort:** S
**Priority:** P1
**Depends on:** None

**Completed:** 2026-08-27 (spec patch, no code change — analyze C2 first)

### [Analyze C4] FR-015 safety pipeline via AgentOrchestrator reuse documented

**What:** Document that gateway safety pipeline (confidentiality→budget→seatbelt→audit→approval) is satisfied via `AgentOrchestrator`/`ZenWiring` reuse inside `server/hosting.rs:TurnExecutor` adapter, not a duplicated chain. Hosted turns emit identical audit records.

**Why:** Analyze gap C4 flagged `server/hosting.rs` handlers lacking explicit `SeatbeltHook` chain. Pipeline lives in `zen-agents/wiring.rs:373,638` and is reused by hosted `execute_stream`.

**Context:** `docs/specs/004-agentic-gateway/spec.md:FR-015` patched 2026-08-27 with `hosting.rs:90-110` reference. No code duplication per Constitution XI (Reuse). Parity provable via `harness_hosting.rs` audit assertions.

**Effort:** S
**Priority:** P1
**Depends on:** None

**Completed:** 2026-08-27 (spec patch, no code change — analyze C4 first)

## Review (2026-09-09 /review — orchestration pattern)

### Orchestration test-lake — RESOLVED 2026-09-09 (all 9 landed same day)

Landed via: intent pure `resolve_intent`/`passes_confidence_gate` seams + alias tests; plan/delegate parse + resume negatives; chunking slot-order both sites; workflow_repo negatives + concurrent checkpoints (migration 007 `owner` claim fence included); orchestrator `review_with_feedback_round` seam (veto → 1 redraft → re-review final). Gates 2321/0/19. List below kept as historical record.

### Original accepted debt (post /review fix round)

**What:** ~9 cheap test gaps from the 2026-09-09 pre-landing review (fix round landed #1-#5 + core tests; these remain):
1. intent.rs low-confidence (`Ok(None)`→Fallback) and timeout rungs untestable — MockProvider reply is fixed; needs a DefaultRouter mock-response seam or a pure gate fn
2. intent.rs `IntentCategory::parse` aliases ("read"/"search"→Query, "write"/"execute"→Action, "admin"/"config"→System, "chat"/"help"→Conversation) + negative-confidence clamp — pure test
3. plan_task parse_plan negatives (missing tasks key / empty array / non-string id-agent-prompt-dep)
4. plan_task resume negatives (unknown plan_id error, resume without state.db)
5. plan_task resume with failed/skipped checkpoints + dependent-of-replayed-ok interaction
6. delegate parse_requests fan-out negatives (empty tasks[], item missing agent/prompt) + tier rejection via scoped DELEGATE_PARENT through invoke
7. max_concurrent chunking untested at both fan-out sites (delegate_task.rs invoke, plan_task.rs layers) — wide-batch tests
8. workflow_repo negatives (duplicate create_plan PK err, load_plan unknown→None, complete_plan no-op) + concurrent checkpoint_task against the single writer
9. orchestrator Momus-veto feedback-round branch has no deterministic seam (plan_approved not injectable)

**Why:** All are negative-path/concurrency/resume-edge lakes; the two CRITICAL contract bugs they guard (mixed-batch ordering, plan gate bypass) are already fixed + pinned, so these are hardening.
**Context:** crates/zen-agents/src/{intent,plan_task,delegate_task}.rs, crates/zen-repo/src/workflow_repo.rs, crates/zen-agents/src/orchestrator.rs. Review session findings #6-#14.
**Effort:** M (a day of test writing). **Priority:** P2. **Depends:** none.

## Review (2026-09-20 /plan-eng-review — 005 spec docs)

### Generated single-ledger for closure records (structural option, CLOSED 2026-09-30 — moot)

**What:** Replace hand-mirrored closure history across spec.md record sections + tasks.md + AGENTS.md Recent Changes with tasks.md as the single source and the other two generated/pointing at it.

**Why:** The closure-status convention (spec.md, 2026-09-20) adds sync discipline but not dedup — each closure must be hand-written correctly in 3+ places or the docs rot (Phase 27 found an entire concurrency workstream unrecorded; T181 found the spec documenting deleted primitives).

**Pros:** Eliminates the write-amplification root cause; one place to audit.
**Cons:** Speckit tooling (specify/converge/implement) reads/writes spec.md+tasks.md directly and would need adapting; AGENTS.md is hand-curated for agents, not generated.
**Context:** Accepted as D2-A (keep convention, record limitation) in the 2026-09-20 eng review after outside-voice tension #8. Trigger: the next verdict-rot incident OR speckit tooling gaining generation support. Start from spec.md's closure-convention blockquote.
**Effort:** M. **Priority:** P3. **Depends on:** speckit tooling evolution; not before.

**Closed (moot):** 2026-09-30 plan-eng-review — `docs/specs/` no longer exists on this machine (gitignored local-only, never committed, directory deleted), so the single-source object (tasks.md) is gone and the trigger conditions have permanently dissolved. The live duplication is AGENTS.md Recent Changes vs TODOS.md — a hand-discipline matter (write closures in one, point from the other), not a tooling gap. Re-evaluate only if `/speckit-specify` regenerates an active spec tree.

### LLM cost reporting is zero everywhere — `llm_cost_cap_usd` cap cannot trip

**What:** Convert worker LLM usage into cost (usage × `ModelMetadata.input_cost_per_million`) and report `llm_cost_usd` from workers; today every worker hardcodes 0.0 (dream.rs:153, session_journaler.rs et al.), so the scheduler's cap check (scheduler/mod.rs:338-346) is live-but-inert.

**Why:** The config key is documented as functional in spec.md's Configuration Surface; a cap that cannot trip is documented-as-live phantom enforcement (outside-voice #2, 2026-09-20).

**Pros:** The `[cron] llm_cost_cap_usd = 10.0` knob becomes real; runaway local/cloud spend gets a ceiling.
**Cons:** Needs usage plumbing per worker call; local Ollama is free (cost 0 is *correct* for local) — only metered providers need it.
**Context:** Spec annotation landed same day (config table marks it inert-until). Start at scheduler/mod.rs cap check + model_meta.rs pricing fields.
**Effort:** S-M. **Priority:** P2. **Depends on:** none.

**Completed:** 2026-09-20 (/speckit-implement batch 3): pricing config keys + metered completion path + WorkerCostLedger sidecar + cap-trip test. Accuracy follow-up superseded 2026-09-30: distill/merge spend is cap-visible (`093e68a`); the remaining bytes/4 estimate accuracy on `complete_metered` is tracked by the deferred "Provider-usage precise plumbing" TODO above.

## Review (2026-09-27 /plan-eng-review — docs/src user guide)

### User-guide consistency check in CI (docs-rot alarm) — IMPLEMENTED 2026-09-30 (`c6067d9`)

**What:** A test (or script wired into `.github/workflows/docs.yml`) asserting `docs/src/cli-commands.md` command tables match the live clap surface (renderable from the `Commands` enum in `crates/zen-cli/src/cli.rs` or `zen --help` output), and spot-asserting documented config keys exist as serde fields on the `zen-core` config structs. Fail CI on drift.

**Why:** The user guide went stale three times — 29→20→23 command count, removed `note`/`search` commands still taught, phantom `llm_preferences`/`max_sensitivity` keys documented that never parsed in any revision. Every drift was silent: users got wrong instructions with no error anywhere. The 2026-09-27 audit (`8c50f62`) fixed the backlog by hand; nothing prevents the next one.

**Pros:** Turns silent doc rot into a red CI job; gates the highest-traffic factual claims (command surface, config keys) with near-zero maintenance once written.

**Cons:** Deliberate guide restructuring requires updating the test; covers tables/keys only, not prose claims.

**Context:** Narrower sibling of the deferred single-ledger TODO above (D2-A covers spec closure records; this gates the user guide's factual tables). Start from the `Commands` enum variants (names render deterministically) and `ProviderConfig`/`AgentConfig`/`Agentic*Config` serde fields in `crates/zen-core/src/config.rs`. Trigger: any future CLI/config surface change lands.
**Effort:** S-M. **Priority:** P2. **Depends on:** none.

**Completed:** 2026-09-30 (plan-eng-review decision 1A): `cli.rs` `#[cfg(test)] docs_consistency` — every clap subcommand must appear as the code-form prefix `` `zen <name> `` in the guide + the header count claim must equal the live variant count; `crates/zen-core/tests/config_documented_keys_test.rs` — sentinel round-trip for the highest-traffic documented keys (phantom keys parse silently without `deny_unknown_fields`; the sentinel asserts the value LANDED). Runs under rust.yml's existing `bin/test` — docs.yml only triggers on `docs/**` so it can never see the crate changes that cause drift.

### NFC alias backfill (T143 closure) — IMPLEMENTED 2026-09-30 (`b344472`)

Pre-2026-09-19 (FR-022/T124) persisted `notion_aliases` rows may be decomposed (NFD); `resolve_alias` normalizes the query but compares against the stored column, so legacy rows could never match, and re-extraction inserts a fresh NFC entity while the NFD row orphans. `NotionsRepo::normalize_aliases_pass` (idempotent, collision-dedup via INSERT OR IGNORE) now runs at the top of `zen wiki reindex`. Tests: `crates/zen-repo/tests/normalize_aliases_test.rs`.

### Distill LLM spend invisible to the cost cap (3A) — IMPLEMENTED 2026-09-30 (`093e68a`)

`zen_loop` hardcoded `llm_cost_usd: 0.0` while T046a's distill stage + merge assist make real LLM calls. Now: `LoopBudget.consumed_cost_usd` + `charge_cost` (same stage→cycle fold-back as tokens), `call_llm` bills via `usage_to_cost_usd` on the routed provider's metadata (mirrors `complete_metered`), `merge_section` returns its cost (tokens stay unbudgeted as before), `LoopCycleReport.llm_cost_usd` feeds `worker_report`. Partial spend before a cycle failure still bills.

### Provider-usage precise plumbing (deferred, trigger-based) — IMPLEMENTED 2026-09-30 (`3aa4eb0`)

**What:** Thread real provider usage echoes through `complete_metered` (workers) and the distill plain path — today both bill on the bytes/4 estimate (router.rs `estimate_tokens`; distill `estimate_tokens` with reply-budget split).

**Why:** For a personal tool on mostly-free local Ollama (cost 0 either way), estimate-vs-real does not change cap decisions; the cap is a runaway ceiling, not a bill. Real plumbing (M effort) buys exactness nobody is consuming yet.

**Pros:** Exact cloud billing; cap trips at true thresholds. **Cons:** M-effort plumbing across 6 provider `complete()` impls (they return only text — usage is discarded inside them); the Anthropic cached path already echoes real usage (`parse_cache_usage`).

**Context:** Reference implementation exists: `anthropic.rs::complete_cached`/`parse_cache_usage` + T046a `billable_tokens` reconciliation semantics (`llm_distill.rs`). **Trigger:** real metered cloud volume in `worker-costs.json` AND a cap misjudgment observed — revisit then. Distill spend is already cap-visible as of `093e68a` (this TODO is accuracy-only). Start at router.rs `complete_metered`.
**Effort:** M. **Priority:** P3. **Depends on:** trigger above.

**Completed:** 2026-09-30 (`3aa4eb0`, owner pulled the trigger early after 3A made distill metering live): rig-core 0.42 already normalizes per-provider usage into `CompletionResponse.usage` — the providers were discarding it inside `agent.prompt()`. Additive: `SyncUsage`/`UsedCompletion`/`reconcile_usage` (Some-nonzero wins; None and all-zero echoes fall back to bytes/4), 6× `complete_with_usage` (rig `completion_request` builder path, same thread-guard), `LlmRouter::call_with_usage` with a delegating default, `complete_metered` + distill plain path reconcile, and `completion_model.rs` feeds the echo into rig `Usage` so AgentRun `run.usage()` is real (stream path keeps len/4, documented). Mock stays None — worker-cost estimate tests unchanged. Remaining estimate sites, deliberately: streaming surface, `executor.rs` inline estimates, `PromptHookTelemetry::record`.

## Review (2026-09-27 /plan-ceo-review — OpenKB compile-hygiene program, deferred items)

Plan: `~/.gstack/projects/zenspace/ceo-plans/2026-09-27-openkb-compile-hygiene.md` (9 accepted workstreams in 3 phases; the two below were explicitly deferred by owner decision).

### E5 — Compiled-wiki page versioning + rollback (iteration-N)

**What:** Before a recompile overwrites a wiki page, save the prior version to `iteration-N/` with a diff; add a rollback surface. Modeled on OpenKB's Skill Factory (`skill history`/`skill rollback`), which exists precisely because OpenKB's own wiki recompile overwrites manual edits (documented known gap).

**Why:** zen's distill recompile has the same overwrite exposure — today only partially mitigated by the CAS VersionSnapshot (self-write-aware OCC), which protects against *external* drift, not against the pipeline itself clobbering a hand-tuned page.

**Pros:** Makes hand-edits durable; removes the "pipeline vs human" conflict class. **Cons:** Storage growth (needs the retention worker to prune iterations); one more read path in compile.

**Context:** Implement AFTER Phase 2's mutation journal lands (shares the snapshot/journal infrastructure — building it first would duplicate). Start from `zen-vault/src/distill/pipeline.rs` compile-write path + `AtomicWikiWriter`.
**Effort:** M (human ~3d / CC ~2-3h). **Priority:** P3. **Depends on:** Phase 2 mutation journal (④) — **IMPLEMENTED per compile-hygiene Phase 4 [X] 2026-09-28: `crates/zen-vault/src/wiki/iterations.rs` (`PageIterations` capture at compile/merge/wikilink-rewrite overwrites into `vault/iterations/`, txn-tracked; `zen wiki rollback [--list][--to <millis>]` with reversible restore; retention age-delete @ 90d; `loop.page.iterations` audit line; commit `e8ef9e2`)**

### E7 — SKILL.md export for external agents

**What:** Generate a `SKILL.md`-style entry (Anthropic/Codex skill format) that points external agents (Claude Code, Codex CLI, Gemini CLI) at the compiled zen wiki — zero-runtime discovery, no MCP setup.

**Why:** OpenKB ships this so any external agent can read its wiki with no extra machinery. zen already has the FR-039 neutral plane (`zen skill list --json`) but does not expose the wiki itself to foreign agents.

**Pros:** Tiny surface (one rendered file + a `zen wiki export-skill` writer); makes the KB consumable outside zen. **Cons:** Personal-workflow preference — only valuable if the owner actually drives external agents at the vault.

**Context:** Writer renders `wiki/index.md` summary + navigation conventions into the skill format; source of truth stays the vault. Start from `zen skill` command surface.
**Effort:** S (human ~2h / CC ~1h). **Priority:** P3. **Depends on:** none. — **IMPLEMENTED per compile-hygiene Phase 4 [X] 2026-09-28: `crates/zen-vault/src/wiki/skill_export.rs` + `zen wiki export-skill [--output][--json]` (Agent Skills spec-conformant: six portable frontmatter fields, `zen-wiki` identity = directory name, 240-entry index cap; default target `~/.zen/skills/zen-wiki/SKILL.md` discoverable by zen's own SkillLoader; commit `697daee`)**

### ⑥ scope note — PDF parsing (from same review) — RESOLVED 2026-09-29

`ingest_local_file` was plain `read_to_string` — no PDF capability. **Resolved** by the all-format ingestion program (2026-09-29): `pdf_to_markdown` (in-process `pdf-extract` + panic containment, commit `1e8d73c`), office formats via `office_oxide` (`c5d8123`), and epub/odt/rtf via the pandoc sidecar (`172f2bb`); promote-time conversion with quarantine semantics per `docs/designs/architecture.md §3.1/§5 (consolidated 2026-10-13)`. The dependency audit Constitution XI required was performed per PR.
