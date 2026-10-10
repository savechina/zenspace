# Memory Strength × Knowledge Wave — 设计记录 (Phase 31 candidate)

**Date**: 2026-10-10 · **Status**: PROPOSED — 5 owner decisions pending (see "Open questions" §)
**Scope**: 记忆遗忘曲线 (Ebbinghaus retention strength) · `.agents/skills` 标准发现面 · orchestrator 知识文档注入 · sub-agent 上下文继承 · 调度链路加固 (fan-out cap 8, refund pin, checkpoint age)
**Method**: read-only design pass, every wiring point file:line-verified against the tree (designer reasoning trace included verbatim below — citations are the evidence of record).
**ID note**: designer used T200-T223; tasks.md Phase 31 renumbers to avoid the Phase 30 (gateway upgrade T201-T205) collision.

---

Task Result

Task ID: bg_1100da7c
Description: Design feature wave (read-only)
Duration: 15m 21s
Session ID: ses_ed9b5ead8ffe8s7QEzFk3dbj5B

---

This is a large-scale, read-only design task. I need to read a lot of files to verify the wiring points, and then create a structured plan. Let me start reading the key files in parallel batches.

Key files to verify:
1. crates/zen-memory/src/memvid_store.rs — metadata/payload channels for strengths
2. crates/zen-memory/src/belief.rs — anchor pattern for decay (last_decayed_at)
3. crates/zen-memory/src/memvid_index.rs — indexer
4. crates/zen-vault/src/distill/retention.rs — retention worker
5. MemoryReward sidecar — locate it (zen-vault? reward.rs)
6. crates/zen-agents/src/skill_loader.rs, skill_hit_router.rs, skill_precipitation.rs
7. crates/zen-vault/src/wiki/skill_export.rs
8. crates/zen-agents/src/orchestrator.rs — inject_wake_up_brief :813/:1567, inject_skill_hits, classify_intent, M1 assembly
9. crates/zen-agents/src/delegate_task.rs, plan_task.rs — build_sub_agent, run_single, fan-out width
10. SearchService — crates/zen-vault/src/search/service.rs, KnowledgeSearchMode
11. TUI chat knowledge injection — crates/zen-cli/src/tui/
12. Scheduler workers — crates/zen-agents/src/scheduler/
13. Migrations — crates/zen-repo/migrations/ (next is 011)
14. Config — zen-core/src/config.rs

Let's proceed with reads in batches. Given effort constraints, I should read strategically: focus on the specific wiring points that need to be cited.

Let's start with parallel glob/grep to locate files, then read them.

Let's read the main files. Batch read:
1. belief.rs's decay mechanism (around lines 230–320)
2. memvid_store.rs (entire file, checking the metadata channel)
3. memvid_index.rs (the indexer)
4. reward_sidecar.rs

Alright. What I need to check next is:
- memvid_core's PutOptions / MemoryCard — do these support metadata/payload channels? Check the memvid-core API. PutOptions might have metadata. Let's grep in the cargo registry, or check where MemoryCard's fields are used in the zen code.
- memvid_index.rs (the indexer)
- reward_sidecar.rs
- retention.rs
- skill_loader.rs
- orchestrator.rs around :813/:815/:818 and :1567/:1569/:1572, plus the M1 assembly (build_identity_context in zen_agent.rs?)
- delegate_task.rs (build_sub_agent, run_single, fan-out width)
- plan_task.rs (reconstruction of resume)
- SearchService's search mode (zen-vault/src/search/service.rs)
- Knowledge injection in TUI chat (zen-cli/src/tui/)
- Config structures in zen-core's config.rs (for naming new keys)
- The workers list in scheduler's mod.rs

Let's read memvid_index.rs and reward_sidecar.rs in parallel.


Alright. Next, I need to check ZenMemvidStore (memvid.rs) — `persist_structured_turn_tagged` — and whether memvid-core has a metadata/payload channel (PutOptions, MemoryCard fields, search hit fields). Let's look at the memvid-core API. First, I'll read zen-memory/src/memvid.rs and check what PutOptions / MemoryCard / SearchResponse hits look like in memvid-core. I'll either grep inside the cargo registry source, or check which version of memvid-core is being used.

Next, I'll check the metadata channel of the memvid-core 2.0.140 API: PutOptions, MemoryCard, SearchHit. Let me look into the registry source.

memvid-core's PutOptions has `extra_metadata: BTreeMap<String,String>` and `tags`. But the question is: does the SearchHit in search results return extra_metadata? Also, is it possible to update metadata in-place without re-putting (frames are append-only)? Let me check SearchHit / SearchResponse and investigate whether extra_metadata is returned at read time.

Confirmed: memvid-core's SearchHit holds `metadata: Option<SearchHitMetadata>`, which includes `extra_metadata: BTreeMap<String,String>` (search.rs:128-130). And PutOptions.extra_metadata is the write channel. However, frames are append-only, and there is no in-place update API for the metadata of existing frames (re-put = a new frame ID). Therefore, storing *changing* strength values in frame metadata would require re-indexing, which is incompatible with incremental strength updates. That said, since strength can be *computed* from anchors (created_at / last-reward timestamp) at query time, storage might not be needed in the first place — it may suffice to just re-rank the search results using a decay function of the hit's timestamp + a reward sidecar keyed by the source path. This is a strong design: strength is lazily computed, not stored (no second writer, no migration required). Reinforcement events come from MemoryReward sidecars keyed by relative paths. However, the reward sidecars are keyed by the card's path — we need to verify what key the orchestrator uses (increment_reward_for_query). Also, search hits have a `uri` field — we need to check what uri the indexer generates (persist_structured_turn_tagged). Let's read ZenMemvidStore's persist_structured_turn* and retrieve/search paths in memvid.rs (offset 150+).

Additionally needed: orchestrator.rs around lines 813/1567 (injection sites), skill_loader.rs, delegate_task.rs (build_sub_agent, run_single, fan-out width, validate_task), plan_task.rs's resume configuration, search/service.rs's SearchService modes, the TUI chat's knowledge injection, retention.rs's worker configuration, scheduler's mod worker list, discover report's section, config.rs's AgenticConfig keys, migration 011's naming, audit line helpers.

That's a lot to read. Let's be efficient about it. Next batch: read memvid.rs offset 150-500 (persist/retrieve), and grep for line numbers of the injection functions in orchestrator.

Alright. Next, let me read around lines 780–900 of orchestrator.rs (the execute path including injection points) and around lines 2000–2100 (inject_skill_hits), and also check the places where knowledge context is assembled (zen_agent.rs's M1 assembly? build_identity_context). Also, let me check how TUI chat performs knowledge injection. Let me read the sections of orchestrator.

Next up is skill_loader.rs (roots), delegate_task.rs (build_sub_agent, run_single, fan-out, validate_task, 32k limit), the resume path in plan_task.rs, SearchService, knowledge injection for TUI chat, the worker and config in retention.rs, and the discover report. Let's try grepping for the key line numbers all at once.


zsh handled `===` weirdly (echo === → "== not found"? Actually it was `echo ===;` — zsh interpreted `===` as a glob?). Anyway, the latter part of each command failed. Let me re-run them separately.

Next, let's read delegate_task.rs's build_sub_agent (line 136), run_single (line 335), the fan-out (lines 549-660), and TUI chat.rs's knowledge injection (lines 90-130). Also plan_task.rs's resume reconstruction too. I'll read them all together.

Important insight: TUI chat executes search before dispatching a turn and passes `knowledge` to `surface.turn_with_recovery(&session_id, &query_owned, knowledge)` — the knowledge is passed to the gateway, and then to session.knowledge. So the orchestrator itself doesn't perform search; knowledge arrives pre-populated in the SessionContext from the TUI surface (zen chat might do the same). In the case of Feature 2c — knowledge injection at the orchestrator level — we need to confirm where session.knowledge is consumed in M1 assembly, and whether the orchestrator should search on its own when knowledge is empty (gateway/QQBot/other surfaces don't populate it).

Next, check M1 assembly: zen_agent.rs's build_identity_context / prompt assembly. Grep for consumption sites of "knowledge" in zen_agent.rs and prompt.rs. Also confirm that the orchestrator's execute passes session to AgentContext, and that prompt construction reads session.knowledge.

Additional things needed:
- Fan-out code in delegate_task.rs around lines 549–660 (chunks(max_concurrent)), the constant BOUNDED_PROMPT_CHARS (32k?)
- Resume reconfiguration in plan_task.rs (compose_task_prompt, upstream_snippet)
- Structure of retention.rs (worker configuration, [agentic.retention])
- Structure of search/service.rs (SearchService::search, modes, tiers)
- scheduler's mod.rs worker list (memvid-indexer worker)
- Sections of the discover report (discover_command / discover_metrics.rs)
- Configuration: sections of AgenticConfig as a place to add keys (LoopConfig, etc.), the mechanism of environment variable overrides
- paths.rs: paths.skills(), memories(), vault paths
- Where MemoryReward sidecars are keyed — the orchestrator's increment_reward_for_query — check what key (path) it uses. Read around that function.

Let's grep for the constants at the top of delegate_task.rs, and read the relevant parts of plan_task.rs.

Good. What's needed next is:
1. `card_id_from_path` — the keying of reward sidecar (full relative path → card ID?). Check reward_sidecar.rs, around line 121 onward.
2. Where `session.knowledge` gets consumed in M1 assembly — build_identity_context / prompt in zen_agent.rs. Grep for "knowledge" in zen_agent.rs and prompt.rs.
3. The configuration of the retention.rs worker (config keys, the report-only precedent for quarantine).
4. The signature of `SearchService::search` in search/service.rs and how tiers work.
5. The memvid-indexer worker in scheduler's mod.rs (where the indexer runs; nightly; daemon-exclusive) — workers/memvid_indexer? Check the scheduler/workers directory.
6. The discover report section — orchestration_stats.rs / the report assembly in discover_command.
7. skills() and memory() in paths.rs.
8. The fields of the MemoryReward struct (types.rs).

Run all these greps together.

Check the assembly of M1 around lines 1040–1070 in zen_agent.rs, and also check memvid_indexer_worker.rs, the retention worker's settings, and search/service.rs. Read through these.


Note: memvid_indexer_worker still uses `paths.workspace_root()` — this is the part where another agent is trying to change the root again. We design on the premise of the post-modification shape (memory_root + wiki_root).

Next things to check:
- retention.rs worker (config + quarantine precedent) — grep the main parts
- search/service.rs — SearchService::search signature + tiers + fusion
- Where SearchService is available from the orchestrator (zen-agents depends on zen-vault — right). Does the orchestrator currently hold SearchService / SqliteClient? Grep for zen_vault usage inside the orchestrator.
- Config's AgenticConfig / retention settings struct.
- The place where the discover report assembles sections (crates/zen-cli/src/cmd/discover_command.rs or zen-vault's discover_metrics).
- Also, the gateway's knowledge/search RPC handler (zen-gateway's server) — where search_knowledge gets dispatched. This is the existing search entry point that the orchestrator can reuse via SurfaceClient… but actually, if we're doing orchestrator-side injection, it might be better to use SearchService directly if a SqliteClient exists in the orchestrator's wiring. Check the fields of orchestrator/wiring.

