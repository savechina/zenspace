# Orchestration SDD Review Fixes — TurnContext isolation, control-plane validation, data backfill, drift repairs (2026-11-14)

**Status:** design of record for the W11 remediation wave (spec.md §Post-Review Hardening, D30/D31 + C8). Owner-approved 2026-11-14 via 22 interactive decisions (D1–D22 in the review report). **IMPLEMENTED 2026-11-14** (W11.1–W11.16 code + pins landed; see tasks.md Wave W11 ledger for per-row evidence). Gates green: `bin/lint` 0; `bin/test` 3774/3775 (the 1 failure is the sandbox NO_COLOR/TERM=dumb TUI-color env, passes with a color terminal; zen-cli untouched). T9 synthesis runtime skips pinned ×4 (budget / no_provider-never-cloud / empty / timeout_or_error) via a test-controllable MockStreamMode (default Reply byte-identical). Trigger: owner directive "use sdd review the current agent orchestration logic and workflow, plan task name conversation", then "update review found issue and task to spec and design and task".

**Constitution XI note:** every fix reuses an existing zen pattern (task-locals, structured-Ok errors, atomic mint, spawn_blocking discipline, forward-only migrations). Nothing here invents a new primitive. Zero new config keys (D22), zero new CLI/protocol surface (D23/D24).

## 1. Review provenance

| Input | Result |
|---|---|
| `/plan-eng-review` 4-section interactive review (SDD cross-artifact: spec ↔ tasks ↔ code ↔ tests ↔ existing data) | 8 findings, all owner-decided |
| 3 parallel explore sweeps (impl map / spec inventory / test map), working-tree state (W5–W9 uncommitted, 62 files +8095/−1067) | verbatim anchors for every finding; coverage ~40/46 paths (87%) |
| Outside voice: codex exec timed out (5 min) → oracle subagent fallback (same model family, fresh context) | 6 NEW findings, all verified verbatim before adoption |
| Full report | `docs/specs/006-agentic-orchestration/review-2026-11-14-sdd-orchestration.md` (decision ledger D1–D22, failure-mode matrix, coverage diagram) |

## 2. Findings → adopted fixes

### A1. TurnContext isolation (spec D30; F1 + OV-1, P1) — decisions D2 + D17

**Evidence.** One daemon-wide orchestrator (daemon.rs:952-964) serves all hosted sessions; sessions overlap (Parallel Execution Contract §4). `propagate_sensitivity/knowledge/history` (orchestrator.rs:1659-1712) write last-writer-wins cells at turn entry; readers compose/dispatch later: `compose_sub_session*` (delegate_task.rs:867-889/948-952), the confidentiality hook cloud-tool gate (confidentiality_hook.rs:200-205 ← wiring.rs:773 ← orchestrator.rs:2574/:3476), the plan quality gate (plan_task.rs:1383). A concurrent Public session overwrites a Confidential session's context ⇒ cloud routing leak + cross-session knowledge/history injection.

**Fix contract.** One `TurnContext {sensitivity, knowledge, history}` task-local in **zen-core** (zen-plugin hooks must read it); scoped around execute/execute_stream; all turn-path consumers migrate; background admission captures synchronously and scopes around the spawned future (`BACKGROUND_TASK.scope` precedent — task-locals survive `block_in_place`/`join_all`, NOT `tokio::spawn`); cells demoted to documented non-turn fallback (scheduler workers), deleted only after full migration (trigger-based cleanup). Single-session behavior byte-identical.

### A2. Control-plane input validation (spec D31; F2, P1) — decision D3

**Evidence.** `session/start` accepts any client string (hosting.rs:924-927) → `sessions().join(session_id)` traversal read/write (conversation.rs:42-48, hosting.rs:799-806/825-828) bypassing the 5-hook tool gates; `latest_plan_for_session` unescaped LIKE (workflow_repo.rs:383-390) — a `%` id matches every session's plan.

**Fix contract.** Charset `[A-Za-z0-9_-]` ≤128 at session/start ⇒ `-32602` (frozen catalog, no new code); exact plan-marker match (json_extract or LIKE-escape); ConversationStore traversal rejection as defense-in-depth. UUIDv7/"local"/qqbot ids byte-identical.

### A3. Data backfill for the vocabulary rename (OV-2, P2) — decision D16

**Evidence.** HEAD wrote `status:"ok"` (git show HEAD:plan_task.rs:605); the working tree replays only `"completed"` (:922). No backfill ⇒ resume of any pre-upgrade plan re-runs every completed task (double spend + repeated side effects). The SDD blind spot: code-vs-spec passed; code-vs-existing-data was unchecked.

