# Agent Pipeline QA Wave — Result Honesty, Intent Speed, Single-Writer Persistence

**Date:** 2026-10-30
**Trigger:** production report — "search and execute plan sub task and multi call
tools sub task not returning results"; intent classification stalling every hosted
turn; sub-agent/plan deliverables arriving empty or silently vetoed.
**Method:** /qa full loop — log archaeology (`~/.zen/logs/`) → parallel codebase
traces → rig-agent 0.42 contract research → Oracle design review (D1–D8) →
parallel implementation lanes → gate verification.

## 1. Production evidence (3 hosted turns, Oct 17)

| Symptom | Evidence | Root cause |
|---|---|---|
| Search turns produce no usable answer | `loop.turn.review` shows Momus veto ×3 (`UntestableAcceptanceCriteria`) on read-only queries | Quality gate ran on EVERY tool-using turn; Momus's plan-review rubric structurally vetoes search-style answers |
| 20s stall before first token | `intent_llm_ms=20001`, `intent_llm_outcome=timeout` on 2/3 turns | Hard `CLASSIFY_TIMEOUT=20s` const against the 35B default model (~19s prefill); LLM-first ordering made every keyword-obvious query pay it. Recorded baseline (V13-A.3): intent p95 < 500ms — violated ~40× |
| web.search 33s then failure | `web.search` invocation 33010ms, DDG unreachable | `retry_with_backoff(3, …)` × 10s client timeout per backend; no per-backend deadline; bare non-actionable error |
| Empty deliverables | delegate/plan results ok-shaped but empty; tool-loop exhaustion returns "" | rig-agent 0.42: `tool_results(vec![])` ⇒ `PromptCancelled`; `MaxTurnsError` propagated as turn death; empty final response delivered as success |
| Truncated answers | prose after a ```json fence vanished | `strip_fenced_tool_blocks` matched the OPEN fence at offset 0, `filter(end>0)` turned it into None ⇒ everything after the first fence dropped (pre-existing, latent) |
| "mock provider" in production audit | `gateway.secrets.redacted provider:"mock"` lines | NOT production routing — T270 integration tests ran against real `~/.zen` without `ZEN_HOME` override (test leak; separate hygiene item) |
| Non-TUI surfaces never hydrate | daemon `hydrate_session` reads jsonl that only the TUI client wrote | Conversation persistence lived client-side; `zen chat`/qqbot/HTTP turns were never persisted ⇒ hydration + memory-replay starvation |

## 2. Oracle decisions (D1–D8, all approved; some with changes)

- **D1 gate scoping** — quality gate triggers on MUTATING tool calls only;
  `Confidential` always gates; unknown tool names fail safe as mutating;
  synthetic bookkeeping names (`<parse>`, `<budget>`, `<dispatch>`,
  `<max_turns>`) never gate.
- **D2 intent speed** — (a) configurable classify timeout (8s default, clamp
  1..60s, E11 warn-not-silent); (b) `[agentic.intent] model` override for a
  small local model; (c) L0 keyword fast path (default ON — the falsifiable
  baseline was violated 40×) with the L2 LLM classification demoted to a
  detached record-only shadow logging the L0-vs-L2 disagreement pair
  (`decision_kind: l0_fast_path`, `l0_l2_agree`) so the τ-calibration
  decision stays falsifiable. Kill-switch `l0_fast_path=false` restores the
  pre-D2 ordering byte-identically.
- **D3 AgentRun contract guards** — `tool_results` never called empty while
  calls are pending (synthetic per-call error results, answered exactly once
  by rig call id); `MaxTurnsError` recovered (last assistant words + `⚠`
  notice + `<max_turns>` audit-parity record) instead of propagated.
- **D4 empty-response honesty** — an empty or tool-JSON-only final/sub-agent
  response is a structured error, never an ok-shaped empty result.
- **D5 budget exhaustion notice** — shared exhaustion-notice wording
  (annotate-never-replace): content survives, notice prefixes; contentless
  becomes notice alone.
- **D6 parse-reject audit** — unparseable fenced tool blocks emit one bounded
  `loop.parse.rejected` audit line (≤4 errors × ≤200 chars, `count` carries
  the total) per rejecting round.
- **D7 single-writer persistence** — the gateway daemon persists completed
  hosted turns (meta-first jsonl at the hydrate-discoverable
  `{sessions}/YYYY/MM/DD/<id>.jsonl`); the TUI client turn-writer is removed;
  TUI keeps a meta-only carve-out (title provenance for the resume picker);
  cancelled/error turns append nothing; persist failure never fails the turn.
  Version skew: old daemon + new TUI ⇒ interim window persists nothing —
  `zen serve restart` after upgrade closes it (documented, accepted).
- **D8 web.search** — per-request timeout (`[web_search] timeout_ms`, default
  8s, clamp 1..30s) AND a per-backend total deadline (one
  `tokio::time::timeout` around each backend's whole retry chain) ⇒
  worst-case cascade 3×8s = 24s (Oracle-approved bound); all-backends-failed
  error aggregates per-backend reasons + actionable hints (Brave/Tavily keys,
  HTTPS_PROXY for DDG).

## 3. Implementation mapping

| Decision | Landing site | Pins |
|---|---|---|
| D1 | `orchestrator.rs`: `is_mutating_tool` (read-only allowlist, `<…>` synthetic pass, unknown ⇒ mutating), `turn_gated(has_mutating, sensitivity)` | classification table (allowlist/synthetic/unknown/case-exactness), `turn_gated_scopes_to_mutating_calls_only`, read-only-turn integration pin |
| D3 | `orchestrator.rs`: `ensured_tool_results` (guards every `tool_results` site), `recover_max_turns_error`, `last_assistant_text` | AgentRun-driven fill pin (initial `CallModel` step consumed; granted set carries `fs.read`; `.max_turns(4)`), recovery wording/propagation pins |
| D4 | `delegate_task.rs`: `sub_response_honesty_error` (empty or stripped-tool-JSON ⇒ structured error, built before the QE shadow so it records `has_error:true`) | honesty-error pins + `ensured`-path contract |
| D5 | `orchestrator.rs`: `exhaustion_notice`/`apply_exhaustion_notice` shared by budget break + max-turns recovery | wording + annotate-never-replace pin |
| D6 | `orchestrator.rs`: `append_parse_reject_audit` wired into both parse-error loops (execute + stream) | bounded-entry pin (kind/turn/count/≤4×200) |
| D7 | `zen-gateway/server/hosting.rs`: `persist_completed_turn` (+ `ensure_session_meta_blocking`/`persist_turn_blocking`, spawn_blocking, exists-check-gated meta mint); `zen-cli/tui/chat.rs` writer removed; `sessions.rs` meta carve-out documented | `tests/session_persistence.rs`: meta-first shape, path parity, hydrate discoverability, ConversationStore reader parity, append-never-overwrite, cancel/error silence, persist-failure tolerance (read-only DATE DIR — home-root chmod is insufficient: POSIX creation permission is governed by the deepest directory) |
| D2a/b/c | `zen-core config.rs`: `IntentConfig::{classify_timeout_ms, model, l0_fast_path}` + accessors (clamp+warn, env layer); `intent.rs`: fast path + `spawn_l0_shadow` (Handle::try_current-guarded) + `LlmOutcome::L0FastPath`; `decision/mod.rs`: `run_l0_fast_path_shadow` + `l0_shadow_entry` | config parse/clamp/env/E11 pins ×3; fast-path-disabled ordering pin; shadow record carries both l0 and l2 labels (agreeing + disagreeing + L2-failure shapes) |
| D2b wire | `zen-core`: `ModelOptions.model`; `zen-provider`: `OllamaProvider::effective_model` (3 completion sites + 2 log sites), `DefaultRouter::stream_options` + `call_stream_with_model` (trait `call_stream` delegates `None` — byte-identical); `intent.rs complete_prompt` passes the override | `stream_options_layers_model_override_over_defaults`, `effective_model_prefers_options_override`. Scope honesty: ollama honors the override (per-call model names are native to its API); other providers keep their configured default until they read `ModelOptions::model` — documented at every layer (config.rs field doc, config.toml sample, intent.rs) |
| FIX-7 (verdict visibility, part of D1/D4 family) | execute path: `verdict_notice`/`apply_verdict_notice` annotate an upheld veto onto the delivered response; stream path: extracted pure `stream_gate_marker` — not-ready wording preserved byte-identical (pre-existing pin), `vetoed` marker additive for the previously-silent case | `verdict_notice_annotates_never_replaces_and_bounds`, `stream_gate_marker_branch_order_is_the_contract` |
| latent truncation bug | `orchestrator.rs strip_fenced_tool_blocks`: close-fence search now starts after the open marker (`rest[start+7..]`) — the offset-0 self-match previously dropped ALL text after the first fence | covered by the exhaustion wording pin (prose-tail survival) |
| D8 | `web_search.rs`: configured budget threaded into all 3 backend request builders (`.timeout()` overrides the client 10s default); every `retry_with_backoff` chain (explicit brave/tavily + all 3 cascade arms) wrapped in one `tokio::time::timeout(budget, …)` — elapsed ⇒ `"timeout after Ns"` reason, cascade continues; pure `all_backends_failed_error(&[(name, reason)])` aggregator (header + per-backend lines + fix hints) and `timeout_reason` helper. `zen-core`: `WebSearchConfig.timeout_ms` + 3 consts + `request_timeout()` accessor (out-of-range ⇒ warn + DEFAULT, mirroring `classify_timeout` — falls back, not clamps) + `apply_web_search_env` (5th layer) + `[web_search]` config.toml sample (commented keys — absent ⇒ defaults authoritative) | `all_backends_failed_error_lists_backends_and_fix_hints`, `…_single_backend_still_actionable`, `timeout_reason_maps_elapsed_to_one_line_reason`, `web_search_timeout_defaults_clamps_and_env_overrides` (parse/default/fallback-not-clamp/env/E11 + merge contract). Existing multiloop missing-key pins pass untouched; zero clippy warnings |

## 4. Deviations & discovered facts

1. **`preferred_model` was dead on the wire** — `TaskRequirements.preferred_model`
   has no consumer in zen-provider; `build_fallback_chain`'s `(Provider, model)`
   tuples have no production consumer either. Per-call model selection did not
   exist at all. Closed by the D2b-wire row above (ollama-scoped, honestly
   documented); full multi-provider plumbing recorded as follow-up.
2. **rig-agent 0.42 protocol details** (from upstream source audit): the
   initial `next_step()` yields `CallModel` and must be consumed before
   `model_response`; a ModelTurn whose tool names are absent from the granted
   sets enters invalid-tool-call resolution (next_step refuses until
   `resolve_invalid_tool_call`); `AgentRun::new` defaults `max_turns=1`.
3. **`streaming_gate_appends_warning_when_not_ready`** pinned the exact
   `delivery not ready` wording — FIX-7 stream marker extracted to
   `stream_gate_marker` preserving that branch verbatim.
4. **Test-infrastructure flake class**: plain `cargo test` (thread-parallel)
   races on process-global env mutation (`ZEN_HOME`, `ZEN_INTENT_*`) — the
   house gate is nextest (process-per-test); 15 thread-race failures under
   plain cargo test are artifacts, not regressions (T118 discipline).
5. **`spawn_race` load flake** reconfirmed (fails under cross-crate load,
   passes isolated) — pre-existing, backlog-observed.
6. **Concurrent T278 Verus wave** (plan_task formal verification) shared the
   checkout during this wave — zero file overlap; lane commits stage
   surgically.

## 5. Verification

- Crate sweeps (nextest): zen-core + zen-agents 1239/1239; zen-gateway
  194/194; zen-cli + zen-plugin 660/660; combined 4-crate sweep 1512/1512;
  post-Lane-C zen-plugin + zen-core 603/603; zen-agents re-run after clippy
  fixes 826/826.
- **Full gate `bin/test`: 3571 passed / 0 failed / 28 skipped** (baseline
  3480, +91).
- `bin/lint`: every file of this wave is clippy-clean (-D warnings) and
  rustfmt-clean; the full-workspace lint is blocked solely by the concurrent
  T278 Verus wave's files (`formal/src/plan_layers_verify.rs` 4 clippy
  errors, `plan_task.rs` fmt) — another session's WIP, deliberately untouched
  (surgical staging excludes `plan_task.rs`, `plan_task/`, `formal/`,
  `zen-agents/Cargo.toml`, `Cargo.lock`).
- Post-lint-fix clippy repairs in this wave's files (all mechanical):
  dangling doc block → `//` note (intent.rs D2a), `ZenPaths::detect().ok()`
  → `if let Ok(..)` (decision), nested verdict type → `ClassifyVerdict`
  alias (intent + decision), ENV_LOCK guard across await → justified
  `#[allow(clippy::await_holding_lock)]` (the serialization IS the point;
  T270 TEST_LOCK precedent, async form).
- Operational follow-up: daemon binary was 12 days stale (built Oct 17) —
  `cargo install --path crates/zen --force --locked` + `zen serve restart`
  required for the fixes to reach production; live repro query recorded in
  the QA session.