The orchestrator (zen-agents) doesn't hold SqliteClient directly — it depends on zen-vault. The gateway's knowledge/search RPC is the existing search surface. What can the orchestrator access? It has `self.executor.router()` (DefaultRouter) and `memvid_store`. Tier2 search needs SqliteClient (state.db). Does zen-agents have SqliteClient? AGENTS.md mentions about the scheduler worker that "zen-repo (scheduler worker opens state.db directly — existing dependency)". Let's check how the scheduler worker opens state.db — e.g., does zen_loop use SqliteClient? Let's grep for SqliteClient within zen-agents.

Also, check how the gateway's knowledge/search handler works (zen-gateway server) — maybe injection at the orchestrator level can reuse the same thing the TUI uses, namely the gateway RPC. But the orchestrator runs inside the gateway (hosted turn) — calling surface client from inside would be circular. Better approach: the orchestrator directly uses zen_vault::search::SearchService and SqliteClient opened from paths.state_db.

Things to verify: grep for SqliteClient within zen-agents; grep for the knowledge/search handler within zen-gateway.


The wiring context is sufficiently gathered. A few more points to quickly verify:

1. gateway's knowledge.rs handler — how does search work there (does it open SearchService? Tier list?). Read knowledge.rs lightly.
2. Fan-out chunking code in delegate_task.rs ~lines 549-660 (join_all, slot ordering) — already confirmed chunks(max_concurrent) at line 626, and the test max_concurrent_chunks_preserve_slot_order at line 1094. Also regarding the claim of ≤8/batch: AGENTS.md says "fan-out ≤8/batch, batch width ≤4 passes the consumer gate." Check where 8 is enforced — parse_requests doesn't seem to limit to 8. Grep for "8" near tasks… Actually, per the spec, ≤8/batch was the claim, but the consumer_decision gate uses batch_width <= 4 (advisory). Is there a hard cap at 8? Let's check the invoke code ~lines 540-660.
3. plan_task's resume: compose_task_prompt at line 276, replay path at ~lines 467-520. Read that region.
4. Retention worker registration and config: [agentic.retention]. Where does the retention worker execute — scheduler/workers/retention.rs. Also check the audit line name loop.retention.applied.
5. Discover report assembly: grep for section names in discover_command.rs or discover_metrics.rs (to know where to add the "memory_strength" section).
6. skill_loader.rs's ZenPaths::skills → line 118 of paths.rs: `pub fn skills()`. Check what that returns (~/.zen/skills).
7. orchestrator's execute_stream lines ~1557-1600 (mirror of execute) — already cited from grep.
8. Check ZenWiring's fields for memvid_store and tool registration (for feature 2c's wiring options).

Let's execute these reads in batch.

Confirmed: no hard cap of ≤8 on fan-out batch width in parse_requests/invoke — only the advisory consumer_decision gate (batch_width <= 4) at delegate_task.rs:237. AGENTS.md claims "≤8/batch" — spec-drift residual confirmed (Feature 3b's report point). Wait — actually AGENTS.md says "(≤8/batch, batch width ≤4 passes consumer gate)." Let me double-check whether there's an 8 cap somewhere else… grep didn't find anything like `tasks.len() > 8`. I'll note it as confirmed: there is no hard cap of 8 in the code, only the ≤12 tasks for plan.execute (schema's maxItems 12, plan_task.rs:317). I'll report it as spec drift: either implement the 8 cap, or fix the docs.

Next, check the remaining items:
1. Reservation refund on drop — run_single: `let reservation = ...try_reserve_tokens(estimated)` — where does it get released/refunded? Check the end of run_single (lines 440–500).
2. Retention worker file scheduler/workers/retention.rs — read the top section for config usage + audit lines.
3. Discover report — check sections in crates/zen-cli/src/cmd/discover_command.rs, or check the orchestration sections in zen-vault's discover_metrics.rs.
4. Return value of paths.skills() — paths.rs:118.
5. Gateway's knowledge.rs handler — read (how tiers are executed, base_dir).
6. Check the injection mirror in orchestrator's execute_stream 1557–1600.
7. Check ZenWiring's fields (memvid_store etc.) — quickly grep for "pub struct ZenWiring."
8. skill_export.rs — where are the output paths determined?
9. Config env override mechanism for adding new keys — got a good enough grasp on the pattern.
10. Migration 011 — confirm that 010 is the highest (done: the listing shows 010_communities.sql as the latest).

Batch. Done.

Next, check: reservation refund — the note in AGENTS.md says "delegate's reservation is refunded on drop — confirmed." `record_usage(reservation, actual, actual)` consumes it. If run_single returns early after a reservation (e.g., early_error path — wait no, the reservation is done at line 377, then early_error is returned from within the DELEGATE_DEPTH scope closure at lines 415–422… actually, does that early return happen after record_usage? Let me trace through: reservation is at line 377. Then lines 391–457 do the scoped async block, and in the case of an LLM round failure, early_error can become Some — the `return (Some(...), ...)` inside the closure returns a tuple, and then at line 458 `if let Some(error) = early_error { return error; }` — this returns without calling record_usage → the reservation gets dropped. Does TokenReservation refund on Drop? Check the token budget implementation. Grep for "impl Drop for.*Reservation" inside zen-agents.

Remaining read-through items:
- Token budget Drop
- The beginning of the retention worker (scheduler/workers/retention.rs)
- The discover report section (discover_command.rs)
- The body of paths.skills (paths.rs:118)
- The output path of skill_export.rs
- The injection line in orchestrator's execute_stream (lines 1557–1600)
- ZenWiring struct
- Scheduler worker registration list (mod.rs) — for adding a new worker or piggybacking
- The M1_TOP_K constant
- SearchService::search signature with tiers parameter — already confirmed at service.rs:151.
- Where the gateway builds scan_dirs for KnowledgeState (inbox + wiki roots).

Run grep/read in batch.


Very good. With this, we have almost everything we need. Remaining small confirmation items:

1. Token reservation drop semantics — grep didn't find TokenReservation inside zen-agents. It's probably in rig-compose or another module. Search for the definition of "try_reserve_tokens".
2. The injection point in orchestrator's execute_stream (lines 1557–1580) — confirm it's the same as execute.
3. skill_export's CLI: the output flag in the wiki command in zen-cli.
4. Scheduler mod registration (the InApp and Full worker lists) — where memvid-indexer and retention are registered.
5. SearchService::search signature (service.rs:151) — the tiers parameter.
6. Config: where RetentionConfig is located (config.rs:1076) — as a pattern for new config structs. Also, where LoopConfig's env override is applied (apply_env_overrides around line 2795). We have enough information for planning. No need to read every line.
7. ZenWiring's memvid_store — confirm the wiring struct holds the memvid store and how orchestrator obtains it (self.memvid_store at orchestrator.rs:789). OK.
8. Check zen-memory's lib exports for adding a new module.
9. SessionContext.knowledge — RetrievedNote type; M1_TOP_K = 5 (orchestrator.rs:50).

Let's grep for the TokenBudget reservation and check execute_stream at lines 1557–1600.

try_reserve_tokens is in the token budget module — probably zen-agents/src/context.rs? grep only matched in delegate_task and orchestrator (the call sites). The definition must be somewhere else — grep for "fn try_reserve_tokens" across all crates.

rig-compose's TokenReservation has Drop-refund (AGENTS.md states "delegate reservations are refunded on drop — confirmed"). Sufficient — cite rig-compose-0.5.0/src/budget.rs:279/465, and note the Drop refund as previously verified.

Now, the last few confirmation items:
1. Scheduler mod.rs worker registration (Full vs InApp list, where memvid-indexer is registered, the daemon-only assertion).
2. The export-skill flag (default output path) for the zen-cli wiki command.
3. The SearchService::search signature at service.rs:151.
4. Confirm the location of `ZenPaths::memory()` (line 167) — ~/.zen/memories. And paths.workspace_root() used by the indexer.
5. Check the orchestrator struct's fields (memvid_store) to see what's available for knowledge injection — around lines 60–120 of orchestrator.rs.

Let's execute all of these together.

With this, all the major wiring verification is complete. A few quick confirmations remain that are worth doing cheaply:

1. The default output path for export-skill — wiki_command.rs:472 wiki_export_skill. Read a few lines.
2. The `MemoryCard` fields in memvid-core (for MemoryCard search — entity_memories returns cards with confidence, effective_timestamp). The strength of MemoryCard search: cards have `confidence`. No problem.
3. `ZenPaths::memory()` = memories directory (line 167). The memvid store is paths.memory()/MEMVID_STORE_FILE (memvid_indexer_worker.rs:65).
4. Where SearchResponse hits expose extra_metadata — confirmed that SearchHitMetadata.extra_metadata exists at search.rs:128-130. Does zen's search path return metadata? The response includes `metadata: Option<SearchHitMetadata>` per hit. And created_at. Good.

Also, who's consuming MemvidStore::search for search ranking in production? ZenMemvidStore::retrieve uses entity_memories. "search_memories" might be in memory_service or orchestrator (the "Retrieved Memories (Memvid)" section from zen_agent's memories parameter). Let's check where zen_agent retrieves memvid memories (the build path with_memvid_store → search). Grep for "Retrieved Memories" / memvid usage in zen_agent.rs.

Actually, at zen_agent.rs:1052-1056 there's a memories parameter being displayed. Who's passing the memories? Grep for callers of "build_system_prompt_with_assembly(" and memvid searches within zen_agent.


With this, everything we need is in place. Let's check around lines 768–850 of zen_agent.rs to confirm the memory retrieval path that serves as the injection point for strength ranking (retrieve_memories uses entity_memories / select_cards, and then there's a search-based recall around line 845). Let me quickly read through that area to quote it accurately.

Now all the verification points are in place. One last simple check: MemoryCard fields (confidence, effective_timestamp, kind/entity/slot/value, source frame) — this has already been confirmed from the usage sites. And whether the frame's uri is a session ID (in the indexer's chunks, journal-YYYY-MM-DD or "knowledge-base") — yes, persist_structured_turn_tagged sets uri(session_id). So the search hits for indexed memories have uri = "journal-<date>" or "knowledge-base". The anchor date for journal chunks can be parsed from the uri (the extract_anchor_date pattern). For "knowledge-base" frames (notions/wisdom), the uri doesn't contain a source path — hmm. The hit's metadata's created_at is the indexing timestamp, not the authoring timestamp. This affects the design of strength: for M3/M4 pages, the recency anchor should come from the source file, but currently the frames don't carry a source path. Options: (a) have the indexer write extra_metadata {source_path} via PutOptions.extra_metadata (memvid supports this; SearchHitMetadata.extra_metadata is read back); this is an additive change in persist_structured_turn (need a variant that passes metadata). Then the strength path can map frame → source file path → MemoryReward sidecar (keyed by card_id_from_path of the relative path) + file mtime/frontmatter date. This closes the reinforcement loop: orchestrator's reward increments use note.path from session.knowledge (the path of the RetrievedNote obtained from vault search); the strength of memvid frames is joined by source path.

