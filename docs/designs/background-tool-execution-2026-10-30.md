# Background Tool Execution — Design Record (2026-10-30)

Status: Approved (Oracle arbitration `ses_e7297c441ffeADiVGFNieF7hte`) — implementation in progress.

## 1. Requirement

User (verbatim): "review agent plan and tools call issue — tools background run, after tool
output result in local disk, the main task collect the tools result after to next step execute."

Reading: agent tools can run in the background; output persists to local disk; the main
loop is notified and collects results before relying on them for the next step.

## 2. Problem (verified)

- rig-agent 0.42 `AgentRun` requires every pending tool call answered exactly once in a
  single `tool_results()` call before the next model round — no deferred/partial answers.
- zen's round loop awaits `dispatch_timed(...)` (orchestrator.rs:2243 execute / :3077
  stream, `buffered(4)`, order-preserving) with NO dispatch-level timeout: one hung tool
  stalls the whole turn until the gateway 900s watchdog kills it.
- Long-running work (builds, test suites, scrapes) therefore cannot outlive a round;
  `shell.exec` foreground timeout clamp is 1..600s and SIGKILLs the group at expiry.
- No task-id / collect / kill primitives exist anywhere (greenfield on the registry side;
  spawn+validation mechanics in `shell_exec.rs` are reusable).

## 3. Upstream consensus (librarian research)

codex-rs unified_exec (session_id + yield-poll, 1MiB ring, 64-proc cap), Claude Code
(run_in_background → task id + output file, push notification, TaskOutput REMOVED in
favor of Read, auto-background on timeout, kill-on-exit), opencode (background:true →
sh_id + disk file, push synthetic message + auto-resume, "DO NOT poll", SQLite job rows,
7d retention). Agreements: (1) task-id handle returned immediately; (2) output to disk
with bounded preview; (3) push beats poll — newer systems deleted the poll tool; (4)
background = long wall time + hard resource caps + explicit kill. rig needs no changes:
build entirely at the tool layer.

## 4. Decisions (Oracle-arbitrated D1–D8)

- **D1 In-daemon TaskRegistry** (not setsid): daemon is launchd-respawned and restarts are
  deliberate (`zen serve restart` drains). SIGTERM drain kills live background tasks with
  audit lines. Crash-orphan hole closed by a boot-time sweep that is **two-tier (amended
  2026-10-31)**: dead group (`kill(-pgid, 0)` ⇒ ESRCH) ⇒ mark-only; live group verified ours (leader's
  observed process age matches the spawn record within ±10s, via macOS `/bin/ps -o
  etime=` / Linux procfs starttime) ⇒ SIGKILL + mark; live but unverifiable ⇒ mark-only
  (pid-reuse fail-safe invariant, kept).
- **D2 `background: bool` param on `shell.exec`** (default false = byte-identical), not a
  separate tool — one concept one tool; validation gates, sensitivity (Confidential), and
  ToolArgRegistry row all inherited unchanged.
- **D3 No demote-on-overrun**: foreground timeout keeps SIGKILL semantics (existing pins
  encode "foreground = bounded"). Model explicitly chooses `background:true`. Revisit
  trigger: production foreground-timeout-retry pattern.
- **D4 Completion = round-boundary push into `accumulated_results`, PER-SESSION scoped**
  (registry entries carry `session_id`; scan filters to owner — cross-session injection
  is a Confidential leak, security pin required). Cross-turn notification comes free:
  the per-session cursor picks up between-turn completions at the next turn's first
  round boundary. Stream path additionally echoes the notice via the token callback.
  **Delivery is at-least-once (amended 2026-10-31)**: the drain does NOT consume; the
  orchestrator confirms via `mark_notified` only after the model round that carried the
  notices returned Ok — a failed round re-offers the notice next round/turn (advisory
  data tolerates duplicates, never loss).
- **D5 `shell.collect {task_id, wait_ms?}`**: default wait 10s (codex-yield precedent,
  clamp 0..=30000); returns status/exit_code/stdout_tail/stderr_tail (each ≤8k chars,
  char-boundary-safe)/output_path/bytes/truncated; full stdout inline when ≤4k chars
  (saves a round). NO cursor — `fs.read` byte-range (FR-023) already covers incremental.
