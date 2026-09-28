# TODOS

## Review

### QQBot outbox drainer (morning-brief push completion)

**What:** Gateway-side task draining `logs/outbox/morning-brief-*.json` via qqbot active send (`msg_id=None`), deleting each file on successful send, leaving failures for retry with warn log.

**Why:** Completes FR-038's push face; without it the 9am brief never reaches chat — the outbox is currently write-only.

**Pros:** True proactive push; reuses `QqBotApi::send_group/c2c_message`; fail-soft seam already tested producer-side.

**Cons:** New gateway runtime surface (poll interval, recipient resolution from `chat_hint`, retry/backoff policy); touches the daemon tick.

**Context:** Producer done 2026-09-03 (005-agentic-loop T079): `MorningBriefWorker` stages `{date, lines, chat_hint}` JSON; seam documented in `crates/zen-agents/src/scheduler/workers/morning_brief.rs`. Active-send API at `crates/zen-gateway/src/channel/qqbot/api.rs:46,61`. Constraint: zen-agents must not depend on zen-gateway — drainer lives gateway-side. Start by reading the qqbot adapter's active-send fallback (`adapter.rs:~528`).

**Effort:** M
**Priority:** P1
**Depends on:** 005-agentic-loop T079 (done, `morning-brief` worker + outbox contract) — **IMPLEMENTED per 005-agentic-loop T101 [X] 2026-09-04: `crates/zen-gateway/src/channel/qqbot/outbox_drainer.rs` (`drain_once` + tick spawn, Public-only fail-closed, never deletes undelivered; recipients via `QqBindingRepo::list_chat_ids`, group-first/C2C-fallback)**

### Harden dispatch hold-transport contract (shutdown_write EOF test)

**What:** Document `shutdown_write` as the only EOF path for detached SPAWNED tasks and add test that `abort + shutdown_write → EOF` while `abort` alone does not hang.

**Why:** Detached `session/turn` handlers hold `Arc<DispatchServer>` + transport; aborting `run()` never closes client — silent hang on cancel, leaks connection.

**Context:** Prior learning `dispatch-spawned-methods-hold-transport` (2026-08-23, 9/10). Fix: keep `tokio::spawn` for responsiveness but make close contract explicit. Files: `crates/zen-gateway/src/server/dispatch.rs:46,188`, `crates/zen-gateway/src/transport/uds.rs:72`, `crates/zen-gateway/src/client/surface.rs:728`. Test location: `crates/zen-gateway/tests/spawn_race.rs` or new `dispatch_shutdown.rs`. Start by reading `DispatchServer::run` and `SPAWNED_METHODS`.

**Effort:** S
**Priority:** P1
**Depends on:** Lane PR1 (gateway-core) — **Aligned to tasks.md T066/T071 (Phase 13.1) — IMPLEMENTED per 004-agentic-gateway tasks.md [X] 2026-08-25**

### Turn-affinity approval fix (thread turn_id through callback)

**What:** Thread `turn_id` / `ConnectionHandle` affinity through sandbox `ApprovalCallback` so `ApprovalBroker::decide` binds to originating turn, not first unclaimed route.

**Why:** Current `decide()` scans for first `!claimed` — concurrent turns misroute approvals, surface B can approve surface A's `shell.exec`, violating SC-007 (privilege escalation, P0).

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

### Generated single-ledger for closure records (structural option, deferred)

**What:** Replace hand-mirrored closure history across spec.md record sections + tasks.md + AGENTS.md Recent Changes with tasks.md as the single source and the other two generated/pointing at it.

**Why:** The closure-status convention (spec.md, 2026-09-20) adds sync discipline but not dedup — each closure must be hand-written correctly in 3+ places or the docs rot (Phase 27 found an entire concurrency workstream unrecorded; T181 found the spec documenting deleted primitives).

**Pros:** Eliminates the write-amplification root cause; one place to audit.
**Cons:** Speckit tooling (specify/converge/implement) reads/writes spec.md+tasks.md directly and would need adapting; AGENTS.md is hand-curated for agents, not generated.
**Context:** Accepted as D2-A (keep convention, record limitation) in the 2026-09-20 eng review after outside-voice tension #8. Trigger: the next verdict-rot incident OR speckit tooling gaining generation support. Start from spec.md's closure-convention blockquote.
**Effort:** M. **Priority:** P3. **Depends on:** speckit tooling evolution; not before.

### LLM cost reporting is zero everywhere — `llm_cost_cap_usd` cap cannot trip

**What:** Convert worker LLM usage into cost (usage × `ModelMetadata.input_cost_per_million`) and report `llm_cost_usd` from workers; today every worker hardcodes 0.0 (dream.rs:153, session_journaler.rs et al.), so the scheduler's cap check (scheduler/mod.rs:338-346) is live-but-inert.

**Why:** The config key is documented as functional in spec.md's Configuration Surface; a cap that cannot trip is documented-as-live phantom enforcement (outside-voice #2, 2026-09-20).

**Pros:** The `[cron] llm_cost_cap_usd = 10.0` knob becomes real; runaway local/cloud spend gets a ceiling.
**Cons:** Needs usage plumbing per worker call; local Ollama is free (cost 0 is *correct* for local) — only metered providers need it.
**Context:** Spec annotation landed same day (config table marks it inert-until). Start at scheduler/mod.rs cap check + model_meta.rs pricing fields.
**Effort:** S-M. **Priority:** P2. **Depends on:** none.

**Completed:** 2026-09-20 (/speckit-implement batch 3): pricing config keys + metered completion path + WorkerCostLedger sidecar + cap-trip test. Remaining accuracy follow-up: provider usage tokens (currently bytes/4 estimate).

## Review (2026-09-27 /plan-eng-review — docs/src user guide)

### User-guide consistency check in CI (docs-rot alarm)

**What:** A test (or script wired into `.github/workflows/docs.yml`) asserting `docs/src/cli-commands.md` command tables match the live clap surface (renderable from the `Commands` enum in `crates/zen-cli/src/cli.rs` or `zen --help` output), and spot-asserting documented config keys exist as serde fields on the `zen-core` config structs. Fail CI on drift.

**Why:** The user guide went stale three times — 29→20→23 command count, removed `note`/`search` commands still taught, phantom `llm_preferences`/`max_sensitivity` keys documented that never parsed in any revision. Every drift was silent: users got wrong instructions with no error anywhere. The 2026-09-27 audit (`8c50f62`) fixed the backlog by hand; nothing prevents the next one.

**Pros:** Turns silent doc rot into a red CI job; gates the highest-traffic factual claims (command surface, config keys) with near-zero maintenance once written.

**Cons:** Deliberate guide restructuring requires updating the test; covers tables/keys only, not prose claims.

**Context:** Narrower sibling of the deferred single-ledger TODO above (D2-A covers spec closure records; this gates the user guide's factual tables). Start from the `Commands` enum variants (names render deterministically) and `ProviderConfig`/`AgentConfig`/`Agentic*Config` serde fields in `crates/zen-core/src/config.rs`. Trigger: any future CLI/config surface change lands.
**Effort:** S-M. **Priority:** P2. **Depends on:** none.

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

### ⑥ scope note — PDF parsing (from same review)

`ingest_local_file` is plain `read_to_string` (crates/zen-vault/src/ingest/web.rs:42-44) — no PDF capability. Tree-index workstream ⑥ first phase covers markdown/long-text segmentation only; PDF parsing requires a new dependency (audit per Constitution XI before adding).