**Fix contract.** Migration 015: `UPDATE workflow_tasks SET status='completed' WHERE status='ok'` (forward-only data backfill, XIII-compliant) + migration test + legacy-fixture resume pin. **Ships in the first release running the new vocabulary against old DBs.** New standing rule (spec D21 §2(d)): vocabulary changes check code-vs-EXISTING-DATA, not only code-vs-spec.

### A4. Unified live staleness (SC-017 amended; F3, P2) — decision D4

**Evidence.** Sweep resolves `2 × watchdog_deadline(false)` live (daemon.rs:504); the resume fence passes fixed `DEFAULT_STALE_CLAIM_SECS=1800` (plan_task.rs:655/901/977) — the repo doc (workflow_repo.rs:23-27) promises callers resolve live; none do. Widened watchdog (clamp 14400) ⇒ mid-run claim steal; shrunk watchdog ⇒ sweep cancels live plans.

**Fix contract.** One resolver in zen-core config (2× resolved `[gateway] turn_watchdog_secs` incl. env layer) called by sweep + every claim site + morning-brief display; the constant is the non-config fallback only.

### A5. Gate parity + ledger repair (F4, P2) — decision D5

**Evidence.** D25 A6.2 + G-27 assigned "plan gate via review_pipeline(router)" to W6.3; W6.3 acceptance listed only A6.1/A6.3; code still `QualityPipeline::new()` (orchestrator.rs:1618 vs :1476/:1505). Confidential plan deliverables skip the semantic-review stage.

**Fix contract.** One-line parity + direct unit pin; W6.3 acceptance text repaired (landed in tasks.md this pass).

### A6. Plan-task fork/model surface (SC-021/022; F5, P2) — decision D6

**Evidence.** Spec says "delegate or plan task"; plan_task.rs:1134-1138 hardcodes `None` with a comment claiming delegate-only scope the spec never granted. SC-022 "may run a hint" overpromises: runtime override deferred to D2b (honestly recorded in W5.2 closure).

**Fix contract.** PlanTaskSpec/schema/parse accept `fork_mode`/`model`; pass through the existing `compose_sub_session_forked` + `validate_model_hints` (≤5 distinct/batch); SC-022 amended to "may carry" + delivered validate/label scope (landed in spec.md this pass). Runtime override stays D2b — not reversed by this wave.

### A7. Mint integrity (F6 + OV-3, P2) — decisions D7 + D19

**Evidence.** `mint_fresh_plan` does create+claim as two writer calls (ownerless window vs SC-017 claim-at-create) and discards both results (`let _ =`, plan_task.rs:647-657) ⇒ phantom plan runs: orphaned task rows, unpersisted "success", impossible resume, silent — the review's only critical failure-mode gap. `latest_plan_for_session` tiebreak: second-granularity created_at + random UUID (workflow_repo.rs:393).

**Fix contract.** Route through the atomic mint; failures ⇒ structured rejection (`plan_todos::open_client` precedent); `ORDER BY created_at DESC, rowid DESC`.

### A8. Synthesis off-runtime resolve (OV-4, P2) — decision D18

**Evidence.** orchestrator.rs:2165-2169 resolves the provider inline on the async worker, contradicting its own hazard comment (:2176-2177) and duplicating the spawn_blocking resolve (:2180). D1-class daemon stall under slow/unreachable ollama.

**Fix contract.** Delete the inline resolve; spawn_blocking first; derive provider_label/is_local/secs/skip from its single result. Lands with the G2 runtime-skip pins.

### A9. Accounting + query hygiene (OV-6, F8, P3) — decisions D20 + D15

- **Admission reserve** (delegate_task.rs:1110 before compose :1145): compose first, estimate += inherited_chars/4 (SC-021 additive clause), `sweep_terminal` at the round-boundary notice drain (expired holds release promptly). D20 §8 refund contract unchanged. Single-reservation redesign **deferred to TODOS.md** (trigger: measured budget-refusal incidents or background-heavy usage).
- **Forecast query** (workflow_repo.rs:485-492): `ORDER BY updated_at DESC LIMIT 500` code constant; recency improves the estimate; SC-018 omit-when-no-history untouched.

### A10. Doc sync + SC-007 honesty (F7 + OV-5, P3) — decisions D8 + D21

Spec/tasks side **landed this pass**: C8 evidence record, SC-007 amended to at-least-once with consume-once cursor (implementation right, commit 6cfbc29; criterion stale), SC-017/SC-022 amendments, C1 3600s annotation, D21 §2(d), W5–W9 header corrections, G-21 annotation, W6.3 acceptance repair. Remaining (W11.7): AGENTS.md staleness row (rides A4) + design-doc status pointers.

### Test pins (G1–G6) — decisions D9–D14