- **D6 Soft contract, no hard barrier**: notify + prompt teaching + turn-end audit line
  listing live session tasks. A hard "refuse to end turn" barrier contradicts the feature
  (25-min builds must outlive turns). Barriers belong to plan.execute layer semantics
  (phase 2, deferred). Revisit trigger: models systematically never collecting.
  **Gate visibility (amended 2026-10-31)**: the quality-gate input gains a facts-only
  context line when the session has still-live tasks (id8/digest8/age) — the gate
  reviewer sees only `final_response` and could otherwise pass a partial deliverable
  as complete. Zero live tasks ⇒ gate prompt byte-identical.
- **D7 Caps**: 8MiB/stream = **stop-capturing threshold, NOT kill** (drain keeps reading
  and discarding so the child never SIGPIPEs; `truncated:true` = bytes actually DISCARDED
  past the cap, an exactly-cap output is not truncated; `bytes_*` keep counting; on a
  failed capture write the meta stops advancing `captured` — never claims bytes the
  disk does not hold [2026-10-31 review fix]). Deadline `[agentic.background]
  max_wall_secs` default 1800 (clamp 60..=86400) enforced on the **existing 30s GC
  tick** (no new sweeper). `max_concurrent` default 4 (clamp 1..=16, consistent with
  delegate). Worst live disk 4×2×8MiB = 64MiB + 7d retention.
- **D8 Contract-clean with amendments**: registry MUST hold `tokio::process::Child` +
  pipe handles (strong hold, mcp_toolsets pattern — dropped Child = closed pipes =
  SIGPIPE death); dispatch_tasks revival REJECTED (schema mismatch, dormant); registry is
  a process-global `OnceLock` in zen-plugin (de facto daemon-scoped — all hosted turns run
  in the daemon per 004; documented assumption).

## 5. Frozen interfaces (lanes implement against these)

```rust
// crates/zen-plugin/src/task_registry.rs (new)
pub struct TaskSnapshot {           // cheap clone for collect/push/audit
    pub task_id: String, pub session_id: String, pub pid: u32,
    pub command_digest: String,     // binary + first args, ≤80 chars, NO secrets (env already scrubbed)
    pub output_dir: String,         // ~/.zen/logs/tasks/<task_id>/
    pub status: TaskStatus,         // Running | Completed(i32) | Failed(String) | Timeout | Killed | Orphaned
    pub started_at_ms: i64, pub ended_at_ms: Option<i64>,
    pub bytes_stdout: u64, pub bytes_stderr: u64, pub truncated: bool,
}
pub fn registry() -> &'static TaskRegistry;
impl TaskRegistry {
    pub fn spawn_background(&self, cmd: tokio::process::Command, session_id: &str,
        command_digest: &str, cfg: &BackgroundLimits) -> anyhow::Result<TaskSnapshot>;
        // register (concurrency gate FIRST) -> create logs/tasks/<id>/ -> take stdout/stderr
        // -> two drain tasks (cap: stop-writing at limit, keep counting) -> wait task
        // (exit -> meta.json via write_atomic, status transition) -> meta.json initial write
    pub fn snapshot(&self, task_id: &str) -> Option<TaskSnapshot>;
    pub fn take_status_changes(&self, session_id: &str) -> Vec<TaskSnapshot>;
        // per-session cursor: terminal transitions since last take; marks notified
    pub fn live_count(&self) -> usize;
    pub fn live_for_session(&self, session_id: &str) -> Vec<TaskSnapshot>;
    pub fn kill(&self, task_id: &str) -> anyhow::Result<TaskSnapshot>;   // SIGTERM -> 3s -> SIGKILL group
    pub fn expire_due(&self, now_ms: i64) -> Vec<String>;                 // GC-tick hook; Timeout status
    pub fn kill_all(&self, reason: &str) -> Vec<String>;                  // SIGTERM drain
}
pub fn sweep_orphans_on_boot() -> usize;  // scan logs/tasks/*/meta.json status=running -> two-tier (D1 amended: dead group mark-only / verified-ours live group SIGKILL+mark / unverifiable mark-only)
// session scoping for the tool layer (APPROVAL_TURN task-local precedent):
tokio::task_local! pub static SESSION_SCOPE: String;   // orchestrator wraps dispatch; shell_exec reads, default "local"
pub struct BackgroundLimits { pub max_wall_secs: u64, pub max_concurrent: usize, pub output_cap_bytes: u64 }
```