Journal chunks: uri journal-<date> provides the age anchor; reward sidecar is keyed by memories/journal/<date>.md → card_id "memories_journal_<date>"? increment_reward_for_query keys on note.path of knowledge notes — these are vault paths from SearchService, not journal paths. But access via memvid retrieve_memories currently doesn't increment reward (the reward path only covers session.knowledge notes). That's fine — reinforcement events for memvid-indexed memories come from (a) reward sidecars of wiki/vault notes joined by source_path metadata, and (b) frame created_at.

Where is the consumption point for search re-ranking? ZenMemvidStore has no ranked search consumer other than contains_turn (marker search). memvid search-based recall: is there a production consumer that calls store.search with an actual query? The retrieve path uses entity_memories (cards), not lexical search. Gateway's memory recall? Let's not over-verify; the design can add a consumer: `retrieve_relevant(query)` — hmm, but Constitution XI / "primitives without consumers are forbidden." Natural consumers: zen_agent's retrieve_memories path (cards) and… actually, MemoryCard search: cards have confidence and effective_timestamp; select_cards sorts by recency. A strength-weighted re-rank can be applied to card selection (select_cards) and/or a new ranked search used in the knowledge injection path (Feature 2c) when memvid is used. To keep it minimal: apply strength re-rank to select_cards (ForPrincipal currently uses recency order) and expose `MemvidStore::search_with_strength`… but who calls MemvidStore::search in production with actual queries? SessionJournaler's replay? No. Let me do a quick grep for callers of `.search(` on memvid in each crate.


A zsh glob expansion issue — please wrap include in quotes.


Consumer: The gateway's `memory/search` RPC (zen-gateway/src/server/memory.rs:125) — this is the production read surface for memvid search. There's also usage inside memvid.rs at 885/1000 (retrieve_with_entity_context? MemvidStoringCompactor?). That's enough: Feature 1's consumer = re-ranking at MemvidStore::search consumers, namely the gateway's memory/search handler and zen_agent's card retrieval (select_cards).

Let's quickly check around lines 100–160 of zen-gateway's memory.rs to see how search results are being used, and confirm the method names.

Now we have a fully verified overall picture. Time to write the deliverable plan. It must be thorough and structured. Let's also nail down some line numbers we'll use in citations:

Verified citations:
- belief.rs:289-306 apply_confidence_half_life (anchor = last_decayed_at.unwrap_or(last_updated).max(last_updated); coefficient 0.5^(elapsed/half_life); floor 0.01; advances last_decayed_at; idempotent). belief.rs:57 last_decayed_at field.
- memvid_index.rs:28 RECENCY_HALF_LIFE_DAYS = 30.0; :410-418 recency_weight; :24 TEMPORAL_ENTITY_TAG; index_all :56; index_m2 :202; index_m3 :271; index_m4 :327; index_incremental :108 (per earlier AGENTS.md investigation findings, no production caller — the worker drives index_all).
- memvid_indexer_worker.rs:47-95 execute; :65 store path = paths.memory().join(MEMVID_STORE_FILE); :51 workspace_root (re-rooting in progress); schedule "0 0 1 * * *" :44; registration at scheduler/mod.rs:682; profile docs mod.rs:613-614 (InApp excludes memvid-indexer — daemon-exclusive).
- memvid-core 2.0.140: PutOptions.extra_metadata BTreeMap<String,String> (types/options.rs:34), SearchHitMetadata.extra_metadata (types/search.rs:128-130), SearchHit.metadata Option<SearchHitMetadata> (search.rs:95), SearchHit.uri (search.rs:82), created_at (search.rs:122). Append-only: no in-place metadata update API (frames are immutable; no update in put path — should state "no update API in the zen wrapper; re-putting a frame issues a new frame_id" — verified that MemvidStore has no update method; memvid-core may have one but the zen wrapper doesn't expose it).
- memvid.rs:348-397 persist_structured_turn_tagged (uri=session_id, tags are turn+extra); ZenMemvidStore singleton; memvid.rs:431-447 contains_turn search.
- reward_sidecar.rs:1-120: flock dir lock :25, read_reward :42, locked_increment :83 sets last_reward_at :95; card_id_from_path :133-143; MemoryReward type types.rs:467-476.
- orchestrator.rs:321-334 increment_reward_for_query (spawns from :877-883 execute; knowledge notes paths → sidecar increment). :341 inject_wake_up_brief; :813/:1567 call sites; inject_skill_hits :2020-2059 (session.knowledge.insert(0,...) :2042, truncates at M1_TOP_K :2051); M1_TOP_K=5 :50; decision::classify_intent :818/:1572; execute :804, execute_stream :1557.
- zen_agent.rs:1025-1096 build_system_prompt_with_assembly: knowledge from session.knowledge :1046-1050; memories parameter :1052-1056 → memory_section :1065. retrieve_memories :773; retrieve_memories_structured :808 (select_cards); retrieve_memories_enriched :844.
- memvid_store.rs select_cards :153-166 (ForPrincipal is in recency order).
- delegate_task.rs: build_sub_agent :136-168 (skills via resolve_skill_ids_for_agent :141 — static skill IDs, no SkillHitRouter injection; memvid_store passthrough :164-166); run_single :335 (SessionContext::new empty :363, AgentContext from prompt only :365, reservation :376-389, record_usage :466-469, early_error return before record_usage :458-460 → reservation is dropped; rig-compose's TokenReservation refunds on drop — AGENTS.md says verified; budget.rs:279/465); evaluate_gates :234-241 (consumer_decision = batch_width <= 4 :237); BOUNDED_PROMPT_CHARS 32_000 :515; ADVISORY_TOKEN_LINE 50_000 :517; MAX_DELEGATE_ROUNDS 4 :46; validate_task :248-274; append_gates_audit :287-331 (loop.delegate.gates :312); invoke :583-651 (no hard cap of 8 on tasks.len() — parse_requests :180-228 is unlimited; chunks(max_concurrent) :626; slot ordering :606-643).
- plan_task.rs: compose_task_prompt :276-288; upstream_snippet :261-271 (4000 chars :256); resume :378-421 (checkpoint replay only on task_id+agent match :421 comment; ok-checkpoint continue :476); schema maxItems 12 :317; per-layer batch goes through run_single around :500-542.
- search/service.rs: SearchService::new(router) :37; search_fused :54 (weights :32-35; DEFAULT_SEARCH_LIMIT 20 :24); search :151 (tier None → TierSelector; preferred==2 → fusion).
- tui/chat.rs:98-117: mode = config.tui.knowledge_search; Fast → tiers ["fts"]; surface.search_knowledge(query, tiers, 5); truncate(5) :135; knowledge is passed to turn_with_recovery :144-146 → gateway → session.knowledge.
- config.rs: KnowledgeSearchMode :1685; tui.knowledge_search :1667; env var ZEN_TUI_KNOWLEDGE_SEARCH :2795-2797; AgenticConfig :449 (loop_cfg :453, tool_loop :455, retention :469); RetentionConfig :1076; SkillsConfig :1207 / auto_route :1209.
- retention.rs (zen-vault): apply_policies :240; run_policy :257; HomeReport's quarantined is report-only :36-38; policies() includes vault/archive/quarantine :227-228. Worker retention.rs (scheduler): RETENTION_SCHEDULE 0 30 3 :30; execute_with_paths :49; audit loop.retention.applied :74-81.
- gateway memory.rs: memory/search handler :118-155 (hits → frameId/snippet/score; no metadata exposure).
- gateway knowledge.rs: KnowledgeState :29-48; search handler :76-123 (service.search tier :98-100; scan_dirs; notes are capped by limit).
- dispatch.rs:504 knowledge/search handle; :64 SPAWNED_METHODS.
- paths.rs: skills() :118 (global_root/SKILLS_DIR); memory() :167; vault :100; wiki :114.
- skill_loader.rs: SkillLoader::new :59 (paths.skills()), new_from_dir :67; list_skills :75; load_skill :106 ({name}.md or {name}/SKILL.md).
- wiki_command.rs: ExportSkill :56/:275; wiki_export_skill :472 (default out = paths.skills()/WIKI_SKILL_NAME/skill_file_name()).
- discover_command.rs: report assembly :280-317 (orchestration/calibration/replay/baselines/skill_eval/success_criteria sections).
- scheduler/mod.rs: register :318; MemvidIndexerWorker registration :682; profile table docs :606-644.
- Migrations: 010 is the latest → next is 011.
- memvid_indexer index_incremental :108 (checksum sidecar memories/.index-checksums.json :104-112) — per AGENTS.md no production caller (worker drives index_all, memvid_indexer_worker.rs:78).

Now, the design decisions.

## Feature 1 — Memory Forgetting Curve

Decision on where to place Strength:
Analysis of options:
- memvid frame metadata (extra_metadata): exists at write time (PutOptions.extra_metadata, echoed in SearchHitMetadata.extra_metadata). But frames are append-only; strength changes with every access/reinforcement → would require re-put → new frame_id, store bloat, breaks replay idempotency keys. Rejected as storage for *changing* strength. However: good for *static provenance* (source_path) — the indexer currently doesn't record source paths for M3/M4 frames (uri is "knowledge-base"); adding `source_path` (+ `indexed_at`) to extra_metadata at index time is a cheap additive change that enables strength joins.
- state.db migration 011 table: violates single-writer? No — this would be derived data, and state.db already holds derived indexes. But strength is fully derivable from (created_at/anchor, reward sidecar counters, last_reward_at) — storing it creates a second source of truth that must be kept in sync; also memvid-indexer is daemon-exclusive while TUI InApp workers would want to read strength. Reading sidecars is process-safe (atomic writes + flock). Rejected: no migration needed; lazily compute strength. This is the cleanest: **no stored strength at all** — strength is a pure function evaluated at read time (Ebbinghaus with reinforcement), with inputs being: frame timestamp (SearchHitMetadata's created_at, or journal uri date anchor), MemoryReward sidecar (access_count, downstream_citations, correction_count, last_reward_at). No idempotency risk from per-cycle writes (there are no writes!) — the T153 compounding bug class is structurally eliminated because nothing accumulates; lazily-computed decay is naturally idempotent. But the task says "idempotency tests are MANDATORY" — for a pure function, the test is: stability of strength(t) across repeated evaluations + N "path" applications ≡ 1 application. Still provide an anchor-pattern analog: if the owner wants a materialized cache later, copy belief.rs's anchor. Design: pure function `retention_strength(created_at, last_event_at, events, now, half_life_days)`, events = access_count + downstream_citations (corrections treated separately — corrections are negative reinforcement? In FR-034 corrections indicate erroneous memories; for the forgetting curve, corrections should lower strength. Keep: corrections subtract? Don't invent — use only access+citations as repetition events; corrections are excluded from reinforcement, documented).

Ebbinghaus with spaced repetition: classical formulation R = e^(-t/S), where stability S increases with each review. To avoid inventing constants: use half-life formulation R = 0.5^(t_eff / half_life), reusing RECENCY_HALF_LIFE_DAYS = 30.0 (memvid_index.rs:28 — explicitly "reuses FR-025's 30-day half-life so all decay in the system shares a single constant"). Reinforcement: each repetition event extends stability. SM-2 style multipliers would be invention. Minimal defensible scheme: stability = half_life * (1 + log2(1 + events))? log2(1+events) is structural (no tunable constant) — doubling events adds one half-life... actually 1+log2(1+n): n=0 → 1×; n=1 → 2×; n=3 → 3×; n=7 → 4×. This is "each doubling of reviews adds one half-life" — a power-law spacing effect, no invented constants beyond the reused 30 days. Anchor time: last reinforcement event time = max(created_at/source date, last_reward_at) — t is measured from the most recent event (this is the anchor pattern: elapsed since anchor). So:

R = 0.5 ^ ( days_since(anchor) / (HALF_LIFE * (1 + log2(1 + access_count + downstream_citations))) )

anchor = max(content_anchor, last_reward_at.unwrap_or(content_anchor)); content_anchor = journal date (uri) or file mtime/frontmatter — for frames: metadata.created_at (index time) is wrong for old content indexed later... the re-rooting fix adds indexed_at; better to use source file date where possible (journal-{date} uri gives the authored date; M3/M4 pages have frontmatter updated_at). Keep it simple: the content_anchor resolution order is (1) journal-YYYY-MM-DD uri date, (2) source_path file mtime via extra_metadata's source_path, (3) frame's created_at. No floor: strength ∈ (0,1], no deletion.

Consumers (ranking only):
1. Gateway `memory/search` (memory.rs:118) — re-rank hits: score' = score × (min_floor + (1-min_floor)×R)? Multiplying lexical scores by strength changes ordering; keep it structural: re-rank by score × R... is that an invented formula? It's ranking only, never gating — permissible under V13-A.3 (structural). But to be safe and observable: add `strength` as a tiebreak/re-rank coefficient with settings `[agentic.memory_strength] search_rerank` default false (gate is closed; absence = no behavior change). Simpler: re-rank only among hits with equivalent scores? No — keep multiplicative re-rank behind a config flag, default OFF, flagged as calibration debt (needs arena/recall measurement before default ON).
2. zen_agent's card retrieval select_cards (memvid_store.rs:153) — currently in recency order; could reorder by strength. Also behind the same config flag.
3. Eviction candidate list → report-only for the retention worker: a new daily computation inside the RetentionWorker (or piggyback on memvid-indexer), writing strength < threshold... thresholds are invented! Instead: report-only top N lowest-strength items (structural ranking, N = reuse the report cap of 40? semantic_lint uses a 40-page cap — could make it a config key `eviction_report_size` default 20 = DEFAULT_SEARCH_LIMIT precedent (service.rs:24)). Written as a `memory-strength-report.json` style section, or as an audit line `loop.memory.strength` + discover report section. Never delete (quarantine precedent retention.rs:36-38).

Computation site: lazily compute at read time (no worker needed for strength itself); report generation piggybacks on the nightly memvid-indexer worker (already daemon-exclusive, already opens the store + touches the memories directory) — after index_all, compute a report across reward sidecars + frame stats... frame-level iteration requires scanning all frames — memvid-core may not expose a cheap full scan. Alternative: the report only covers *reward sidecars + journal/wiki file anchors* (file-based, cheap, no store scan): enumerate memories/.reward/*.json + journal dates → strength distribution → audit line. This is honest: strength over indexed sources (files), not over frames. Frames inherit strength via uri/source_path joins at search time. Good — avoids full store scan entirely.

Reinforcement writer: already exists — increment_reward_for_query (orchestrator.rs:321, called from :877-883). Gap: memvid card retrieval (retrieve_memories) doesn't increment reward. Feature 1 can add: when a memvid card/search hit is injected into M1, increment the sidecar keyed by the hit's source_path via card_id_from_path — reusing FR-034's mechanism (T051-compatible single writer? The reward sidecar is already written from the orchestrator; adding another call site in the same crate is fine — same writer path, flock-protected across processes).

Config keys (all under `[agentic.memory_strength]`, gate defaults to CLOSED):
- enabled (default false, env var ZEN_MEMORY_STRENGTH) — master gate; absence = zero behavior change.
- half_life_days (default 30.0 = reuses RECENCY_HALF_LIFE_DAYS; overridable, env var ZEN_MEMORY_STRENGTH_HALF_LIFE) — reuse not invention; config exists for tunability, default is the documented constant.
- search_rerank (default false, env var ZEN_MEMORY_STRENGTH_RERANK) — ranking only.
- eviction_report_size (default 20 — DEFAULT_SEARCH_LIMIT precedent, env var ...) — structural.
Flagged as calibration debt: the spacing coefficient (1+log2(1+n)) shape is structural but uncalibrated; rerank default OFF until arena measurement is available.

No migration. No new tables. No second writer to markdown. Strength is never persisted (derived at read time) → T153 class is structurally impossible; still, the mandatory idempotency test: N evaluations at a fixed `now` are identical; evaluation after a "virtual cycle" where now advances by Δ equals a single evaluation at now+Δ (no compounding state).

Indexer changes (prerequisite, additive):
- persist_structured_turn_tagged → add a metadata-carrying variant: PutOptions.extra_metadata {"source_path": rel, "tier": "m2|m3|m4"} — memvid-core options.rs:34 supports it; SearchHitMetadata.extra_metadata search.rs:128 echoes it back. Old frames without metadata → strength falls back to uri date/created_at (backward compatible).
- Design based on the post-re-rooting shape: MemvidIndexer::new(memory_root, wiki_root) explicit roots.

Wiring:
- zen-memory/src/strength.rs (new module): pure functions retention_strength(...), resolve_anchor(...), StrengthInputs; export from lib.rs.
- memvid_store.rs select_cards: optional strength-aware ordering variant `select_cards_with_strength` (additive; don't change select_cards behavior when gate is OFF).
- gateway memory.rs:118 search handler: after response, if enabled+rerank: compute R per hit from extra_metadata/source join → re-rank + include "strength" in hit JSON (additive field).
- scheduler/workers/memvid_indexer_worker.rs execute(): after index_all, if enabled: build eviction report from reward dir + journal dates → audit line `loop.memory.strength` (kind name) + write logs/memory-strength-report.json (atomic write via zen_core::atomic_file::write_atomic) — fail-open.
- discover_command.rs report: new section `memory_strength` reading the report json (additive, mirrors :284 replay section pattern).
- Orchestrator reward increments for memvid-derived injections (if cheap): optional task.

Tests:
- Idempotency: evaluate twice → identical; "N cycles ≡ 1" via anchor arithmetic (T153 lesson).
- Monotonicity: strength decreases with age; increases with events; never ≤0, never >1.
- Anchor resolution order (uri date > source mtime > created_at).
- Backward compatibility: frames without extra_metadata → fallback anchor, no error.
- Rerank gate OFF → byte-identical hit order (pinning old behavior).
- Report is fail-open on corrupted sidecars.
- Migration test: N/A (no migration).

## Feature 2 — .agents/skills + Knowledge Injection

(a) SkillLoader multi-root:
- SkillLoader::new(paths) currently uses single root paths.skills() (skill_loader.rs:59-61). Change to multi-root: roots = [paths.skills() (zen-native, highest priority), ~/.agents/skills (user scope), walk workspace .agents/skills from cwd to repo root (project scope)]. Priority decision: pi/codex precedent — project scope is closest to cwd and wins? External research has codex+pi: user ~/.agents/skills + project <repo>/.agents/skills, name collision → first-wins+warn. For zen: recommend priority order zen-native (~/.zen/skills) > project (.agents/skills closest to cwd wins) > user (~/.agents/skills); collision → first-wins + tracing::warn (pi precedent). Rationale: zen-native root hosts machine-generated zen-wiki exports; user overrides must not silently shadow zen's own exports... actually hmm — the operator might want to override zen-wiki skills. Counterargument: first-wins with warn is observable; keep zen-native first for stability (export-skill discovery is pinned by tests skill_export_discovery.rs). Note override priority as an open question for the owner.
- Walk to repo root: reuse git repo root detection? .zen/ marker is the project context marker (AGENTS.md path scoping). Decision: walk cwd upward until filesystem root or a directory containing `.git`, collecting `.agents/skills` at each level; closest wins. Limit walk depth with structural constant = stop at repo root (git anchor), documented.
- Implementation shape: SkillLoader holds `roots: Vec<PathBuf>`; list_skills merges (first-wins dedup by name + warn); load_skill probes roots in order. new_from_dir stays as-is (single root). Additive constructor `new_multi_root(paths, workspace_root)`.
- Router unchanged: inject_skill_hits (orchestrator.rs:2029) constructs SkillLoader::new(&paths) — gets multi-root for free via constructor change. skill_trigger_eval compute(&paths.skills()) (discover_command.rs:301) stays zen-root only (evaluating confirmed zen skills; note this).

(b) export-skill target: add `--target agents|zen` flag (or `--agents`) to ExportSkill (wiki_command.rs:56, dispatch :275, implementation :472): writes to ~/.agents/skills/zen-wiki/SKILL.md. Default unchanged (zen root). Config alternative rejected: flag is per-invocation intent, config key adds a dormant surface. Keep `--output` explicit path priority.

(c) Orchestrator knowledge injection:
- Current state: TUI passes knowledge via gateway turn (chat.rs:98-146); orchestrator does no retrieval at all; zen chat? (probably similar). Gateway/qqbot/MCP surfaces don't pass knowledge → turns arriving there have no KB context.
- Design: a new `inject_knowledge_context` in orchestrator.rs, inserted after inject_skill_hits, before classify_intent? Order matters: intent classification doesn't need knowledge; knowledge injection needs the intent result (skipping Conversation). So insert after ladder classification (execute :847 / execute_stream :1601), before build_agent :860/:1614. Condition: only if session.knowledge is empty or below a small count (surfaces that already passed knowledge — TUI — skip; no double retrieval). Gate: `[agentic.orchestrator] knowledge_inject` default false (absent = closed), env var ZEN_ORCHESTRATOR_KNOWLEDGE_INJECT. Skip on Conversation category (intent.category — ladder output; Query/Action/System get injection) — rationale: small talk doesn't need KB; reuse existing classification, no new heuristic.
- Retrieval mechanism: orchestrator doesn't hold SqliteClient. Options: (A) open SqliteClient::open_lazy(paths.db()/state.db) per turn — expensive per turn; (B) hold an Arc<SearchService>+client in orchestrator constructed on first use (OnceLock) — zen-agents already depends on zen-repo (scheduler worker opens state.db, existing dependency) and zen-vault; (C) route via memvid store lexical search (already in orchestrator :69 memvid_store) — but memvid search only covers nightly-indexed content; (D) gateway RPC — circular inside daemon. Choose B, with lazy init + fail-open (degradation: no injection + warn). Mode: tier decision — reuse KnowledgeSearchMode semantics? That's [tui] scoped. For orchestrator: config `[agentic.orchestrator] knowledge_tiers` default "fts" (Fast equivalent; T138 decision: don't move to fusion by default without measured recall deficit) — search via SearchService::search(query, dir, client, Some(2), None, Some(limit)) against vault wiki dir; limit default 3 (char budget precedent). Budget: 4000 chars per note? Precedent: UPSTREAM_SNIPPET_MAX_CHARS 4000 (plan_task.rs:256); total injection budget config knowledge_budget_chars default 4000 (total), truncated at char boundary. Latency guard: wrap with tokio::time::timeout — reuse? Stream first-token budget is the LLM path; for search: FTS5 queries are ~ms; guard with 2-second timeout config? Invention... make it structural: timeout_ms config default 2000 flagged as calibration debt, on timeout → skip injection + warn (fail-open, never block a turn). Sensitivity: attach RetrievedNote with sensitivity from the session policy; sanitize content with InputSanitizer (wake-up brief precedent :361).
- Insert into session.knowledge (push after existing skill hits, truncate at M1_TOP_K :2051 pattern), RetrievedNote { path, content, sensitivity, relevance } — flows into M1 via zen_agent.rs:1046-1050, and into increment_reward_for_query :878 (sidecar increments — synergy with Feature 1).

(d) Sub-agent propagation:
- Today: run_single creates a fresh empty SessionContext (delegate_task.rs:363) — sub-agents get zero knowledge/skills; build_sub_agent only grants static skill IDs (:141), no SkillHitRouter injection.
- Decision: inherit parent's already-retrieved block (cheap, bounded) — parent's session.knowledge is available in the orchestrator but run_single doesn't receive a session... DelegateTaskTool holds sensitivity via SharedSensitivity (:279-284) — same pattern: hold a SharedKnowledge snapshot (Arc<Mutex<Vec<RetrievedNote>>>) updated by orchestrator before the tool loop after injection; run_single copies into the sub's SessionContext.knowledge, with depth-aware budget shrinking: depth 1 gets full block truncated to knowledge_budget_chars; deeper levels halve per level? Invention... structural: budget shrinks by half per depth level (documented as structural, cap levels ≤3). Fresh retrieval per subtask is rejected: cost (one search per subtask × fanout 8) + latency + sub-prompts already have 32k bounded brief; also delegate sub-turns are synchronous within the parent turn.
- Respect 32k bound: knowledge goes into SessionContext (prompt assembly), not task.prompt — BOUNDED_PROMPT_CHARS validation unchanged; but token estimate :376 (prompt.len()/4 + 1024) should account for inherited knowledge: add knowledge_chars/4 to estimate (additive).
- Skill injection for sub-agents: run SkillHitRouter against subtask prompt in run_single (reusing orchestrator's inject_skill_hits logic — extract into a free function `inject_skill_hits_with(router, loader, session, query)` to avoid duplication; orchestrator method delegates). Gate: same [skills.auto_route] enabled + new `[agentic.delegate] inherit_context` default false? Or piggyback on the knowledge_inject gate? Separate key: `[agentic.delegate] context_inheritance` default false (absent = closed), env var ZEN_DELEGATE_CONTEXT_INHERITANCE — governs both knowledge block + skill hit injection for sub-agents.
- Audit: extend loop.delegate.gates entries with per-task `inherited_notes: n, inherited_chars: n, skill_hits: n` (additive fields, delegate_task.rs:297-310).

## Feature 3 — Chain Hardening (minimal, 006 extensions)

(a) Context propagation contract: introduce `SubAgentContext { knowledge: Vec<RetrievedNote>, skill_hits: Vec<String>, depth_budget_chars: usize }` composed in one function `compose_sub_session(...)` inside delegate_task.rs, used by both run_single and the plan_task batch path — single seam, contract documented. Rejected: prompt string concatenation only (loses sensitivity tagging + reward bookkeeping; also 32k bound pressure).
(b) Verification report (from today's code reading):
- Fanout width: parse_requests has no hard cap of ≤8 (delegate_task.rs:180-228) — AGENTS.md/spec claim "≤8/batch" is drift; only the advisory consumer_decision ≤4 (:237) and plan.execute's maxItems 12 (plan_task.rs:317). Fix: enforce a hard cap of 8 in parse_requests (spec-compliant, structural) or correct docs. Recommend: enforce 8 (spec says so; cap = reuse of recorded value).
- Checkpoint staleness: resume replays ok checkpoints matching on task_id AND agent (plan_task.rs:421 comment region); stale-claim steal is >3600s — risk: replayed checkpoint output is injected as upstream_snippet (4000 cap :256) — stale output from a previous run can become inconsistent with re-executed sibling tasks; minimal mitigation: record checkpoint elapsed time in audit; don't redesign. Report only.
- Reservation refund: the early_error path (LLM round failure :458-460) returns before record_usage :466 — reservation is dropped; rig-compose's TokenReservation refunds on drop (AGENTS.md 2026-09-07 entry says verified; budget.rs:465 record_usage consumes it). Verify existence of Drop impl in rig-compose budget.rs — confirmed "impl Drop" in budget.rs? Haven't grepped. Quick check for plan accuracy.
- Slot ordering of join_all: verified correct (:606-643, test :1094).
(c) Per-subtask telemetry: additive audit fields from 2d + `knowledge_inherited` bool + chars in the run_single completion info! (:486-491).
(d) Resume reconstruction: compose_task_prompt (:276) builds from base prompt + upstream snippets; resumed tasks are re-executed via run_single (:506 prompt composition) — if inheritance lives in run_single/compose_sub_session (a), then resume automatically gets the same injection — answer: yes, and no separate wiring is needed if (a) places the seam in run_single; plan_task calls run_single (:629 delegate invoke and plan layer batch). Verified: plan_task.rs uses run_single via self.delegate.run_single — check around :500-542. Saw outcome = .... Won't re-verify; AGENTS.md states per-layer parallel batch goes through delegate run_single. Good.

Let's quickly verify rig-compose's TokenReservation Drop refund and plan_task's use of run_single for accuracy.

All verified. TokenReservation Drop refund: rig-compose budget.rs:253-259. plan_task holds Arc<DelegateTaskTool> (:62), uses validate_task (:510) and run_single (docs :9). Good.

Next, write the deliverables. Must be comprehensive: per-feature decisions, data model, config table, wiring file:line, task decomposition with [P] markers and dependencies, same-file notes, test matrix, acceptance criteria; sequencing/commits; ≤5 open questions; risks. No file edits. It'll be a long response, but expected for a design deliverable. Let's write it concisely and completely.

# Work Plan — 3 Features (READ-ONLY design; all wiring points verified against source)

Verification notes up front (things I confirmed rather than trusted):

- memvid-core 2.0.140 **does** have a per-frame metadata channel: `PutOptions.extra_metadata: BTreeMap<String,String>` (`~/.cargo/registry/.../memvid-core-2.0.140/src/types/options.rs:34`) echoed back on search hits as `SearchHitMetadata.extra_metadata` (`src/types/search.rs:128-130`), plus `SearchHit.uri` (`search.rs:82`), `score` (`:93`), `created_at` (`:122`). Frames are append-only — there is **no in-place metadata update** in `MemvidStore` (`crates/zen-memory/src/memvid_store.rs:30-138`), so mutable state cannot live in frame metadata.
- The exact decay mechanism to copy: `Belief::apply_confidence_half_life` (`crates/zen-memory/src/belief.rs:289-306`) — anchor = `last_decayed_at.unwrap_or(last_updated).max(last_updated)` (:290-293), factor `0.5^(elapsed_days/half_life)` (:298), advance anchor on apply (:304), floor 0.01 (:299). The 30-day constant is already shared: `RECENCY_HALF_LIFE_DAYS = 30.0` (`crates/zen-memory/src/memvid_index.rs:26-28`).
- Reward sidecar facts: `locked_increment` sets `last_reward_at` on every increment (`crates/zen-vault/src/distill/reward_sidecar.rs:95`); keys derive from full relative path via `card_id_from_path` (`:133-143`); `MemoryReward {access_count, downstream_citations, correction_count, last_reward_at}` (`crates/zen-vault/src/distill/types.rs:467-476`); the production writer is `increment_reward_for_query` over `session.knowledge` paths (`crates/zen-agents/src/orchestrator.rs:321-334`, spawned at `:877-883`).
- Orchestrator injection seam confirmed at `orchestrator.rs:813/815/818` (execute) and `:1567/1569/1572` (execute_stream); intent result available at `:847`/`:1601`; `build_agent` at `:860`/`:1614`; `M1_TOP_K = 5` (`:50`); `session.knowledge` → M1 assembly at `crates/zen-agents/src/zen_agent.rs:1046-1050` → `memory_section` `:1065`.
- TUI is the **only** knowledge-populating surface today: `crates/zen-cli/src/tui/chat.rs:98-117` (Fast → `tiers=["fts"]`, limit 5) then passes knowledge into the gateway turn (`:144-146`). Gateway/qqbot/MCP turns arrive with empty `session.knowledge`.
- Sub-agents get nothing: `run_single` builds a **fresh empty** `SessionContext` (`crates/zen-agents/src/delegate_task.rs:363`); `build_sub_agent` grants only static skill IDs (`:141`, `:154-157`), never `SkillHitRouter` hits, never knowledge.
- Fan-out: **no hard ≤8 cap exists** — `parse_requests` accepts unbounded `tasks[]` (`delegate_task.rs:180-228`); only the advisory `consumer_decision: batch_width <= 4` gate (`:237`) and chunked concurrency (`:626`). Spec drift confirmed. Slot ordering verified correct (`:606-643`, test `:1094`). `BOUNDED_PROMPT_CHARS = 32_000` (`:515`), `MAX_DELEGATE_ROUNDS = 4` (`:46`).
- Reservation refund verified: early-error return (`delegate_task.rs:458-460`) drops the reservation without `record_usage`; `TokenReservation`'s `Drop` refunds (`rig-compose-0.5.0/src/budget.rs:253-259`). **Sound — no leak.**
- Plan resume: recomposition flows through `compose_task_prompt` (`plan_task.rs:276-288`, 4000-char upstream snippets `:256`) → `run_single` via the shared `Arc<DelegateTaskTool>` (`:62`, `:9`, `:510`). So a context seam placed **inside `run_single`** automatically covers resume — no separate wiring.
- Indexer worker: `memvid_indexer_worker.rs:47-95` (store at `paths.memory()/MEMVID_STORE_FILE` `:65`, nightly `0 0 1 * * *` `:44`, daemon-exclusive per profile table `scheduler/mod.rs:613-614`, registration `:682`); still rooted on `paths.workspace_root()` `:51` — design below assumes the POST-fix `(memory_root, wiki_root)` constructor.
- Retention precedent: report-only quarantine (`crates/zen-vault/src/distill/retention.rs:36-38`), `apply_policies` `:240`, worker + `loop.retention.applied` audit (`scheduler/workers/retention.rs:30`, `:74-81`).
- Discover report assembly point: `crates/zen-cli/src/cmd/discover_command.rs:280-317` (sections `orchestration/calibration/baselines/replay/skill_eval/success_criteria`).
- SkillLoader single root: `skill_loader.rs:55-68` (`paths.skills()` = `~/.zen/skills`, `crates/zen-core/src/paths.rs:118-120`); `new_from_dir` exists (`:67`). export-skill default target: `crates/zen-cli/src/cmd/wiki_command.rs:472-490` (`paths.skills()/zen-wiki/SKILL.md`), subcommand `:56`, dispatch `:275`.
- Search stack: `SearchService::search(query, base_dir, client, tier, domain_filter, limit)` (`crates/zen-vault/src/search/service.rs:151`), fusion only when tier auto-selects to 2 (`:167-172`); gateway `knowledge/search` handler (`crates/zen-gateway/src/server/knowledge.rs:76-123`); orchestrator holds **no** `SqliteClient` (only `memvid_store`, `orchestrator.rs:69`) but zen-agents already opens state.db in workers (`scheduler/workers/zen_loop.rs:1212`) — the dependency exists.
- Next migration number: **011** (`crates/zen-repo/migrations/` tops out at `010_communities.sql`).

---

## FEATURE 1 — 记忆遗忘曲线 (retention strength over memvid-indexed memories)

### Design decisions

| #     | Decision                                                | Choice                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | Rejected alternative (one line)                                                                                                                                                                                                                   |
| ----- | ------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| F1-D1 | Where strength lives                                    | **Nowhere — computed lazily at read time** from (content anchor, `MemoryReward` sidecar counters, `last_reward_at`). Inputs: frame `uri` date / new `extra_metadata.source_path` / file mtime; sidecars at `memories/.reward/`                                                                                                                                                                                                                                                                  | state.db migration 011 table: creates a second source of truth needing sync writes from a daemon-exclusive worker while InApp processes read — and derived data that can be recomputed must not be stored (single-writer discipline, T051 spirit) |
| F1-D2 | …but frame metadata **is** used, for static provenance only | Indexer writes `extra_metadata {source_path, tier, indexed_at}` at put time (write-once, immutable — fits append-only frames; `memvid-core options.rs:34`)                                                                                                                                                                                                                                                                                                                            | Storing mutable strength in frame metadata: impossible without re-put → new frame_id → store bloat + breaks `SessionReplayer` blake3 idempotency                                                                                                    |
| F1-D3 | Decay function                                          | `R = 0.5 ^ (days_since(anchor) / (HALF_LIFE × (1 + log2(1 + events))))`, `events = access_count + downstream_citations`; `anchor = max(content_anchor, last_reward_at)`; half-life **reuses 30.0** (`memvid_index.rs:28` / `belief.rs:278`); clamp `R ∈ (0,1]`, no floor-accumulation state                                                                                                                                                                                                       | Additive per-access boost constants (SM-2 intervals): invented numbers, V13-A.3 violation. `log2(1+n)` is structural (each doubling of reviews adds one recorded half-life)                                                                         |
| F1-D4 | Idempotency strategy                                    | **Stronger than the belief anchor**: since R is a pure function of persisted inputs and `now`, per-cycle compounding (the T153 P1, `belief.rs:267-273`) is *structurally impossible* — nothing accumulates in a strength store. The anchor pattern still governs the formula (elapsed measured from `last_reward_at`-style anchor, never from a stored "decayed" value). Mandatory tests below pin both properties                                                                            | Materialized strength cache with `last_computed_at` anchor: only if profiling ever demands it; copy `apply_confidence_half_life` (`belief.rs:289-306`) verbatim then                                                                                    |
| F1-D5 | Corrections are **not** reinforcement                       | `correction_count` excluded from `events` (a corrected memory is not a strengthened one); it is surfaced in the eviction report as a risk flag                                                                                                                                                                                                                                                                                                                                        | Subtracting corrections: would need an invented penalty weight                                                                                                                                                                                    |
| F1-D6 | Consumer 1 — search rerank                              | Gateway `memory/search` (`zen-gateway/src/server/memory.rs:118-155`) and card selection (`memvid_store.rs:153-166`): rerank `score × R`, additive `strength` field in hit JSON. **Ranking only, never filtering/deletion.** Gate default OFF (calibration debt: needs arena/recall measurement before default-on, T138 discipline)                                                                                                                                                              | Deleting low-R frames: violates "ranking only" + memvid append-only                                                                                                                                                                               |
| F1-D7 | Consumer 2 — eviction CANDIDATE report                  | Nightly, piggybacked on `MemvidIndexerWorker::execute` (already daemon-exclusive, already holds the store + touches `memories/`): file-based sweep (reward sidecars + journal dates + wiki/wisdom mtimes — **no full frame scan**) → lowest-R `eviction_report_size` entries → `logs/memory-strength-report.json` (atomic) + one `loop.memory.strength` audit line. **Report-only, mirroring retention quarantine** (`retention.rs:36-38`); RetentionWorker itself unchanged (no new deletion policy) | Standalone worker: +1 scheduler slot (17→18 Full) for work that needs the same nightly cadence and the same roots                                                                                                                                 |
| F1-D8 | Reinforcement events                                    | Reuse FR-034 writer path unchanged (`orchestrator.rs:321-334`). **Gap closed**: memvid-derived injections don't currently increment sidecars — add increments for injected memvid hits keyed by `card_id_from_path(source_path)` (same flock discipline; same-crate call site, not a second writer)                                                                                                                                                                                       | New event log: duplicates `.reward`                                                                                                                                                                                                                 |

### Data model

- No migration. No new tables. No markdown schema change.
- New: frame `extra_metadata` keys `source_path` (vault-relative, e.g. `vault/wiki/notions/rust.md` or `memories/journal/2026-06-01.md`), `tier` (`m2|m3|m4`), `indexed_at` (RFC3339). Old frames lack them → anchor fallback chain `uri journal-date → created_at → indexed_at`, fully backward-compatible.
- New module `crates/zen-memory/src/strength.rs`: `StrengthInputs {content_anchor: DateTime<Utc>, last_event_at: Option<DateTime<Utc>>, events: u64}`, `retention_strength(inputs, now, half_life_days) -> f64`, `anchor_from_uri(uri) -> Option<NaiveDate>` (generalizes `extract_anchor_date`, `memvid_index.rs:399-405`), `eviction_report(...)`; exported via `lib.rs`.

### Config keys (`[agentic.memory_strength]`, zen-core config.rs near `RetentionConfig` `:1076`)

| Key                  | Default                                            | Env                             | Gate semantics                                                                          |
| -------------------- | -------------------------------------------------- | ------------------------------- | --------------------------------------------------------------------------------------- |
| `enabled`              | `false`                                              | `ZEN_MEMORY_STRENGTH`             | absent/false ⇒ zero behavior change anywhere (no metadata writes, no rerank, no report) |
| `half_life_days`       | `30.0` (reuse `RECENCY_HALF_LIFE_DAYS`)                | `ZEN_MEMORY_STRENGTH_HALF_LIFE`   | clamp 1.0..=365.0; unparsable → default + warn (cron-timezone discipline)               |
| `search_rerank`        | `false`                                              | `ZEN_MEMORY_STRENGTH_RERANK`      | **calibration debt flag**: ranking-only; default-off until arena measurement                |
| `eviction_report_size` | `20` (`DEFAULT_SEARCH_LIMIT` precedent, `service.rs:24`) | `ZEN_MEMORY_STRENGTH_REPORT_SIZE` | structural (report length only)                                                         |

### Wiring points (file:line)

1. `crates/zen-memory/src/memvid.rs:348-362` — add `persist_structured_turn_meta(session_id, content, extra_tag, extra_metadata)` variant building `PutOptions` with `extra_metadata`; existing methods delegate with empty map (byte-identical when gate off).
2. `crates/zen-memory/src/memvid_index.rs` — `index_m2_episodic :248`, `index_m3_semantic :303`, `index_m4_wisdom :370`, `index_single_file :183`: pass `source_path/tier/indexed_at` when enabled (POST-fix roots: `memory_root` for journal, `wiki_root` for notions/wisdom).
3. `crates/zen-memory/src/memvid_store.rs:153-166` — additive `select_cards_with_strength(store, selection, query, sidecar_lookup, now, half_life)`; `select_cards` untouched.
4. `crates/zen-gateway/src/server/memory.rs:141-152` — after `search()`, when enabled+rerank: join hits → `extra_metadata.source_path` → sidecar → R; re-sort; add `"strength"` to hit JSON (additive field, protocol MINOR note if contracts list response fields).
5. `crates/zen-agents/src/scheduler/workers/memvid_indexer_worker.rs:77-95` — post-`index_all`, when enabled: build report (fail-open), `write_atomic` to `logs/memory-strength-report.json`, append `loop.memory.strength` audit line (`zen_core::jsonl::append_jsonl_line`, retention-worker pattern `scheduler/workers/retention.rs:74-88`).
6. `crates/zen-cli/src/cmd/discover_command.rs:280-317` — additive `memory_strength` section reading the report json (mirror `replay` `:284`).
7. `crates/zen-agents/src/orchestrator.rs:321-334` — extend `increment_reward_for_query` (or a sibling fn) to also increment sidecars for memvid-sourced injections carrying `source_path` (Feature 2c synergy).
8. `crates/zen-core/src/config.rs` — new `MemoryStrengthConfig` in `AgenticConfig` (`:449`), merge + `apply_env_overrides` (pattern at `:2795-2797`), `config/config.toml` commented sample.

### Task breakdown

| ID       | Task                                                                                 | Deps      | Files (serialization groups)                                       |
| -------- | ------------------------------------------------------------------------------------ | --------- | ------------------------------------------------------------------ |
| T200     | `MemoryStrengthConfig` + env + merge + toml sample + sentinel test                     | —         | **G1**: zen-core config.rs                                             |
| T201 [P] | `zen-memory/src/strength.rs` pure module + lib export + unit tests (incl. idempotency) | —         | **G2**: zen-memory                                                     |
| T202     | `persist_structured_turn_meta` + indexer `extra_metadata` (POST-fix roots)               | T200,T201 | G2                                                                 |
| T203 [P] | `select_cards_with_strength`                                                           | T201      | G2 (serialize with T202 — same crate)                              |
| T204     | gateway `memory/search` rerank + additive `strength` field                               | T200,T201 | **G3**: zen-gateway                                                    |
| T205     | indexer-worker nightly report + `loop.memory.strength` audit                           | T200-T202 | **G4**: zen-agents scheduler                                           |
| T206 [P] | discover report `memory_strength` section                                              | T205      | **G5**: zen-cli                                                        |
| T207     | orchestrator reward-increment for memvid injections                                  | T202      | **G6**: zen-agents orchestrator (serialize with T210/T211 — same file) |

### Test matrix

- **Idempotency (MANDATORY, T153 lesson)**: (a) `retention_strength` evaluated N times at fixed `now` → identical; (b) "N cycles ≡ one": advancing `now` in k steps of Δ and re-evaluating equals one evaluation at `now+kΔ` (no compounding state exists — pin it); (c) if any materialization is ever added, the belief-anchor test shape (`belief.rs` tests `:834-846`) is the template.
- Monotonicity: R strictly decreases with age; strictly increases with `events`; `0 < R ≤ 1` for all inputs incl. clock skew (future anchor → R=1, `recency_weight` precedent `memvid_index.rs:868-870`).
- Anchor resolution: uri-date > source mtime > created_at; missing metadata (legacy frames) → fallback, no error.
- Gate-off byte-identity: rerank off → hit order identical to today; `enabled=false` → indexer writes no `extra_metadata` (pin via put-options capture).
- Metadata round-trip: put with `source_path` → search hit echoes it (memvid-core channel test).
- Report: corrupt sidecar → fail-open + warn (never aborts worker); report size capped; audit line emitted only when enabled.
- Rerank gate: unparsable `half_life_days` env → default + warn.

### Observable acceptance (no invented numbers)

- `zen discover report` gains a `memory_strength` section; `logs/audit.jsonl` gains `loop.memory.strength` lines with `{computed, lowest: n, half_life_days}`.
- With `enabled=false` (default): `bin/test` delta is new tests only; zero behavior change (pinned by gate-off tests).
- Strength distribution is *reported*, never enforced: no deletion path exists in the feature.

---

## FEATURE 2 — `.agents/skills` standard integration + knowledge-doc injection

### Design decisions

| # | Decision | Choice | Rejected alternative |
|---|----------|--------|---------------------|
| F2-D1 | Multi-root discovery | `SkillLoader` gains `roots: Vec<PathBuf>` = [`~/.zen/skills` (zen-native), workspace `.agents/skills` walked cwd→repo root (closest wins), `~/.agents/skills` (user)]. Walk stops at the `.git` anchor or FS root (structural bound, no depth constant) | Config-listed roots: dormant-key risk, and codex/pi both converge on the fixed two-plane convention |
| F2-D2 | Collision policy | **first-wins + `tracing::warn!` naming both paths** (pi precedent); zen-native root first so `zen wiki export-skill`'s pinned discovery (`zen-agents/tests/skill_export_discovery.rs`) can never be silently shadowed | Last-wins/merge: ambiguous, and overriding machine-generated exports breaks the export→discover contract test |
| F2-D3 | Router unchanged | `inject_skill_hits` (`orchestrator.rs:2020-2059`) keeps trigram-Jaccard 0.72/max_hits 1/top-5 — only the loader's root set changes; malformed SKILL.md skipped with warn (already `load_skill` `Ok/Err` shape, `skill_loader.rs:106`) | Embedding scorer activation: T075 deliberately keeps embedding off the per-turn path |
| F2-D4 | export-skill target | New `--target zen\|agents` flag on `ExportSkill` (`wiki_command.rs:56`), default `zen` unchanged; `--output` still wins. Flag, not config: per-invocation intent, no dormant key | Config key: adds a persistent surface for a one-shot decision |
| F2-D5 | Orchestrator knowledge injection point | New `inject_knowledge_context(...)` inserted **after** the intent ladder (`orchestrator.rs:847` / `:1601`) and **before** `build_agent` (`:860`/`:1614`), both entry points; skipped when `intent.category == Conversation` (uses existing ladder output — no new classifier) and when `session.knowledge` is already non-empty (TUI-preceded turns never double-retrieve) | Injection before classification: can't skip Conversation; per-surface injection (gateway handler): misses qqbot/MCP/CLI surfaces |
| F2-D6 | Retrieval stack | Lazily-initialized (`OnceLock`) `SearchService + SqliteClient::open_lazy(paths.db())` held by the orchestrator; `search(query, vault_root, client, Some(2), None, Some(limit))` — **FTS5-only tier 2, honoring T138** (fusion stays out of the default path). Fail-open everywhere: init failure/timeout/error → skip injection + warn | Gateway RPC from inside the daemon: circular; memvid lexical search: nightly-stale index; fresh per-turn SqliteClient: migration-lock churn |
| F2-D7 | Budgets | Total injected block ≤ `knowledge_budget_chars` (default **4000** — `UPSTREAM_SNIPPET_MAX_CHARS` precedent, `plan_task.rs:256`), per-note truncate char-boundary; limit default **3** notes; `InputSanitizer` + 256KiB-per-source cap (wake-up-brief hygiene precedent, `orchestrator.rs:336-361`); retrieval wrapped in `tokio::time::timeout(knowledge_timeout_ms)` default **2000ms** — **flagged calibration debt** (FTS5 is ms-scale locally; the bound is a hang guard, not a tuned value) | No timeout: violates "every path has a budget" (the `complete_cached` P2 lesson) |
| F2-D8 | Sub-agent propagation | **Inherit the parent's already-retrieved block** (cheap, bounded, zero extra searches): `DelegateTaskTool` gains a `SharedKnowledge` snapshot (`Arc<Mutex<Vec<RetrievedNote>>>`, exact `SharedSensitivity` pattern `delegate_task.rs:279-284`) refreshed by the orchestrator after injection; `run_single` copies it into the sub `SessionContext` (`:363`) with depth-halved budget (structural: `budget >> depth`, clamp ≥512 chars, max_depth ≤3 already caps it) | Fresh retrieval per subtask: one search × fan-out ≤8 per layer, latency + state.db contention, and sub-prompts are already bounded 32k briefs |
| F2-D9 | Sub-agent skill hits | Extract the body of `inject_skill_hits` into a free `inject_skill_hits_into(router, session, query)`; `run_single` calls it on the subtask prompt (multi-root loader comes free via F2-D1) | Duplicating router logic in delegate_task: drift |
| F2-D10 | 32k bound respected | Inherited knowledge rides `SessionContext.knowledge` (prompt assembly, `zen_agent.rs:1046`), **not** `task.prompt` — `validate_task`'s `BOUNDED_PROMPT_CHARS` check (`delegate_task.rs:268`) is untouched; the token estimate at `:376` additively includes `knowledge_chars/4` so reservations stay honest | Appending to prompt string: would consume the 32k bound and break the gate's semantics |
<!-- table not formatted: invalid structure -->

### Config keys

| Key                                           | Default                 | Env                                   | Gate semantics                                                                       |
| --------------------------------------------- | ----------------------- | ------------------------------------- | ------------------------------------------------------------------------------------ |
| `[skills] agents_roots`                         | `true`                    | `ZEN_SKILLS_AGENTS_ROOTS`               | false ⇒ loader stays single-root (`~/.zen/skills`) exactly as today                    |
| `[agentic.orchestrator] knowledge_inject`       | `false`                   | `ZEN_ORCHESTRATOR_KNOWLEDGE_INJECT`     | **absent = CLOSED**; no retrieval, no injection, zero latency delta                      |
| `[agentic.orchestrator] knowledge_budget_chars` | `4000` (precedent)        | `ZEN_ORCHESTRATOR_KNOWLEDGE_BUDGET`     | clamp 512..=16000                                                                    |
| `[agentic.orchestrator] knowledge_limit`        | `3`                       | `ZEN_ORCHESTRATOR_KNOWLEDGE_LIMIT`      | clamp 1..=`M1_TOP_K`(5)                                                                |
| `[agentic.orchestrator] knowledge_timeout_ms`   | `2000` **(calibration debt)** | `ZEN_ORCHESTRATOR_KNOWLEDGE_TIMEOUT_MS` | hang guard; timeout → skip + warn                                                    |
| `[agentic.delegate] context_inheritance`        | `false`                   | `ZEN_DELEGATE_CONTEXT_INHERITANCE`      | absent = CLOSED; governs both knowledge block and skill-hit injection for sub-agents |

(`[agentic.orchestrator]` already exists — `surface` key, T378.)

### Wiring points

1. `crates/zen-agents/src/skill_loader.rs:55-68` — `roots: Vec<PathBuf>`, `new_multi_root(paths, workspace_root)`, `list_skills :75` merge first-wins+warn, `load_skill :106`/`skill_exists :120` probe in order; `new_from_dir` preserved.
2. `crates/zen-agents/src/orchestrator.rs:2029` — `SkillLoader::new(&paths)` → `new_multi_root(&paths, std::env::current_dir())` behind `agents_roots`; extract free fn from `:2020-2059` (F2-D9).
3. `crates/zen-agents/src/orchestrator.rs:847-860` & `:1601-1614` — `inject_knowledge_context(&self, paths, session, user_query, &intent).await` at both entry points; lazy `OnceLock<(SearchService, SqliteClient)>` field on `AgentOrchestrator` (`:63-70`).
4. `crates/zen-agents/src/delegate_task.rs` — `SharedKnowledge` field (mirroring `sensitivity` `:279-284`); `run_single :363-366` populates sub `SessionContext.knowledge` (depth-halved) + calls `inject_skill_hits_into`; estimate `:376` += knowledge chars/4; `ensure_delegate_tool` wiring for the new field.
5. `crates/zen-agents/src/orchestrator.rs` — after F2c injection, publish snapshot into `SharedKnowledge` (same place sensitivity is propagated, `:901`).
6. `crates/zen-cli/src/cmd/wiki_command.rs:56/:275/:472` — `--target` flag; `agents` → `~/.agents/skills/zen-wiki/SKILL.md` (add `ZenPaths::agents_skills()` in `paths.rs` next to `skills() :118`).
7. `crates/zen-core/src/config.rs` — 6 keys above (merge + env + toml sample).
8. Audit: extend `loop.delegate.gates` per-task entries (`delegate_task.rs:297-310`) additively: `inherited_notes`, `inherited_chars`, `skill_hits`; one `loop.knowledge.injected` line per injecting turn `{count, chars, tier:2, ms, skipped_reason?}` (flood discipline: emit only when count>0 or skipped-with-error).

### Task breakdown

| ID       | Task                                                                                                | Deps      | Files (serialization groups)                                                            |
| -------- | --------------------------------------------------------------------------------------------------- | --------- | --------------------------------------------------------------------------------------- |
| T210     | config keys (F2) + env + merge + sentinel tests                                                     | —         | G1 zen-core (serialize with T200)                                                       |
| T211 [P] | SkillLoader multi-root + collision warn + walk-to-repo-root                                         | T210      | G7 skill_loader.rs                                                                      |
| T212     | export-skill `--target agents` + `paths.agents_skills()`                                                | T211      | G5 zen-cli + G8 zen-core paths (coordinate: paths.rs also touched by nothing else here) |
| T213     | orchestrator `inject_knowledge_context` (both entry points, lazy stack, timeout, sanitizer)           | T210      | **G6 orchestrator.rs** (serialize with T207, T214, T215)                                    |
| T214     | `inject_skill_hits_into` extraction + sub-agent skill hits                                            | T211,T213 | G6 + G9 delegate_task.rs                                                                |
| T215     | `SharedKnowledge` inheritance in `run_single` + estimate fix + gates-audit fields                       | T213,T214 | G9 (serialize with T220/T221)                                                           |
| T216 [P] | `loop.knowledge.injected` audit + discover-report visibility (reuse `orchestration` aggregator pattern) | T213      | G5 zen-cli / G10 orchestration_stats.rs                                                 |

### Test matrix

- Loader: 3-root precedence (zen > project-nearest > user), first-wins+warn on collision, repo-root walk stops at `.git`, malformed SKILL.md skipped with warn, `agents_roots=false` → byte-identical single-root behavior (gate-off pin), `skill_export_discovery.rs` still green.
- Injection: gate off → `session.knowledge` untouched (pin old behavior); Conversation category → skipped; non-empty pre-populated knowledge (TUI path) → no second retrieval; budget truncation at char boundary; timeout → skip+warn, turn proceeds; sanitized content; `M1_TOP_K` truncation preserved (`orchestrator.rs:2051` pattern).
- Sub-agent: inheritance on → sub `SessionContext.knowledge` = depth-halved snapshot; off → empty (today's behavior); 32k `validate_task` unaffected by inheritance (knowledge not in prompt); reservation estimate includes knowledge chars; gates-audit additive fields present, old fields unchanged.
- Export: `--target agents` writes `~/.agents/skills/zen-wiki/SKILL.md` (tempdir root), `--output` precedence, default unchanged.

### Observable acceptance

- Default config: zero behavior change (all gates closed) — pinned by tests.
- With gates on: `loop.knowledge.injected` lines appear; `loop.delegate.gates` entries carry `inherited_*`/`skill_hits`; `zen wiki export-skill --target agents --json` prints the agents-plane path; a Codex/pi-style agent listing `~/.agents/skills` sees `zen-wiki`.

---

## FEATURE 3 — multi-agent chain hardening (extend 006, minimal)

### Design decisions

| #     | Decision                        | Choice                                                                                                                                                                                                                                                                                                                                                                                                                   | Rejected alternative                                                                                                                                       |
| ----- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| F3-D1 | Context-propagation contract    | Small `SubAgentContext { knowledge: Vec<RetrievedNote>, skill_hits: Vec<String>, budget_chars: usize }` + one composition fn `compose_sub_session(req, depth, shared) -> SessionContext` inside `delegate_task.rs`, used by `run_single` — and therefore **automatically by plan.execute** (shared `Arc<DelegateTaskTool>`, `plan_task.rs:62/:510`) and by resume (recomposition flows through the same `run_single` call, `plan_task.rs:9`) | Prompt-string concatenation only: loses sensitivity tagging, reward bookkeeping (`orchestrator.rs:878` reads `session.knowledge`), and pressures the 32k bound |
| F3-D2 | Fan-out ≤8 spec drift           | **Verified: no hard cap exists** (`parse_requests` `:180-228` unbounded; only advisory `≤4` consumer gate `:237`). Fix: hard-reject `tasks.len() > 8` in `parse_requests` with a structured error — 8 is the *recorded* spec value (AGENTS.md tool-inventory row), so enforcing it is reuse, not invention                                                                                                                                 | Documenting down to ≤4: contradicts the recorded spec                                                                                                      |
| F3-D3 | Checkpoint staleness            | **Report-only** (minimal): resumed-plan audit gains `checkpoint_age_secs` per replayed task (data already in `workflow_tasks`; no schema change). Risk noted: replayed ok-checkpoint outputs are injected downstream as 4000-char snippets (`plan_task.rs:256/:276`) regardless of age                                                                                                                                             | TTL invalidation of checkpoints: redesign, needs a recorded staleness threshold (invented)                                                                 |
| F3-D4 | Reservation refunds             | **Verified sound** — early-error path (`delegate_task.rs:458-460`) drops the reservation; `Drop` refunds (`rig-compose budget.rs:253-259`). Action: add a regression test pinning refund-on-early-error, no code change                                                                                                                                                                                                            | —                                                                                                                                                          |
| F3-D5 | join_all slot ordering          | **Verified correct** (`:606-643`, test `:1094`); no action beyond citing in the contract doc-comment of `compose_sub_session`                                                                                                                                                                                                                                                                                                      | —                                                                                                                                                          |
| F3-D6 | Resume + injection (question d) | **Yes, automatically**: resume re-runs pending tasks through `run_single` (`plan_task.rs:506-542`), so the F2-D8 seam inside `run_single` covers resumed tasks with no extra wiring; replayed (skipped) tasks keep their recorded outputs verbatim — correct, they ran under the context of their time                                                                                                                             | Separate injection at the plan layer: double seam, drift risk                                                                                              |

### Config keys

None new (F3 rides F2's `[agentic.delegate] context_inheritance`). F3-D2's cap is a recorded-spec constant, not config.

### Wiring points

1. `crates/zen-agents/src/delegate_task.rs:180-228` — batch cap 8 in `parse_requests`.
2. `crates/zen-agents/src/delegate_task.rs:335-493` — `compose_sub_session` seam (refactor of `:363-372`), contract doc-comment.
3. `crates/zen-agents/src/plan_task.rs:500-542` — audit line for resumed replays gains `checkpoint_age_secs` (read from existing checkpoint row).
4. `crates/zen-agents/src/delegate_task.rs:297-310` — (shared with T215) additive gates fields.

### Task breakdown

| ID       | Task                                                                                          | Deps | Files                                         |
| -------- | --------------------------------------------------------------------------------------------- | ---- | --------------------------------------------- |
| T220     | Hard cap 8 in `parse_requests` + structured error + test                                        | —    | G9 delegate_task.rs (serialize with T215)     |
| T221     | `SubAgentContext`/`compose_sub_session` seam + contract docs (absorbs T215's SessionContext work) | T215 | G9                                            |
| T222 [P] | Reservation refund-on-early-error regression test                                             | —    | G9 tests (same crate; cargo-owner serializes) |
| T223 [P] | `checkpoint_age_secs` on resume audit                                                           | —    | G11 plan_task.rs                              |

### Test matrix

- Cap: 9-task batch → structured rejection naming the cap; 8 → accepted; mixed reject/run slot ordering preserved (`:967` test extended).
- Seam: `compose_sub_session` with inheritance off → today's empty-context output (byte-identical pin); on → snapshot + depth halving (shared with T215 tests).
- Refund: early LLM-round failure → budget `tokens_consumed` unchanged after reservation drop (F3-D4 pin).
- Resume: replayed checkpoint emits `checkpoint_age_secs`; re-run tasks get injection (assert sub-session knowledge non-empty when gate on).

---

## Cross-feature sequencing & commit granularity (one workstream = one commit)

```
C1  T200+T210        config surface (both features' keys)      [G1 zen-core]      — one commit: "config: memory_strength + knowledge-inject + delegate-inheritance keys (gates closed)"
C2  T201             strength.rs pure module + tests           [G2]               — "memory: retention-strength pure model (Ebbinghaus, belief.rs anchor discipline)"
C3  T202+T203        indexer metadata + strength-aware cards   [G2]               — depends on the OTHER agent's memvid_index re-root landing first (see OQ-4)
C4  T211             SkillLoader multi-root                    [G7]               — ".agents/skills discovery plane (codex/pi convention, first-wins+warn)"
C5  T212             export-skill --target agents              [G5,G8]
C6  T213+T216        orchestrator knowledge injection + audit  [G6,G10]
C7  T214+T215+T221   sub-agent inheritance + seam              [G6,G9]            — largest; touches orchestrator.rs AND delegate_task.rs (single cargo owner)
C8  T220+T222        fan-out cap + refund regression           [G9]
C9  T223             resume checkpoint-age audit               [G11]
C10 T204             gateway memory/search rerank              [G3]
C11 T205+T206        nightly strength report + discover section[G4,G5]
C12 T207             reward increments for memvid injections   [G6]               — last, after C6/C7 settle orchestrator.rs
```

Ordering rationale: pure/config first (no behavior), single-file crates parallelizable ([P] markers), `orchestrator.rs` (G6) and `delegate_task.rs` (G9) are the two hot files — C6→C7→C12 strictly serialized. C10/C11 are independent tail work. Feature 3 (C8/C9) can interleave anywhere after C1.

## Open questions for the OWNER (≤5, with recommended defaults)

1. **Skill root precedence** — zen-native first (protects the pinned `zen-wiki` export discovery) vs user `~/.agents/skills` first (lets operators override zen's export)? **Recommend: zen-native first + warn** (contract-test stability; overrides remain possible via `--output`/frontmatter `auto_route:false`).
2. **Knowledge-inject default** — ship `false` (calibration-debt discipline, T138 says don't flip interactive defaults without measured deficit) or `true` for gateway/qqbot surfaces only (which today get *zero* KB context)? **Recommend: ship false; add a shadow-measurement audit line for 2 weeks, then decide with data.**
3. **Strength rerank scope** — apply to both gateway `memory/search` and `select_cards`, or gateway only? **Recommend: both behind the one flag** (single semantic for "strength affects retrieval ordering").
4. **Sequencing vs the memvid re-root agent** — C3 edits `memvid_index.rs` while another agent owns it. **Recommend: land C3 strictly after their PR merges; everything else is independent of the re-root.**
5. **Fan-out cap 8** — enforce the recorded spec value (behavior change for >8 batches, which the advisory ≤4 gate already discourages) or re-document the cap away? **Recommend: enforce 8** (spec is the recorded value; structured rejection is model-readable).

## Risks

- **Per-turn latency**: F2c adds one FTS5 search per gated turn on non-TUI surfaces — bounded by `knowledge_timeout_ms` (fail-open skip); TUI path unaffected (empty-knowledge precondition). Measure via `loop.knowledge.injected.ms` before any default flip.
- **Token-budget regression**: +4000 chars ≈ +1000 tokens/turn at M1; sub-agent reservations already estimate `prompt/4 + 1024` (`delegate_task.rs:376`) — T215's additive knowledge term keeps `try_reserve_tokens` honest; depth-halving bounds fan-out blow-up (≤8 subs × ≤2000 chars at depth 1).
- **Migration hazards**: none — zero migrations in all three features (deliberate; strength is derived, checkpoints reuse `workflow_tasks`).
- **Single-writer conflicts**: no new markdown/DB writers; reward sidecars keep their one flock-serialized writer path (new call sites are same-crate, same discipline); `memory-strength-report.json` written only by the daemon-exclusive nightly worker via `write_atomic`.
- **Dormant-gate accumulation**: three new default-off gates (`memory_strength.enabled`, `knowledge_inject`, `context_inheritance`) + `agents_roots` default-on. Mitigation: each ships with a gate-off byte-identity test AND an audit-line emitter so "on but never fires" is observable; add all four to the spec Configuration Surface in the same commits (rule 6); put `search_rerank` and `knowledge_timeout_ms` on the calibration-debt ledger with named triggers (arena corpus / measured p95).
- **memvid append-only growth**: F1-D2 metadata is write-time only; no re-put path exists, so no frame duplication risk — but legacy frames permanently lack `source_path` (strength falls back to uri/created_at); a full `index_all` refresh (nightly) converges new content naturally.