Background refund after collect/kill/expiry (G1) · synthesis runtime skips ×4 incl. never-cloud (G2) · fork knowledge positive inheritance + invalid string (G3) · plan.todos sensitivity mirror under concurrency (G4, rides A1) · real-task expiry (G5) · LIVE_PLANS cancel-arrival join semantics (G6).

## 3. Rejected (do not re-propose without new evidence)

| Candidate | Decision | Reason |
|---|---|---|
| Per-session orchestrator instances | D2-B | Contradicts the recorded shared-orchestrator architecture (process-wide wiring, lazy MCP, knowledge_stack OnceCell); week-scale refactor for a race the task-local pattern closes |
| Exactly-once notices (mark consumed on injection) | D21-B | Reverses shipped reliability fix 6cfbc29 — a failed round would swallow the notice; losing notices is worse than repeating them |
| Single armed reservation moved into the future | D20-B | Redesigns the D20 §8 refund contract inside a fix wave; deferred to TODOS.md with trigger |
| Compat read `status IN ('ok','completed')` instead of backfill | D16-B | Two vocabularies live forever — the drift class D21 §2's literal-rename decision rejected |
| Full D2b runtime model override in this wave | D6-C | Owner-recorded deferral; per-provider plumbing is its own wave |
| Accept-and-document for any P1/P2 | D2-C/D3-C/etc. | Ships known leaks/correctness bugs; completeness is minutes of CC time |

## 4. Wave hooks (tasks.md W11)

| Lane | Rows | Modules |
|---|---|---|
| A (merge first — security) | W11.1 → W11.4 → W11.16 → W11.8 ∥ W11.10 | zen-agents orchestrator/delegate/wiring, zen-core TurnContext, zen-plugin hooks |
| B | W11.5 → W11.6 → W11.15 + W11.9 → W11.12 → W11.3 | plan_task/plan_control/plan_synthesis, zen-core staleness, daemon sweep line, morning_brief |
| C | W11.2 → W11.14 → W11.13 → W11.11 → W11.7 | gateway hosting, zen-memory, zen-repo, docs |

Gates: `bin/lint` 0 + `bin/test` green after each merge; `bin/verify` (Verus + DST + congruence) at the end. W11.14 rides the first release running the new vocabulary against old DBs. Conflict flag: zen-core lib.rs (A: TurnContext module, B: staleness resolver — different files, trivial merge).

## 5. Verification pins this design owes

1. Per-consumer concurrent-session race pins (Confidential + Public overlap ⇒ no cloud route, no foreign knowledge/history) — A1.
2. Traversal/wildcard/oversize sessionId rejection + legacy-id plan isolation — A2.
3. Legacy-'ok'-fixture resume replays (migration 015 test + resume pin) — A3.
4. Widened-watchdog fence==sweep equality — A4.
5. Plan gate semantic-reviewer parity unit pin — A5.
6. Plan-task fork inheritance content + 6th-hint rejection + absent byte-identity — A6.
7. Forced mint-failure structured rejection, zero orphan rows; same-second tiebreak determinism — A7.
8. Synthesis runtime skip pins ×4 (no_provider never cloud / empty / timeout / budget) — A8+G2.
9. Inherited-chars admission estimate + drain-release; forecast old-rows-excluded — A9.
10. Refund/fork/expiry/join-semantics/sensitivity-mirror pins — G1/G3/G4/G5/G6.

## 6. Source index

- Review report + decision ledger: `docs/specs/006-agentic-orchestration/review-2026-11-14-sdd-orchestration.md` (gitignored local ledger per W9.6; this design doc is the git-tracked record).
- Spec: `docs/specs/006-agentic-orchestration/spec.md` — C8 (evidence), D30/D31 (contracts), SC-007/SC-017/SC-022 amendments, D21 §2(d).
- Tasks: `docs/specs/006-agentic-orchestration/tasks.md` — Wave W11 (W11.1–W11.16 ≡ T1–T17, T11 merged), G-30, Wave-11 execution order, W6.3 acceptance repair.
- JSONL task artifact: `~/.gstack/projects/zenspace/tasks-eng-review-20261114-144731.jsonl` (16 tasks).
- Code anchors: orchestrator.rs, delegate_task.rs, plan_task.rs, plan_todos.rs, plan_control.rs, workflow_repo.rs, hosting.rs, daemon.rs, conversation.rs, confidentiality_hook.rs, approval_hook.rs, task_registry.rs (all line numbers in the review report, verified verbatim 2026-11-14).
- Precedents reused: SESSION_SCOPE/APPROVAL_TURN/BACKGROUND_TASK task-locals · plan_todos open_client structured degradation · mint_plan_with_tasks atomic claim-at-create · spawn_blocking off-runtime discipline (C4/D1-class) · forward-only additive migrations (XIII) · upstream-orchestration-adoption-2026-11-01.md (design-of-record format).