Tool contracts:
- `shell.exec` + `background: bool` (default false). Background result:
  `{"task_id","status":"running","output_dir","stdout_path","stderr_path","note":"collect via shell.collect; do not poll in a loop"}`.
- `shell.collect {task_id, wait_ms?}` → `{"task_id","status","exit_code?",
  "stdout_tail","stderr_tail","output_path" | inline "stdout" when ≤4k,"bytes_stdout",
  "bytes_stderr","truncated"}`. Unknown task_id → structured error mentioning
  `logs/tasks/` (meta survives daemon restart; collect falls back to meta.json when the
  registry lost the entry).
- `shell.kill {task_id}` → `{"task_id","status":"killed"}`.

Config (`crates/zen-core/src/config.rs`, `[agentic.background]`, E11 warn-not-silent,
env 5th layer): `max_wall_secs` default 1800 clamp 60..=86400 (`ZEN_BACKGROUND_MAX_WALL_SECS`),
`max_concurrent` default 4 clamp 1..=16 (`ZEN_BACKGROUND_MAX_CONCURRENT`). Output cap
(8MiB), tail (8k), inline threshold (4k), collect default wait (10s) = code constants.

Wiring: `tools.register` shell.collect + shell.kill; sensitivity BOTH **Confidential**
(collect returns shell output = same data class as shell.exec; kill is destructive);
`ToolArgRegistry`: no path/command args for collect/kill (task_id only — output paths are
SERVER-COMPUTED, never model args, sidestepping seatbelt write-deny on `.zen`).
Quality gate: `shell.collect` joins the `is_mutating_tool` read-only allowlist;
`shell.kill` stays mutating via fail-safe default.

Disk layout: `~/.zen/logs/tasks/<task_id>/{stdout,stderr,meta.json}` — `.zen` carve-out
makes it fs.read-able in every sandbox mode; meta via `write_atomic`; retention row
AgeDelete 7d on `logs/tasks/` (opencode precedent).

Push line format (orchestrator, both loops, shared helper):
`⚙ background task <id8> `<digest>` <status> — collect: shell.collect {"task_id":"<id>"}`

## 6. Lanes

- **F (foundation, sequential-first)**: zen-core config keys + config.toml; zen-vault
  retention row; zen-plugin task_registry.rs + shell_exec background param +
  shell_collect.rs + shell_kill.rs + lib exports; zen-agents wiring.rs registration
  (tools, sensitivity, arg registry).
- **O (orchestrator, after F)**: orchestrator.rs — SESSION_SCOPE around dispatch,
  round-boundary per-session scan + push (single shared fn called from BOTH execute and
  execute_stream loops), stream-callback echo, turn-end `loop.background.live` audit,
  `is_mutating_tool` += shell.collect.
- **G (gateway, after F, parallel with O)**: daemon.rs — SIGTERM drain `kill_all` with
  audit lines, boot `sweep_orphans_on_boot()`, GC-tick `expire_due` hook (if GC tick is
  gateway-side; else wire in scheduler).

## 7. Pin-test contract

1. `background:false` byte-identical (all existing shell_exec pins stay green).
2. `background:true` returns running-status immediately; exactly-once rig answer.
3. Disk truth: stdout/stderr/meta.json exist; meta running→completed with exit_code.
4. Overflow = truncate-not-kill: `truncated:true`, process completes, no SIGPIPE death.
5. Registry holds Child: pipes alive past spawning scope (no premature EOF).
6. Deadline: expired task killed, meta `Timeout` (short clamp in test).
7. Concurrency cap: over-limit spawn → structured rejection.
8. Push: exactly one line per completion; cross-session task NOT injected (security pin);
   between-turn completion picked up at next turn's first round boundary.
9. Collect: wait semantics; 8k char-boundary-safe tails; ≤4k inline full; meta-fallback
   for unknown-to-registry task.
10. Kill: group SIGKILL, meta `Killed`, audit line.
11. SIGTERM drain kills live tasks with audit; boot sweep two-tier (dead ⇒ mark-only; verified-ours live ⇒ SIGKILL+mark; unverifiable ⇒ mark-only); Notices at-least-once (take/mark split).
12. Retention row applies to `logs/tasks/`.
13. Quality gate: shell.collect read-only (no gate), shell.kill mutating (gate fires).
14. Config: clamps + invalid-value warn-not-silent + env overrides.

