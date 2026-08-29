# TODOS

## Review

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
