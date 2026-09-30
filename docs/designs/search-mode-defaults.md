# Search-Mode Defaults — T138 Decision Memo

**Date:** 2026-09-30 · **Status:** DECISION READY (owner to pick) · **Origin:** Phase-24 convergence task T138 — "fusion+PPR reachable only via gateway `knowledge/search` auto mode (default Fast bypasses it) — decide whether that is the intended default."

## 1. Premise correction (code-verified 2026-09-30)

T138's recorded claim is **imprecise**. Fusion is NOT only reachable via the gateway RPC. The actual surface map:

| Surface | Reaches RRF+PPR fusion? | Mechanism |
|---|---|---|
| TUI chat context injection (**default Fast**) | ❌ FTS-only | `[tui] knowledge_search = "fast"` pins `tiers=["fts"]` (`tui/chat.rs:98-106`) |
| TUI chat with `[tui] knowledge_search = "full"` | ✅ multi-word auto | `tiers=None` → `TierSelector` → `search_fused` |
| TUI `/search` command | ✅ always (multi-word) | passes `tiers=None` unconditionally (`tui/commands.rs:382`) |
| `zen chat` CLI | ✅ always (multi-word) | passes `tiers=None` unconditionally (`chat_command.rs:43`) |
| Agent tools `tier2/3/4_search` | ❌ never | bypass `SearchService`/selector, call tier impls directly; `tier4_search` takes a notion name (BFS), no PPR |
| HTTP `/api/v1/chat`, qqbot channel | ❌ no KB search at all | handlers never call `knowledge/search` |
| Scheduler workers | ❌ | only `wiki_compiler` direct `search_notions_fts` |

`KnowledgeSearchMode` gates **exactly one call site** (TUI chat context injection). The gateway RPC's auto-select is a separate mechanism, and it is already the default behavior for `zen chat` and TUI `/search`.

## 2. What the modes actually cost (per fused query)

`search_fused` (`search/service.rs:74-84`) runs 4 lists in parallel: FTS5 (weight 1.0), vec0 embeddings (1.0 — requires a local embedding computation per query), entity graph (0.8), personalized PageRank (0.9), fused via RRF (k=60). Single-word / `similar:` / `graph:` / `summarize:` queries never fuse (selector routes them to a single tier).

The default-Fast trade: the TUI injects context **every turn**, so Fast buys a bounded per-turn latency profile (one FTS query) at the cost of recall — no semantic (vec0), no graph neighborhood, no PPR.

## 3. Options

**A) Status quo + close the env-var parity gap (S).** `[tui] knowledge_search` is the only 5-layer config key with **no env override** (`apply_env_overrides` lacks `ZEN_TUI_KNOWLEDGE_SEARCH`). Add it; default stays Fast.

**B) Flip the TUI chat default to Full (S code, M consequence).** Every turn pays 4 subsystem calls + a local embedding. Recall up, per-turn latency now vault-size-dependent. Touches the highest-traffic interactive path — needs a latency measurement before, not after.

**C) A fused `search_knowledge` agent tool (M).** The real gap for agentic quality: the orchestrator/delegates can call single tiers but can never fuse, so agent answers cannot use RRF+PPR retrieval regardless of any config. A `ZenTool` adapter over `SearchService::search_fused` (Private sensitivity, granted alongside tier2/3/4 in `delegate_tools::AGENT_TOOLS`) makes fusion reachable where the agent actually searches.

**D) Do nothing (record).** Defensible: `zen chat` and `/search` already fuse; the TUI per-turn injection is deliberately lean.

## 4. Recommendation

**A now, C as the follow-up worth wanting; B only behind a measured latency budget.** A is a parity fix consistent with every other config key. C closes the gap T138 was actually pointing at (agent-side fusion) without touching the interactive per-turn path. B is a product trade (recall vs per-turn latency) that should ride on measurements from C-era usage, not be flipped blind.

## 5. Decision

(Owner input pending — record choice here.)
