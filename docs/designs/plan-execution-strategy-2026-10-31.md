# Design Record: Plan as Execution Strategy

**Date**: 2026-10-31 · **Feature**: 006-agentic-orchestration (D24, plan W9)
**Status**: IMPLEMENTED (W6 durable plan ledger, 2026-11; `plan.todos`/`plan.execute`/`plan.control` on `workflow_plans`/`workflow_tasks`). Status pointer synced 2026-11-14 (W11.7/T7); hardening fixes in `orchestration-sdd-review-fixes-2026-11-14.md`
**Trigger**: `/plan-ceo-review` SCOPE EXPANSION pass over W5-W8

## Problem

The orchestration plan specified a per-turn todo ledger (`TodoLedger`, in-memory, "no
table, does not survive the turn") while `workflow_plans` / `workflow_tasks`
(migrations 006/007) already persist per-task status with claim-fenced resume
(`WorkflowRepo::claim_plan`, `load_task_rows`). Two state machines for one concept.

## Decision

1. **`plan.todos` reads and writes `workflow_tasks` directly.** Item titles come from the
   plan's `spec_json`. No migration, no second ledger, no `TodoLedger` struct.
2. **`plan.todos` is read-only for the quality gate.** Mutating classification would fire
   Metis→Momus→Hermes on every checklist tick; with no deliverable to judge that
   reproduces the structural veto fixed on 2026-10-30. Precedent: `shell.collect` is
   already whitelisted at `orchestrator.rs:839`.
3. **Lifecycle gets a terminal state.** `cancelled` (new value on the existing
   free-form `status` column) is written by turn-cancel, daemon drain and SIGTERM;
   in-flight tasks become `failed`, pending become `skipped`. A mark-only boot sweep
   closes ownerless `running` rows older than 2× the turn watchdog.
4. **Retention + sensitivity.** 90-day `AgeDelete` on both tables by `updated_at`
   (`running` excluded); session `Sensitivity` recorded in `spec_json`.
5. **No CLI surface.** Owner directive: a plan is an agent scheduling/execution strategy,
   not a product surface. The `zen plan list|show|cancel|resume` family was withdrawn.
   Control lives on the `plan.control {op: cancel|append}` tool.
6. **No in-flight task edit.** A running sub-turn already consumed its prompt; "editing"
   it can only mean abort-and-rerun, which may repeat side effects (half-written files).
   Undefined semantics, deliberately out of scope.

## Evidence (verified 2026-10-31)

| Claim | Source |
|---|---|
| Plan state already persisted with resume | `crates/zen-repo/migrations/006_workflow_plans.sql`, `007_plan_owner.sql` |
| Resume is claim-fenced | `workflow_repo.rs:95-111` (`claim_plan`), `:152` (`load_task_rows`) |
| Status values are free-form `TEXT` | `006_workflow_plans.sql` (`status TEXT`) |
| `is_mutating_tool` is a read-only whitelist | `orchestrator.rs:822-841` |
| `shell.collect` precedent | `orchestrator.rs:839` |
| `LlmConfig` is legacy routing | `config.rs:394-409` (`"Legacy per-task LLM routing entry"`) |
| No `[llm]` section exists | `config/config.toml` (grep: no match) |
| TUI already renders markdown tables | `tui/markdown.rs:313,343,392-399` |
| Morning brief stages to outbox | `scheduler/workers/morning_brief.rs:42` |

## Rejected alternatives

| Alternative | Why rejected |
|---|---|
| In-turn `TodoLedger` (the original W6.2 design) | Second state machine beside a durable one; guarantees drift; blocks every cross-turn capability. |
| `zen plan` CLI family | Owner directive. Also the largest docs-rot surface in a cycle themed on simplification. |
| Full mid-flight steering (edit in-flight tasks) | No clean semantics; abort-and-rerun repeats side effects. |
| Autonomous retry on failure | Contradicts "tell the user before spending"; can pay twice for the same hole. |
| Per-plan cost ceiling instead of a forecast | Requires evidence that estimates are accurate enough to gate on; forecast first, gate later. |
| Backing up the ledger with a JSON snapshot at turn end | Still two sources of truth; cannot feed `claim_plan` resume. |

## Open risks carried into implementation

- **F2** `[llm] request_timeout_secs` would land in a table documented as legacy. Decide:
  new dedicated section, or the existing `[gateway]` timeout family.
- **F3** A 12-task / 3-layer plan can outlive the 900 s turn watchdog and be cancelled
  mid-flight, orphaning background sub-turns. Needs a per-wave heartbeat with a
  plan-level ceiling.
- **F4** The synthesis pass re-renders an answer the user already saw on the stream path.
  Skip it when the in-loop answer is already substantive.
- **F5** `wisdom/plans/<slug>.md` has no named reader. Give it one (brief "done" line, or
  `plan.todos get` provenance) or cut the page.