## 8. Deferred / revisit triggers

- plan.execute per-layer background + collect barrier (phase 2) — the natural home of
  hard "collect before next step" sequencing.
- Gateway structural `background_task` event kind (TUI-native rendering) — v1 uses the
  stream-callback echo.
- Cursor/since_bytes collect — only if fs.read byte-range proves insufficient.
- Demote-on-overrun (D3) — on production foreground-timeout-retry evidence.
- Hard turn-end barrier (D6) — on evidence models systematically never collect.
- Cross-session collect policy — v1 allows collect-by-unguessable-uuid across sessions
  (trust-equivalent to fs.read on `.zen`); tighten if that ever leaks.

## 9. Review remediation record (2026-10-31)

Review lanes: two-party blind review (contracts & ops) + Oracle arbitration of deferred
items (`ses_e702f0306ffeP9lY6vZJ78YZeP`). Both lanes flagged the same theme clusters:
async-worker blocking, exactly-once notice loss, boot-sweep pid-reuse. Landed fixes:

- **P1 (security)** = task_id uuid/charset validation + collect/kill meta.json fallback confined under `logs/tasks/<id>/` — crafted meta `stdout_path`/rewrite escape closed (arbitrary read/write).
- (a) `%`→*fix label*: SIGTERM drain loops now bound each child's kill escalation window (kill_all_blocking, no unbounded join).
- (b) `kill()` pid-reuse guard: `reaped` flag checked at signal time — never signal a reaped pid.
- (c) GC-tick expiry (3s/10s waits under lock) → mutex-gap structure: reads take a snapshot under the lock, waits/cas happen without it, status-revalidated under the lock before mutation.
- (d) boot-sweep re-checks the live registry at mark time (TOCTOU between scan and act).
- (f) sweep cost bound: metas older than the 7d retention window are skipped.
- (g) registry `Mutex` poisoning tolerance via shared `lock_ok` (warn + into_inner) — hot-path fix-now (Oracle ITEM4-g).
- (h) sweep skips metas whose task has a live registry entry (boot-object race).
- (i/j) tool-loop failure accounting + `live_count` canonical lock path.
- (k) shell.kill escalation thread named + detached cleanly; group spawned with `process_group(0)` (parent-less group kill parity).
- (l) **D4 at-least-once** (Oracle ITEM1-iii): take/mark split — `take_status_changes` re-offers un-marked terminal transitions; `mark_notified` retires them only after the carrying model round returned Ok.
- (m) **two-tier sweep** (Oracle ITEM2): dead group ⇒ mark-only; live group verified ours (leader process age vs spawn record, ±10s, macOS `/bin/ps -o etime=` / Linux procfs) ⇒ SIGKILL + mark; unverifiable ⇒ mark-only.
- (n) **D6 gate visibility** (Oracle ITEM3): facts-only background context line appended to the quality-gate Task input when the session has live tasks.
- (o) concurrency TOCTOU: atomic slot reservation + rollback on spawn failure (cap cannot be overrun).
- (p) drain_pipe meta accounting: `captured` advances only on successful writes (meta never claims disk bytes it does not hold); `truncated` = bytes actually discarded (exactly-cap output NOT flagged).

Backlog (explicitly arbitrable, deferred): meta `.audit` durability framing beyond
write_atomic; doctor surfacing of stale `running` metas; `shell.exec` structured-args
timeout propagation typing. All three have defined revisit triggers recorded here.

Security delta lane's five self-check questions (lane dropped after 3 infra failures;
hand-verified): `logs/tasks/` fs.read exposure == pre-existing audit.jsonl exposure class
(same `GLOBAL_ROOT_SUBTREES` anchor); push lines are synthesized static text (no tool
output transit); collect results pass `screen_tool_output` at both dispatch sites;
ConfidentialityHook is consulted per-invocation and spawn gates run inside the
Confidential `shell.exec` dispatch; `shell.collect`/`shell.kill` are Confidential ⇒
excluded from the external MCP registry by the FR-028 exclusion (pinned by
`build_mcp_registry_excludes_confidential_tools`).
