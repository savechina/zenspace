# Zen Architecture (Living Summary)

- **Date:** 2026-10-13
- **Role division:** This document is the *living current-architecture record* — it always reflects what the system **IS now**, and is updated on every architecture change. It is **never backfilled from specs**. Per-feature detailed designs of completed work live under `../specs/<feature>/` (historical, linked here only for depth).
- **Gates at time of writing:** `bin/test` 3115 passed / 0 failed / 25 skipped; `bin/lint` exit 0.
- **Source of truth / tracking:** tracked under git in `docs/designs/`. The `../specs/` directory is a **gitignored local ledger** (`.gitignore` → `docs/specs`), not shipped.
- **Consolidation (2026-10-13):** the six per-topic design records (`doc-conversion`, `e1-semantic-lint-writers`, `gateway-upgrade-management`, `memory-strength-knowledge-wave`, `openkb-compile-hygiene`, `search-mode-defaults`) were absorbed into this document (§3 details + §5 decision registry) and their files removed — this file is now the single architecture design record.

---

## 1. System Overview

Zen is a personal-memory / knowledge agentic CLI: notes flow `inbox → distill → wiki/graph/search`, an orchestration layer routes turns across 13 registered agents, and a gateway daemon owns the local memory store and hosted sessions. It is **local-first** — Private/Confidential data stays on local models unless a route explicitly opts into the cloud.

### 1.1 Binary / Library Split

`crates/zen/src/main.rs` is a thin (~13-line) wrapper: it runs `zen_core::process_hardening::init()`, best-effort loads `.env` via `dotenvy`, then calls `zen_cli::shell().await`. All behavior lives in the `zen-cli` library; `Option<Commands>` on the clap parser decides TUI (ratatui, `None`) vs. subcommand dispatch (`Some`). `crates/zen/src/main.rs:11`.

### 1.2 Crate Map (12 workspace members)

Verified against `Cargo.toml` `[workspace] members`.

| Crate | One-line role | Key path |
|-------|---------------|----------|
| `zen` | Binary entry (thin wrapper) | `crates/zen/src/main.rs` |
| `zen-cli` | CLI library: clap `Parser`, TUI (ratatui), 23 commands, dispatch | `crates/zen-cli/src/cli.rs` |
| `zen-core` | Core infra: config layers, error taxonomy, paths, secrets, constants, validation | `crates/zen-core/src/config.rs` |
| `zen-service` | Business logic: starter, wps, cleanup | `crates/zen-service/` |
| `zen-repo` | Unified data layer: `SqliteClient` + 9 domain repositories (FTS5, vec0, graph) | `crates/zen-repo/src/client.rs` |
| `zen-vault` | 10+ knowledge services: note, wiki, search, distill, tindy, notion, ingest, graph | `crates/zen-vault/src/` |
| `zen-agents` | 13 agents / 4 tiers, orchestrator, QualityPipeline, plan-DAG, scheduler | `crates/zen-agents/src/registry.rs` |
| `zen-provider` | 13 named providers / 3 protocol types, `DefaultRouter`, model metadata | `crates/zen-provider/src/router.rs` |
| `zen-auth` | Keychain + `SecretRef` resolution | `crates/zen-auth/` |
| `zen-plugin` | Agent tools, WASM sandbox (wasmtime), MCP client, plugin registry | `crates/zen-plugin/` |
| `zen-gateway` | Sole-owner UDS daemon: JSON-RPC registry, hosted turns, HTTP `/api/v1` + MCP, qqbot channel | `crates/zen-gateway/src/daemon.rs` |
| `zen-memory` | Identity context (`SOUL.md`, `MEMORY.md`) + memvid store wrapper | `crates/zen-memory/src/memvid_store.rs` |

### 1.3 Dependency Graph

Edges spot-verified against each crate's `Cargo.toml`. `zen-cli` depends on nearly every domain crate; domain crates depend downward on `zen-core`.

```
zen (binary)
 └── zen-cli (library: clap Parser, TUI, command dispatch)
      ├── zen-service  → zen-core
      ├── zen-gateway  → zen-core, zen-vault, zen-agents, zen-provider, zen-memory, zen-repo
      ├── zen-vault    → zen-repo → zen-core
      │                 → zen-memory, zen-provider
      ├── zen-agents   → zen-memory → zen-core
      │                 → zen-provider, zen-vault, zen-repo, zen-plugin
      ├── zen-provider → zen-auth → zen-core
      │                 → zen-core
      ├── zen-plugin   → zen-core
      └── zen-core     (leaf: config, errors, paths, types, secrets, …)

zen-memory → zen-core
zen-auth   → zen-core
zen-plugin → zen-core
```

---

## 2. Architectural Framework

### 2.1 Type Taxonomy (7 layers)

Types are grouped in seven layers, each with a single responsibility. Types must not leak across layer boundaries except through explicit dependency injection.

| Layer | Key types |
|-------|-----------|
| Config (配置层) | `ProviderConfig`, `AgentConfig`, `ModelEntry`, `ModelOptions`, `VariantConfig`, `FallbackStep`, `RetryPolicy` |
| Core (核心层) | `Sensitivity`, `MessageRole`, `Message`, `SessionStatus`, `Session`, `SessionContext`, `RetrievedNote` |
| Agent 层 | `AgentSpec`, `AgentProfile`, `Role`, `Capability`, `AgentClearance`, `ToolPermission`, `LlmPreference` |
| Execution (执行层) | `Turn`, `TurnState`, `TurnEvent`, `TurnRegistry`, `SessionHost`, `TurnExecutor` |
| Routing (路由层) | `ModelMetadata`, `ModelRouter`, `ComplexityLevel`, `TaskType`, `Task`, `SemanticEntropy` |
| Events (事件层) | `SessionEvent` (`Session` / `Message`) |
| Tools (工具层) | `ToolSchema` (external dependency: `rig_compose`) |

Notable relationships: `AgentSpec ⊂ AgentProfile` (canonical identity embeds the static definition); `Session` is the domain model persisted as the first `session/meta` JSONL event; `MessageRole` serializes lowercase (`"user"` / `"assistant"`); `TurnState` has **no** `Failed` variant — errors emit a `"turn_error"` event and the turn still transitions to `Completed`.

### 2.2 Triple-Enum Safety Taxonomy

Three deliberately separate enums model orthogonal safety concerns. They were historically conflated under one "SensitivityLevel"; **no `From`/`Into` impls exist between any pair** — cross-domain bridging is explicit at the orchestration layer, never implicit via the type system.

| Enum | Axis | Domain | Variants | Ord | Crate |
|------|------|--------|----------|-----|-------|
| `Sensitivity` | Data classification | zen-core (universal) | Public / Private / Confidential | Yes | `zen-core/src/types.rs` |
| `SafetyLevel` | Action validation | zen-core (validate only) | Safe / Warning / Protected | No | `zen-core/src/validate.rs` |
| `AgentClearance` | Agent permission | zen-agents (agent) | Low / Medium / High | Yes | `zen-agents/src/agent_profile.rs` |

### 2.3 Path Spec v2

A single root `ZEN_HOME` (production `~/.zen`, dev/test `~/.zentest`) governs all state. Vault, memory, sessions, and logs are **global** — personal knowledge follows the user, not the project.

- **Config is global-only**, at `~/.zen/config.toml` (4-layer merge); a workspace `config.toml` is never read. `crates/zen-core/src/config.rs:2310`.
- **`.zen/` is project context only**: a marker directory that also serves as the sandbox allowlist — no config, user data, or output workspace-awareness. `crates/zen-core/src/paths.rs:106`.
- **Workspace-root detection** (`ZenPaths::find_workspace_root`, `paths.rs:263`): `ZEN_WORKSPACE` env override wins (any existing dir); otherwise walk up from cwd — or from `ZEN_HOME`'s parent when `ZEN_HOME` is set — looking for a `.zen/` marker, stopping at `$HOME` / `global_root`. No marker ⇒ `None`.
- `None` is the **normal hosted form**: the launchd daemon runs with `cwd=/`, so project-workspace writes from hosted turns are not supported — the global tree stays fully functional.
- Test seam: `ZenPaths::for_testing(root)` pins a temp root with `workspace_root: None` (`paths.rs:99`).

### 2.4 Config Inheritance (4 layers) + Env + Cache

`load_config()` merges, lowest to highest (`crates/zen-core/src/config.rs:2304`):

```
Default (Rust `Default` impl)
  → embedded config/config.toml (include_dir, compile-time)
    → global ~/.zen/config.toml      (missing file ⇒ Default)
      → environment variables ZEN_*  (apply_env_overrides)
```

- `apply_env_overrides()` is the final layer and takes highest priority.
- Parsed config is **cached once per process** (`CONFIG_CACHE`, an `RwLock`, non-test builds only).
- `invalidate_config_cache()` (`config.rs:31`) is the **integration-test seam**: production never calls it; tests link the normally-compiled crate (where `#[cfg(test)]` is off and the cache is live) and must observe mid-process env changes.
- In `#[cfg(test)]` the cache is disabled entirely (fresh parse each call).

### 2.5 Config Truth Rules (T192)

- The embedded `config/config.toml` ships **only keys `ZenConfig` actually parses**; unknown keys elsewhere are silently ignored (no `deny_unknown_fields`, by design). See the file header, `config/config.toml:5`.
- Optional/sample keys appear **commented out** so their defaults are preserved and documented without changing behavior (e.g. `[tui] knowledge_search`, `config/config.toml:30`).
- Adding a key that no code reads is the "phantom-key" anti-pattern and is prohibited; each sampled key names its env override and the design doc that governs it.
## 3. Current Subsystem Architectures

The nine subsystems below carry the system's runtime behavior; each subsection names its components with `crate::path` anchors and links the design record that holds the full detail (landed reality wins over stale spec text — specs are historical).

### 3.1 Knowledge Pipeline

Notes are captured raw, converted to Markdown, distilled into the entity graph and wiki, then consumed by lint/reindex/export surfaces. The vault Markdown tree is the **canonical** source of truth; FTS5, vec0, and the entity graph are **derived** indexes inside `state.db`.

**Host ingest → inbox.** `sweep_host_sources` (`zen-agents/src/scheduler/workers/zen_loop.rs:242`) stages `md/txt/pdf/office` plus `is_convertible_extension` sidecars from each configured `[[agentic.loop.host_sources]]` entry into `vault/inbox/_incoming/{host_dir_hash}/`. The staging tree doubles as the **seen-set**: a file is ingested exactly once (pending slot OR `promoted/{name}` ledger) and re-staging never re-fires. `SourceIngester::promote_incoming` (`zen-vault/src/distill/source_ingest.rs`) moves promoted files to `inbox/{host_hash}_{filename}` and defers while the target is still pending distill. Stage and promote share the one `is_convertible_extension` predicate, so the two lists cannot drift (T4 audit fix); conversion happens at promote time, not stage time.

**All-format conversion** (`zen-vault/src/ingest/convert.rs`; design consolidated — see §5):

| Format | Mechanism | Failure semantics |
|---|---|---|
| `md` / `txt` | direct read (pure-text path byte-identical, `String::from_utf8`) | non-UTF-8 = original error |
| `pdf` | `pdf-extract` 0.10 pinned to memvid-core's same minor; `%PDF` header gate + `catch_unwind` | panic/malformed = terminal → quarantine |
| `docx/xlsx/pptx` | `office_oxide` in-process | terminal → quarantine |
| `epub/odt/rtf` | `pandoc` via `$PATH` sidecar, structured argv, 30s drain | `Unavailable` (pandoc absent) = **stay staged, retry**; `Failed` = terminal → quarantine |

**zen-loop distill cycle** (`zen-agents/src/scheduler/workers/zen_loop.rs` → `zen-vault/src/distill/pipeline.rs::run_scoped`):

```
host_sources sweep → inbox ingest → requeue_pending (archive/pending/* → inbox)
  → pipeline: T007 checkpoint → load → T004 normalize → extract → T003 Notion persist
     → auto-link → compile → contradictions → T005 archive
  → Stage 3a raw_graph_routing (GraphRouter::route_and_join)
  → LLM distill stage (llm_distill, bounded) → merge execution → Stage 4b communities → reindex
```

| Stage | Responsibility |
|---|---|
| T007 checkpoint / recovery | recover-or-restart on a prior crashed cycle; crash replay of stale txns |
| load + T004 normalize | read inbox notes, deterministic content normalization (FR-003) |
| extract | notion/entity extraction; the LLM distill stage adds bounded enrichment on top |
| T003 Notion persist | persist notions + relationships into the `state.db` graph |
| auto-link / compile | two-pass wiki compile with the ghostlink whitelist |
| contradictions | detect contradictions across compiled pages |
| T005 archive | move processed inbox notes to `vault/archive/<yyyy-mm>/` |

- **Stage dedup** (`note_stages`, migration 009): the terminal stage is recorded per `(file_path, content_hash)` only after a successful commit; cycle start skips content already archived and drops the duplicate from the inbox.
- **Stage 3a graph routing** (`zen-vault/src/graph_router.rs`): host sources route by `worker_type` — Code → deterministic slug/package notions (no LLM), Doc → heuristic `NotionExtractor`; `raw_policy = copy` preserves originals read-only under `vault/raw/{host_hash}/`. `allow_cloud=false` (default) requests `Sensitivity::Private` (local-only).
- **Later stages** (`zen_loop.rs`): Stage 4b `run_community_stage` writes `wiki/communities/<slug>.md` for communities above `community_min_size`; Stage 5b/5c incubate and reverify hypotheses, the refinement queue ordered by MAP-elites archive-cell crowdedness; wisdom hooks then emit report/gaps/git.
- **Pending requeue** (`requeue_pending`, `zen_loop.rs:878`): budget/ORAV-deferred notes land in `vault/archive/pending/` and are moved back to the inbox at cycle start — the pool drains, nothing strands (SC-004).
- **Quarantine** (`pipeline.rs:907`): terminal conversion/extraction failures go to `vault/archive/quarantine/` with a seen-set marker; `max_ingest_bytes` (default 64 MiB) fires the same path. Quarantine **never deletes user data**.
- **TransactionScope** (`zen-vault/src/distill/transaction.rs`): all cycle mutations share one txn; CAS rollback covers multi-file rollback and CAS commit gates on **external** drift only (self-writes, including `log.md`/`index.md` churn, are txn-tracked). Crash replay ④ (`recovery.rs::replay_stale_transactions`) rolls back stale `.txn-*.jsonl` left by dead processes at cycle entry, before the new `txn.begin()`.

**Compile hygiene** (compile-hygiene program ①-⑥ + E-series, consolidated — see §5):

| Mechanism | Where | Config gate |
|---|---|---|
| Ghostlink whitelist + strip | `zen-vault/src/distill/wiki_compile.rs` (`compile_with_policy`) | `[agentic.compile] ghostlink_enforcement` (default `strip`) |
| OKF conformance lint | `zen-vault/src/tindy/lint.rs` | `[agentic.compile] okf_lint` (default true) |
| `sources:` provenance | `render_page` frontmatter (code-managed; LLM never writes) | — |
| Page iterations + rollback | `zen-vault/src/wiki/iterations.rs`; CLI `zen wiki rollback` | — |
| Delete cascade | `zen-vault/src/distill/cascade.rs` (`sweep_dead_sources`, `redirect_page_source`) | cycle-internal, txn-tracked |
| Semantic lint (READ-ONLY) | `zen-vault/src/tindy/semantic_lint.rs` | `[agentic.lint] semantic` (default false) |
| Retention (report-only quarantine) | `zen-vault/src/distill/retention.rs` | `[agentic.retention]` |

Ghostlink handling is two-pass: the whitelist is pre-existing wiki page names ∪ same-run titles/slugs, compared through NFC `zen_repo::normalize_alias` (`[[Rust Guide]]` matches `rust-guide.md`); ghosts are stripped from the body **and** the frontmatter `wikilinks:` list. Page iterations are captured into `vault/iterations/` — **outside the wiki dir on purpose**, since inventory, lint, and CAS snapshot all enumerate the wiki dir — and `restore` captures the clobbered version first, so a rollback is itself reversible. The semantic lint's findings flow through the existing `reports/lint-*.md` writer; it creates **no new wiki write surface** (the E1 read-only decision). The daily retention worker (both scheduler profiles) rotates `audit.jsonl` @10 MiB keep-3 and loop-gaps/discovery-tree @5 MiB keep-2, age-deletes outbox>30d / journal>90d, and treats quarantine as **report-only**. Audit lines are flood-gated on the append-only `logs/audit.jsonl`: `loop.host.ingested`, `loop.compile.ghostlinks`, `loop.communities.computed`, `loop.note.archived` (cycle_id/source/dest/checksum — the undo triple), `loop.pending.requeued`, `loop.retention.applied`.

**Wiki consumers** (`zen-cli/src/cmd/wiki_command.rs`, `zen-vault/src/tindy/`): `zen wiki lint` (structural + OKF + provenance + optional semantic), `zen wiki reindex` (checksums + embeddings; also runs NFC alias backfill), `zen wiki export-skill [--target zen|agents]` (Agent Skills `SKILL.md`, `zen-vault/src/wiki/skill_export.rs`).

Invariants: (1) vault Markdown canonical, all indexes derived; (2) inbox drains every cycle — source of truth for SC-004; (3) quarantine is additive, never destructive; (4) a crashed cycle's partial writes are rolled back, never left half-applied.

### 3.2 Memory

The memory subsystem is the gateway-owned memvid store plus a read-time retention-strength model layered over it (Phase 31/32 — decisions consolidated, see §5).

**Store & ownership.** `ZenMemvidStore` wraps `Arc<Mutex<MemvidStore>>` (`zen-memory/src/memvid.rs:29,164`). The gateway daemon is the **sole owner** of the read-write handle; TUI/chat/HTTP surfaces reach it only through gateway RPC. Frames are append-only; the zen wrapper exposes no in-place update (re-putting a frame issues a new frame id), so *changing* values are never stored in frame metadata — only static provenance is.

| Accessing surface | Path to the store |
|---|---|
| Gateway daemon | sole RW owner of `Arc<Mutex<MemvidStore>>` |
| Hosted turns / TUI / chat | gateway RPC (`SurfaceClient`) |
| Nightly `memvid-indexer` worker | daemon-exclusive; drives `index_incremental` |
| Session-replay GC tick | daemon tick; blake3 turn-key idempotent |
| `zen discover report` | reads the report JSON only (read-only) |

**M1–M5 tiers** (indexed by `zen-memory/src/memvid_index.rs`):

| Tier | Content | Indexer |
|---|---|---|
| M1 | identity / session-working context (not memvid-indexed) | — |
| M2 | episodic journal (`memories/journal/`) | `index_m2_episodic` |
| M3 | semantic notions / knowledge-base (recursive walk) | `index_m3_semantic` |
| M4 | wisdom beliefs/reflections (`wiki/wisdom/`) | `index_m4_wisdom` |
| M5 | runtime virtue logs (`memories/virtue_logs/`) | read by `load_virtue_logs` |

**Retention strength — pure function, zero storage.** `retention_strength` (`zen-memory/src/strength.rs:67`, module performs **no I/O and reads no config**):

```
R = 0.5 ^ ( days_since(effective_anchor) / (half_life_days × (1 + log2(1 + events))) )
effective_anchor = max(content_anchor, last_reward_at.unwrap_or(content_anchor))
events = access_count + downstream_citations        (corrections deliberately excluded)
```

`content_anchor` precedence (`resolve_content_anchor`): journal-uri authored date > source-file mtime > frame `created_at` (index time is last). `half_life_days` default `30.0` (reuses `RECENCY_HALF_LIFE_DAYS`, `config.rs:1290`), clamp `1.0..=365.0`. The `1 + log2(1 + n)` stability multiplier is structural, not tuned (each doubling of reinforcements adds one half-life). Because nothing accumulates, the T153 compounding-decay bug class is structurally impossible; idempotency tests pin N-cycle ≡ one evaluation at `now + NΔ`.

**Rank floor.** `strength_floor` (`config.rs:1324`, default `0.0`) clamps the rank coefficient to `max(R, floor)` at the two rank sites (gateway `memory/search` rerank + `zen_agent::clamped_card_strength`, `zen-agents/src/zen_agent.rs:711`). Default `0.0` is byte-identical to pre-floor ordering; the emitted `strength` field stays raw `R`. `select_cards_with_strength` (`zen-memory/src/memvid_store.rs:180`) is the additive strength-aware variant; a constant resolver is byte-identical to `select_cards` (recency order).

**Provenance channel (T208).** The indexer writes `extra_metadata {source_path, tier}` via memvid `PutOptions` (`memvid_index.rs:39,90`), gated on `[agentic.memory_strength] enabled` (`memvid_indexer_worker.rs:321`); `SearchHitMetadata` echoes it back at read. Consumers MUST judge by **key presence**, never map emptiness — memvid-core injects its own `extractous_metadata` key, so an empty-vs-absent check is wrong (`zen-memory/src/memvid_store.rs` test contract). Legacy frames without provenance fall back to uri/created_at with no error.

**Reinforcement.** `MemoryReward` sidecars under `memories/.reward/` are incremented under an advisory flock on `memories/.reward/.lock` (`zen-vault/src/distill/reward_sidecar.rs:25`): `increment_access` / `increment_citations` / `increment_corrections`, keyed by `card_id_from_path`. Writes are tmp+fsync+rename atomic and `MemoryReward` is `#[serde(default)]`, so older sidecars stay decodable; a concurrent test pins zero lost updates. Both the enriched (preferred) and structured (fallback #2) retrieval paths carry gated reward increments (`zen_agent.rs:1154-1158`; T211/T221).

**Retrieval assembly** (`zen-agents/src/zen_agent.rs`): session knowledge is injected at `zen_agent.rs:1046-1050`; memvid memories come through `retrieve_memories_enriched` (preferred) → `retrieve_memories_structured` (fallback #2) → `retrieve_memories` (cards), truncated at `M1_TOP_K = 5`. **Session replay** (`zen-memory/src/session_replayer.rs`): a 30s GC tick incrementally replays `jsonl → mv2` with blake3 turn-key idempotency, checkpoints in state.db `memvid_replay_offsets`, so a restarted daemon never double-appends. **Belief tier** (read-side): `Belief::reliability() = min(provenance, content)`; durable wisdom is promotable AND reliability ≥ 0.9, while low-confidence beliefs stay readable as the M2 working tier from `memories/demoted-beliefs/`.

**Incremental indexing.** `index_incremental` (`memvid_index.rs:193`) skips files whose mtime-ms matches the `memories/.index-checksums.json` sidecar, falling back to full `index_all` when the sidecar is missing/unreadable; the nightly `memvid-indexer` worker drives this (T219 — previously a full re-append every night). `memory/rebuild` RPC deliberately stays a **manual, bounded full re-append** and honors the same provenance gate (T223).

**Gates** (`[agentic.memory_strength]`, env `ZEN_MEMORY_STRENGTH*` is the 5th layer):

| Key | Default | Effect |
|---|---|---|
| `enabled` | false | provenance write + all strength behavior |
| `half_life_days` | 30.0 (clamp 1..=365) | decay half-life |
| `search_rerank` | false | gates the two ranking consumers |
| `eviction_report_size` | 20 | nightly bottom-N report size |
| `strength_floor` | 0.0 | rank coefficient floor (0.0 = byte-identical) |

The nightly lowest-strength report is report-only: bottom-N by strength written via `write_atomic` to `logs/memory-strength-report.json`, with a `loop.memory.strength {count, lowest, half_life_days}` audit line only when `count > 0`; `zen discover report` surfaces it additively (missing/corrupt ⇒ `{"recorded": false}`). It **never deletes**. The gateway `memory/search` rerank is the public RPC surface (contract-004), served for external UDS clients; the in-repo active rerank consumer is the structured-fallback card resolver.

### 3.3 Search

Five tiers, cheapest-first (`zen-vault/src/search/`, mode-surface decisions consolidated — see §5):

| Tier | Signal | Notes |
|---|---|---|
| T1 ripgrep | filesystem regex | first cut, no index |
| T2 FTS5 | `notes_fts` (porter) | keyword; snippets read from canonical files (below) |
| T3 vec0 | semantic KNN | 384-dim embeddings zero-padded to vec0's 4096 |
| T4 graph + PPR | entity graph BFS + `seeded_ranking` PPR | damping 0.85, 20 iterations, `MAX_SEEDS = 6`, `restart = 1 - damping`; PPR runs as a 4th RRF list |
| T5 LLM synthesis | `SubgraphContext` render | only when a subgraph is assembled |

**Tier internals.** T3 embeddings are computed in-process (`compute_embeddings`, ort/ONNX) and padded to vec0's fixed 4096 width (Principle XIII #7 — vec0 has no `ALTER`, so the width is fixed once). T4's `seeded_ranking` (`tier4.rs`) resolves 3+ char alphanumeric query tokens to entity names then aliases (`MAX_SEEDS = 6`), ranks the graph relative to those seeds with PPR, and excludes the seeds themselves; it returns empty when nothing resolves. The agent-facing `tier4_search` instead takes a notion name and runs plain BFS (no PPR — PPR is fusion-only). T5 prepends a 2-hop `SubgraphContext` render.

**Query-intent routing** (`TierSelector::select_tier`): `similar:` → T3 vec0; `graph:` → T4 BFS; `summarize:` → T5; single-word → T2; multi-word auto (`preferred == 2`) → `search_fused`.

**RRF fusion** (`zen-vault/src/search/fusion.rs`): `score(d) = Σ_t weight_t / (k + rank_t(d))`, `k = RRF_K = 60` (`fusion.rs:7`, Cormack et al. / Graphiti); documents key on `(file, line)` so multi-signal agreement accumulates and each `FusedResult` carries `provenance` (the tier labels that matched). `search_fused` (`zen-vault/src/search/service.rs:54`) runs the four indexed tiers in parallel via `tokio::join!` (`service.rs:74`); a failing tier contributes nothing and warns — the remaining tiers still rank (per-tier **fail-open**). Weights (`service.rs:31-34`): FTS5 `1.0` / vec0 `1.0` / graph `0.8` / PPR `0.9`. `DEFAULT_SEARCH_LIMIT = 20` (`service.rs:24`). Fusion is not free: vec0 requires a local embedding computation per query.

**Mode routing** (`SearchService::search`, `service.rs:151`): `preferred == 2` routes through `search_fused`; otherwise the selected single tier runs.

| Surface | Reaches fusion? | Mechanism |
|---|---|---|
| TUI chat context injection (default **Fast**) | no — FTS only | `[tui] knowledge_search = "fast"` pins `tiers=["fts"]` |
| TUI chat with `knowledge_search = "full"` | yes (multi-word) | `tiers=None` → selector → fusion |
| TUI `/search`, `zen chat` CLI | yes (multi-word) | `tiers=None` unconditionally |
| Gateway `knowledge/search` RPC | yes (auto-select) | its own tier auto-select, separate from `KnowledgeSearchMode` |
| Agent tools `tier2/3/4_search` | never | call single tier impls directly (no `SearchService`) |
| Scheduler workers (`wiki_compiler`) | never | direct `search_notions_fts` only |

`ZEN_TUI_KNOWLEDGE_SEARCH` (`fast|full|off`, case-insensitive; unparsable warns and keeps the TOML value) is the env override for the TUI mode — default stays Fast. `tier3_search` closes the gap where semantic KNN was reachable only from the gateway RPC (granted to the search-capable agents alongside `tier2_search`/`tier4_search` and `compute_embeddings`). Options C (a fused `search_knowledge` agent tool) and B (flip the TUI default behind a measured latency budget) remain recorded follow-ups.

**Tier-2 snippets** (`zen-repo/src/notes_repo.rs`): `notes_fts` is contentless (`content=''`), so the FTS query returns `'' as snippet` and the hit's snippet is populated by reading the **canonical file** (`read_canonical_snippet`). Cap `SNIPPET_FILE_MAX_BYTES = 256 * 1024` (`notes_repo.rs:17`); missing / non-UTF-8 / over-cap ⇒ empty snippet, zero logging (fail-open). One data-layer fix serves every tier-2 consumer (gateway `knowledge/search`, TUI Fast, `tier2_search`, fusion).

Invariants: indexes never gate the canonical store; every tier fails open; agent tools never silently fuse (single-tier contract).
### 3.4 Orchestration & Delegation

The turn plane. `crates/zen-agents` owns pure domain logic (registry, agent loop, quality gate, guards); the gateway calls it and never re-implements it. The multi-agent pieces that were once a dead parallel system (F1 stub delegation, F3 `ZenCoordinator`) were fused in 006 and later superseded in part by 001-vision convergence (T372-T379, PD-01) — the ledger lives in `../specs/006-agentic-orchestration/spec.md` (C1-C6).

#### Registry — 13 agents, 4 tiers

`AgentProfile` is the canonical identity (`crates/zen-agents/src/agent_profile.rs`), managed by `DefaultAgentRegistry` (`registry.rs`) and embedding `AgentSpec` as `definition: Option<AgentSpec>` (static prompt/permissions/constraints). Tier = `Role`; clearance = `AgentClearance` (Low/Medium/High), compared against `Sensitivity` only at the orchestrator layer (no cross-domain `From` impls).

| Tier | `Role` | Members |
|---|---|---|
| Orchestrator | `Role::Orchestrator` | Sisyphus |
| Planner | `Role::Planner` | Metis, Momus, Prometheus, Zeus |
| Specialist | `Role::Specialist` | Oracle, Explore, Librarian, Argus |
| Worker | `Role::Worker` | Junior, Hermes, Hephaestus, Atlas |

#### AgentOrchestrator turn flow

`AgentOrchestrator::execute` / `execute_stream` (`crates/zen-agents/src/orchestrator.rs`) drive one turn through the same spine:

```
prompt
  -> classify_intent(...)        # intent ladder (below)
  -> build_agent(agent)          # grants from delegate_tools::AGENT_TOOLS + [agents.tools] overlay
  -> inject_knowledge_context    # M1 retrieval, gated [agentic.orchestrator] knowledge_inject
  -> inject_skill_hits + inject_wake_up_brief
  -> AgentRun loop               # rig-agent state machine (PD-01 B)
       CallModel -> resolve_invocations -> dispatch (5 hooks) -> model_response
  -> quality_pipeline            # only tool-mutating or Confidential (see below)
  -> AgentExecution { response, quality_notes, delivery_ready, tool_calls }
```

**Intent ladder** (`crates/zen-agents/src/decision/mod.rs`, T168/T169; fallback `intent.rs`):
- **L0** `KeywordRung` — deterministic `INTENT_SIGNALS` keyword fast path; abstains on miss.
- **L1** calibrated rungs — `EmbeddingIntentRouter` (cosine over `SEED_INTENT_EXAMPLES`) then `LocalClassifierRung` (typed local model); each accepted only at/above calibrated τ. No τ (default) ⇒ L1 is observation-only and the byte-identical pre-T169 path runs.
- **L2** `LlmRung` — the LLM classifier with the 0.7 verbal-confidence gate; failure/timeout (20s) degrades to the L0 fast path; below 0.7 → Conversation fallback.
- Signal-based resolution (`resolve_agent(category, signal)`) keeps all 13 agents reachable from every rung. The probe that L1 rungs depend on (`is_local_llm_available`) is now wired through `impl LlmRouter for DefaultRouter` (`crates/zen-provider/src/router.rs:1541`), delegating to the inherent Ollama-only probe whose `health_check` was dedicated-threaded in the D1-class fix (2026-10-13). Byte-identity today: all three consumers sit behind τ gates defaulting closed.

**Tool loop** (PD-01 B, T114/T115/T095): `execute`/`execute_stream` run inside `AgentRun` (`max_turns = max_tool_rounds + 2` headroom), with zen keeping ALL I/O. Native `ToolCall` is the primary channel (`resolve_invocations`); the fenced parse runs only when zero native calls appear. `[agentic.tool_loop] max_rounds` default 8, clamp `1..=16` (`ZEN_TOOL_MAX_ROUNDS`). Per-round token accounting plus a mid-loop bailout when the turn budget is exhausted.

**Hosted-turn data flow** (surface → gateway → agents → surface):

```
TUI / chat / qqbot / HTTP client
  -> UDS or loopback HTTP frame  (JSON-RPC 2.0)
  -> gateway dispatch            (initialize handshake, guards, registry lookup)
  -> SessionHost::turn           (claims turnId, registers approvals, starts watchdog)
  -> TurnExecutor::execute_stream (orchestrator adapter)
        -> AgentOrchestrator turn flow (above)
        -> token callback -> delta/structural frames
  <- per-client bounded outbound queue (delta lossy-merged, structural never dropped)
  -> session/event notifications (+ session/resume replay after lastSeq)
```

#### delegate.task — model-driven sub-agents

`DelegateTaskTool` (`crates/zen-agents/src/delegate_task.rs`) runs a REAL LLM sub-turn (`MAX_DELEGATE_ROUNDS = 4`) on an isolated `ZenAgent`, returning its final response as tool output. Hard gates live in the shared `validate_task` (also used by `plan.execute`): known agent, `spawn_allowed` tier matrix (O→O denies, P→P/O denies, Specialist/Worker are leaves), depth cap, and bounded-prompt limit.

| Knob | Default | Clamp / rule |
|---|---|---|
| `delegate.enabled` (kill switch) | true | off ⇒ turns never delegate |
| `max_depth` (T373) | 1 | 1..=3; task-local `DELEGATE_DEPTH`/`DELEGATE_PARENT` chain, root parent Sisyphus |
| fan-out `tasks[]` | — | hard cap `FANOUT_MAX_TASKS = 8` (T216, `parse_requests`); slot-ordered results (`results[i] ≡ tasks[i]`) |
| `max_concurrent` (T374) | 4 | 1..=8; batch width ≤4 passes the consumer gate |
| `context_inheritance` (T215) | false | on ⇒ `compose_sub_session` copies parent knowledge, budget `base >> depth` clamp ≥512 |

Pre-spawn advisory gates (independent / consumer-decision / bounded-32k / worth-it) are audited per task as `loop.delegate.gates`, carrying additive `{inherited_notes, inherited_chars, skill_hits}` (T215). Sub-agents strip `delegate.*` at the depth cap and always strip `plan.execute` (`build_sub_agent`).

#### plan.execute — Sisyphus-only plan DAG

`PlanExecuteTool` (`crates/zen-agents/src/plan_task.rs`) executes a model-drafted `{tasks:[{id, agent, prompt, depends_on}]}`: Kahn layering (≤12 tasks `MAX_PLAN_TASKS`, ≤3 layers `MAX_PLAN_LAYERS`), per-layer parallel batches through the shared delegate `run_single`, upstream failure → downstream skipped, merged deliverable through the quality pipeline (Hermes verdict = completion signal), one `loop.plan.completed` audit line. Persistence is `workflow_plans`/`workflow_tasks` (migrations 006/007) with per-task idempotent checkpoints; `resume_plan_id` replays ok-checkpoints and re-runs the rest, claim-fenced via `owner` (one live resume per plan, stale >3600s stealable).

#### Quality pipeline & dispatch hooks

- **Quality gate** (`crates/zen-agents/src/review/`): Metis → Momus → Hermes, with Zeus escalation and the async `SemanticReviewer` for HIGH blast radius (`Confidential` metadata only — the dead entropy leg was deleted). A Momus veto triggers exactly ONE feedback round, then delivers with notes. Scope is narrowed: the gate runs ONLY for tool-mutating turns (`!tool_calls.is_empty()`) or Confidential sessions; pure chat synthesizes `delivery_ready: true` (D3, 2026-09-11). The local judge is de-anchored (T170: commits its own answer before seeing the draft); escalation to the frontier fires only below `[agentic.review] escalate_threshold`, absent ⇒ frontier-only byte-identical.
- **Dispatch pipeline** (`crates/zen-agents/src/wiring.rs:636-647`): every tool call passes `ConfidentialityHook → DispatchBudgetHook → SeatbeltHook → ToolAuditHook → AskApprovalHook`. Approval routing is turn-affine (T103): the hook reads the task-local `APPROVAL_TURN` and routes by exact `turn_id`, so concurrent turns never claim each other's route; unscoped TUI/CLI keeps the legacy first-free path.

### 3.5 Gateway & Carriers

`crates/zen-gateway` is the sole-owner daemon: one `ZEN_HOME` = one daemon. Design contract: [004-agentic-gateway/design.md](../specs/004-agentic-gateway/design.md); upgrade management: consolidated — see §5 (Phase 30 entry).

#### Ownership & lifecycle

- **StartupLock** (`daemon.rs::StartupLock`) is taken before `uds::bind_socket`, making the exists→probe→unlink-stale→bind hand-off exclusive (kernel releases on SIGKILL). The daemon writes its own pid after winning the lock; CLI start is idempotent (a live daemon pre-probe prints "already running" and exits 0 without touching the pid file).
- **Scheduler role lease** (`zen-agents/src/scheduler/lease.rs`): a cross-process advisory flock on `<logs>/scheduler.lock` held for the scheduler host's lifetime, so two TUIs / TUI-then-daemon cannot double-fire. `health/status.scheduler` prefers the live `AtomicBool` over the static `scheduler_hosted` flag.

#### Transports

| Carrier | Surface | Notes |
|---|---|---|
| UDS JSON-RPC 2.0 | primary TUI/chat | `<global>/gateway.sock` 0600, JSONL; `MethodRegistry` = 21 rows; error catalog `-32000..-32099` closed/additive (`protocol/`) |
| Loopback HTTP (axum) | `--http` | `/health` + `/api/v1/{chat,agents,ws,mcp}`, binds 127.0.0.1 only |
| MCP stdio | `zen serve mcp` | one gateway-owned offering; Streamable HTTP also mounted |
| qqbot channel | `[channels.qqbot]` | WS gateway + v2 REST sender; allowlisted chats (`allowed_users`, deny-by-default) get persistent agent sessions; `/new` resets a session, `/status` renders gateway health; `OutboxDrainer` actively pushes staged morning-briefs |

The carrier-independent envelope (P2) is identical across carriers; only framing differs. `Transport` (`transport/mod.rs`) is the whole carrier contract — async send/recv of `Frame`s, nothing else.

#### Turn lifecycle

Hosted turns run through the one-method `TurnExecutor` trait (`server/hosting.rs`), an `#[async_trait]` adapter with an owned-payload callback (`&mut dyn FnMut(String) + Send`) so the signature stays lifetime-free. The orchestrator adapter drives `execute_stream`; the gateway emits delta frames (lossy-merged) and never-dropped **structural** frames (`tool_started`/`tool_completed`, `turn_completed`, `turn_error`). Replay uses a per-turn ring (2048 frames / 60s, `RING_CAPACITY`/`RING_WINDOW`) served by `session/resume {turnId, lastSeq}`; a replayed `session/turn` returns `-32004 turn-already-completed` (idempotent). Audit: `gateway.turn.started` once per registration, exactly-once `gateway.turn.completed` (outcome completed|cancelled).

#### Guards

| Guard | Value | Failure contained |
|---|---|---|
| Turn watchdog | 900s (`ZEN_TURN_WATCHDOG_SECS`) | hung LLM/tool call |
| Circuit breaker | 5 consecutive failures → 60s | cascade failures |
| Doom-loop | 20 turns / 10 min window | agent stuck repeating |
| Stale-client GC | 30s heartbeat | dead clients holding approvals |
| Timeout cascade | client ceiling 960s > watchdog 900s > first-token 600s | ordering guarantees the daemon resolves before the client abandons |

Every rejection is `-32020 guard-rejected{guard, reason}` plus an `<logs>/audit.jsonl` line.

#### Approval routing, upgrade management, launchd

- **Approval (Q3)** with `ZEN_SANDBOX_MODE=ask` routes the `approval/request` back to the originating surface; the T103 turn-affinity broker (`server/approval.rs`) matches by exact `turn_id` (`decide_for`/`route_for`; unknown/claimed → Deny). Carriers that did not negotiate `approvals` fail fast with `-32010`.
- **Upgrade management**: the pid record carries additive `version`/`protocol`; `zen serve status [--json]` compares binary vs daemon version and shows `STALE`/`STALE-unknown`. `zen serve restart` uses `launchctl kickstart -k` when the LaunchAgent exists, else graceful stop (SIGTERM drain ≤10s + audited cancels) under StartupLock. `[gateway] upgrade_policy = warn | auto-restart`, default **warn** (env `ZEN_GATEWAY_UPGRADE_POLICY`); invalid values fall back to warn with a warning. `zen doctor`'s `daemon-version` probe is diagnose-only.
- **launchd persistence** (macOS-only): `zen serve install` writes `~/Library/LaunchAgents/dev.zen.serve.plist` (`RunAtLoad`, `KeepAlive`, 60s throttle); `zen serve uninstall` boots out and removes it. Non-macOS returns a clear `ZenError`.
### 3.6 Providers & Routing

LLM access is abstracted behind the `LlmRouter` trait (`crates/zen-provider/src/router.rs:362`) and implemented by `DefaultRouter` (`crates/zen-provider/src/router.rs:648`). Thirteen named providers map onto three protocol types; the routing layer is provider-agnostic above the protocol adapter.

| Protocol | Named providers | Adapter |
|----------|-----------------|---------|
| rig-native | ollama, openai, anthropic, cohere, gemini, mistral | direct rig-core clients |
| openai-compatible | deepseek, aliyun, groq, perplexity | `providers/rig_openai.rs` wrapper |
| anthropic-compatible | moonshot | openai adapter with anthropic shape |

`Provider` is the enum every entry point parses through (`crates/zen-provider/src/router.rs:101`, `parse_provider_name` at `:1555`); `Mock`/`Unknown` are tolerated so config typos degrade rather than panic. `LlmRouter` surface:

- `route(&TaskRequirements) -> Provider` — provider selection (`crates/zen-provider/src/router.rs:364`).
- `call` / `call_stream` / `list_providers` — dispatch.
- `call_with_usage` — default delegates to `call` with `usage: None` (`:378`); `DefaultRouter` overrides to surface real usage.
- `is_local_llm_available` — trait default is `false` (`:404`); `DefaultRouter` overrides (`:1546`) by delegating to its inherent probe (`:1243`), which touches **only** `provider_instance("ollama")` + `health_check()`. The free helper `is_local_llm_available(&dyn LlmRouter)` (`:1578`) now reaches that real probe (previously hit the trait default — dormant local-first rungs).

`DefaultRouter` extras: `route_with_preferences` (`:1272`, `LocalOnly`/`CloudOnly`/`Provider`/`Any` preference ladder), `complete_metered` (`:1053`), `build_fallback_chain` (`:1318`).

**Auth resolution (4-tier)** — `resolve_api_key` (`crates/zen-provider/src/router.rs:27`):

`SecretRef`/Keychain (`zen-{provider}-api-key` → env fallback via `SecretResolver`) → configured `api_key` `SecretRef` → `api_key_env` → default `{PROVIDER}_API_KEY` env; ollama needs none (local, no auth tier).

**Model selection** — `TaskRequirements` (`crates/zen-provider/src/router.rs:197`) drives `ModelRouter`. *Accuracy note:* specs name `ModelRouter::select`; the landed method is `ModelRouter::route_task(ComplexityLevel)` (`crates/zen-provider/src/model_meta.rs:203`) — `Simple`→cheapest, `Standard`→best `cost_efficiency`, `Complex`→highest `capability_score`, `Critical`→max capability. Routing metrics live in `ModelStats` / `PromptHookTelemetry` (`:270`–`:328`).

**Fallback & lazy construction** — `build_fallback_chain` (`crates/zen-provider/src/router.rs:1318`) orders agent provider → configured `[[agents.*.fallbacks]]` → default provider → terminal `Mock`. Providers are constructed lazily and secrets resolved at first use, so an unconfigured provider never blocks startup. Only Ollama's `health_check` is reachable from the local probe; the sync `health_check` on the cloud providers (anthropic/mistral/cohere/gemini) is currently unused.

**Async-safety inventory** (the sync surface must never block on a runtime thread):

| Site | Guarantee | Pin |
|------|-----------|-----|
| provider `complete()` / `embed()` (6 providers) | dedicated `std::thread::spawn` → own runtime → `join()`; Err not panic | `crates/zen-provider/tests/sync_complete_no_panic.rs` |
| `OllamaProvider::health_check` | dedicated-thread probe (2026-10-13; was `reqwest::blocking` on the calling thread) | `sync_complete_no_panic.rs` + `crates/zen-provider/tests/local_llm_probe_dyn.rs` |
| `intent::llm_classify`, review-pipeline judge, `semantic_lint` | callers wrap sync route/call in `spawn_blocking` | per-site tests |

**Streaming budgets** (`crates/zen-agents/src/completion_model.rs:11`): first-token **600s** (`ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS`, covers cold local-model load + prefill), inter-token **120s** (`ZEN_STREAM_INACTIVITY_TIMEOUT_SECS`); client turn ceiling **960s** (`ZEN_TURN_TIMEOUT_SECS`). Invariant: turn ceiling > gateway watchdog (900s) > first-token budget.

**Sensitivity routing** — `enforce_sensitivity(provider, sensitivity)` (`crates/zen-provider/src/router.rs:1203`), called by `route` and `route_with_preferences`, refuses to send `Private`/`Confidential` data to a cloud provider. When no local model is reachable it returns the typed `LlmError::ProviderUnavailable` ("Local LLM unavailable…") rather than silently escalating to cloud.

**Provider adapters** — one module per rig-native protocol under `crates/zen-provider/src/providers/` (`ollama.rs`, `openai.rs`, `anthropic.rs`, `cohere.rs`, `mistral.rs`, `gemini.rs`); deepseek/aliyun/groq/perplexity and moonshot ride the openai/anthropic adapters.

**Call-site surface** — `LlmRouterExt::complete(task, request, sensitivity)` (`crates/zen-provider/src/router.rs:465`) builds `TaskRequirements { max_tokens, sensitivity, preferred_model, budget_limit }` (`:197`) and maps failures to canonical `ZenError`. `complete_metered` returns `MeteredCompletion { text, provider, model, input_tokens, output_tokens, cost_usd }` (`:211`) — exactly `0.0` for local or unpriced providers.

**Usage plumbing** — `SyncUsage` (`crates/zen-provider/src/router.rs:248`) + `UsedCompletion` (`:261`) + `reconcile_usage` (`:283`): a real non-zero usage echo wins; `None`/all-zero (rig's "failed to report") falls back to a `bytes / 4` estimate. `complete_metered` bills the echo directly on the non-stream path.

**Cached-path pricing** — `CachedTokenUsage` (`crates/zen-provider/src/model_meta.rs:101`) models Anthropic's four **disjoint** buckets; `cached_usage_to_cost_usd` (`:165`) weights them: uncached **1x**, cache write **1.25x** (`CACHE_WRITE_MULTIPLIER`, `:84`), cache read **0.1x** (`CACHE_READ_MULTIPLIER`, `:90`), output at output price. `CacheSegments`/`PromptCacheControl` (`crates/zen-provider/src/cache.rs`) partition system/doc/summary/whitelist; `AnthropicProvider::complete_cached` bypasses rig-core over raw `/v1/messages` (client exposes no `cache_control`) with a 120s timeout (`crates/zen-provider/src/providers/anthropic.rs:30`). `[agentic.cache] breakpoints` is consumed by the T046 `llm_distill` stage (the reader that makes the key non-phantom).

### 3.7 Scheduler & Learning Loops

`ZenScheduler` (`crates/zen-agents/src/scheduler/mod.rs`) is a cron-driven tick loop (`DEFAULT_TICK_INTERVAL_SECONDS = 30`, `:107`) using the `cron` crate. Wall-clock cron fields evaluate in `[cron].timezone` (default **`Asia/Shanghai`**; env `ZEN_CRON_TIMEZONE`), resolved by `CronConfig::timezone_or_default()`; an unparsable name falls back to `Utc` plus a warning. Fire decisions anchor on the passed tick instant (`Schedule::after`), never the system clock. `create_default_scheduler()` (`:661`) is the config-free dormant constructor (stays `Utc`); production serve/TUI uses `create_configured_scheduler[_with]` (`:744`), which honors timezone wiring and the `[agentic.loop]` gate. A per-worker in-flight flag (`:114`) ensures a slow worker never overlaps itself; the flag is cleared via a panic-safe guard.

Two worker-set profiles (`SchedulerProfile`, `:619`):

| Profile | Host | Workers |
|---------|------|---------|
| `Full` | `zen serve start` daemon | all 17 |
| `InApp` | TUI in-process | 15 (excludes daemon-exclusive `memvid-indexer` and `morning-brief`) |

Concurrency is guarded by a cross-process role lease (`crates/zen-agents/src/scheduler/lease.rs`): advisory `flock` on `<logs>/scheduler.lock`, kernel-released on exit. `LeaseError::Contention` (skip/back off) is distinguished from `OpenFailed` (loud filesystem error). The TUI probe gate fails **closed** when the lease is held; the daemon retries until free. This closes the probe-then-spawn TOCTOU between two TUIs or a TUI-then-daemon start.

**Worker contract** — every worker implements `ZenWorker { id, description, schedule, execute }` (`crates/zen-agents/src/scheduler/mod.rs:72`) and returns `WorkerReport { worker_id, success, fact_count, duration_ms, llm_cost_usd }` (`:56`). `worker_ids()`/`list()` (`:546`/`:551`) expose registration summaries; `enable`/`disable` (`:566`/`:577`) gate a worker without re-registration. Cron expressions use the 6-field (seconds-first) form.

**Worker inventory** (id → role):

| Worker | Role |
|--------|------|
| `session-journaler` | Scan session conversations, extract durable facts, write journal entries |
| `dream` | Nightly consolidation: extract facts, update memory, compress logs, recompute notions |
| `memory-curator` | Route typed signals to `wiki/wisdom` + memories surfaces, update `MEMORY.md` |
| `memvid-indexer` | Nightly ingest journal/wiki/wisdom into the memvid store (incremental checksums) |
| `subconscious` | Evaluate workspace state, log micro-actions (remind, suggest, organize) |
| `notion-extractor` | Scan journals, extract notions (LLM + keyword fallback), upsert to state.db |
| `wiki-compiler` | Compile wiki pages from state.db notions (incremental) |
| `commitment-tracker` | Track commitment lifecycle, due-date review, anti-talk detection |
| `reflection` | Aggregate session reflections into `wiki/wisdom/reflections/` |
| `wisdom-synth` | Weekly LLM synthesis of reflections + beliefs into candidates |
| `decision-tracker` | Scan decisions for 30-day unclosed; trigger review |
| `express` | Weekly LLM expression of insights into publishable drafts |
| `evidence-gatherer` | Scan low-evidence beliefs, generate research suggestions |
| `promotion` | Stage validated hypotheses into the Hybrid C promotion queue |
| `zen-loop` | Knowledge-processing RSI cycle (below) |
| `morning-brief` | 9am 3-line brief staged to the qqbot outbox |
| `retention` | Daily bounded-retention sweep (rotate JSONL, age-delete, cap keep-newest) |

**Coexistence model** — a `zen serve` daemon registers `Full`; a TUI with no live hosted scheduler registers `InApp` behind the lease. `[cron] tui_scheduler` (env `ZEN_TUI_SCHEDULER`) disables all out-of-daemon learning when false.

Default cadences (all overridable via `[cron]`): `memory-curator`/`subconscious`/`session-journaler`/`zen-loop` every 5 min, `notion-extractor` every 10 min, `wiki-compiler` every 30 min, `dream` 02:00, `wisdom-synth` Sun 02:00, `express` Sat 15:00, `commitment-tracker` 08:00, `reflection` 06:00, `evidence-gatherer` Mon 06:00, `memvid-indexer` 01:00, `retention` 03:30, `decision-tracker` 09:00, `promotion` 04:00, `morning-brief` 09:00.

**Per-worker gating** — `zen-loop` is gated by `[agentic.loop].enabled` (disabled ⇒ manual-only via `zen wiki loop run`); `retention` by `[agentic.retention] enabled`/`dry_run`; the memvid-indexer's memory-strength report by `[agentic.memory_strength] enabled`. Each run that acts emits a structured audit line (e.g. `loop.retention.applied`, `loop.memory.strength`).

**zen-loop RSI cycle** (`crates/zen-agents/src/scheduler/workers/zen_loop.rs`) — per tick: pending re-queue (`requeue_pending`) → host-source sweep → inbox ingest → graph routing (`3a`) → distill (normalize/txn/checkpoint) → tree-index long docs → verify → community summarization (`4b`) → reindex → hypothesis incubation (`5b`) → refinement/reverify (`5c`) → report/gaps/git. Budget is configurable (`[agentic.loop] max_steps` default 10, `max_tokens` default 8000). The CAS gate (`cas_commit`, self-write-aware OCC in `crates/zen-vault/src/distill/pipeline.rs:468`) commits only on **external** drift, so a writing cycle never livelocks on its own edits.

**Learning loops** (all wired, not spec-only):

- RLVR reward sidecars → refinement queue ordered by **archive illumination** (`Archive::prioritize`; cells from `crates/zen-vault/src/distill/archive.rs`).
- Hypothesis archive as a MAP-elites grid (evidence × gap-domain × freshness) persisted to `logs/hypothesis-archive.json`.
- Reflexion verbal feedback on every rejection (`crates/zen-vault/src/distill/reflection.rs`), appended to exploration prompts.
- Skill precipitation behind the Hybrid C first-occurrence gate + report-only trigger eval (`crates/zen-agents/src/skill_trigger_eval.rs`).
- Calibration harness `crates/zen-vault/src/distill/decision_audit.rs`: ECE/Brier over labeled records, Clopper-Pearson certified thresholds written to `logs/decision-audit/thresholds.json` (absent ⇒ gate closed, byte-identical legacy behavior).
- Vendor eval gate (`crates/zen-vault/src/distill/vendor_eval.rs`) over four axes, reporting `InsufficientData` rather than a fabricated verdict.

Worker spend is metered: `WorkerReport.llm_cost_usd` → month-keyed `WorkerCostLedger` (`logs/worker-costs.json`, `crates/zen-agents/src/scheduler/mod.rs:180`), enforced against `[cron] llm_cost_cap_usd`. Other durable sidecars: `logs/loop-last-report.json`, `logs/refinement-queue.json`, `logs/placeholders.json`. Reference: §5 (Phase 30 entry).

### 3.8 Safety & Hardening

**Sandbox modes** (`crates/zen-core/src/sandbox.rs:15`, env `ZEN_SANDBOX_MODE`):

| Mode | Meaning |
|------|---------|
| `read-only` | No writes; most restrictive |
| `workspace-write` | Writes confined to workspace roots |
| `ask` | Prompt the originating surface for approval (`ZEN_SANDBOX_MODE=ask`) |
| `danger-full-access` | All guards relaxed (explicit, unsafe) |

`SandboxValidator` (`crates/zen-core/src/sandbox.rs:534`) resolves symlinks and canonicalizes before path checks (`validate_path_for_write`/`_read`), rejecting escapes into `PROTECTED_PATHS` `.git/.zen/.ssh/.aws/.gnupg/.env` (`:59`) and workspace-exit; macOS `/tmp → /private/tmp` root-canonicalization is handled. `ToolArgRegistry` (`:246`) maps each tool to its path/command-bearing arg keys so no tool bypasses validation (`validate_invocation`, `:404`), and `is_blocked_command` (`:170`) rejects destructive shell patterns (`rm -rf /`, etc.); `is_dangerous_network_target` (`:189`) catches policy-banned network commands.

**Gateway guards** (`crates/zen-gateway/src/server/guards.rs`) protect hosted turns:

| Guard | Threshold | Effect |
|-------|-----------|--------|
| Watchdog | 900s (`ZEN_TURN_WATCHDOG_SECS`) | Cancels a stalled turn |
| Circuit breaker | 5 consecutive failures → 60s cool-down | Pauses a flaky session |
| Doom loop | 20 turns / 600s window | Caps runaway turn rate |
| Stale-client GC | 30s | Reclaims abandoned clients |

Rejections surface as JSON-RPC `-32020 guard-rejected{guard,reason}` plus an audit line in `<logs>/audit.jsonl`.

**Network egress policy** (`crates/zen-core/src/network_policy.rs`) — `validate_url` blocks non-public IPs (loopback, link-local, RFC1918) and metadata hostnames (`metadata.google.internal`, `metadata.goog`) before any request; allowlist via `[sandbox.network_policy]` `allow_hosts`; `allow_private_ranges` is the DangerFullAccess parity escape.

**Env scrubbing** (`crates/zen-core/src/env_scrub.rs`) — child processes never inherit secret-bearing vars (`*_API_KEY`/`_TOKEN`/`_SECRET`/`_PASSWORD`/`_CREDENTIAL` plus an `ALWAYS_SCRUB` list) or loader-injection vars; `shell.exec`'s `env` param is the only sanctioned injection path (SC-017: child env contains zero secret-bearing names). The same scrubbing applies to MCP stdio transport.

**Resource limits** (`apply_resource_limits`, `crates/zen-core/src/sandbox.rs:769`, called from `crates/zen-agents/src/wiring.rs:279`): `RLIMIT_NPROC = 50`, `RLIMIT_NOFILE = 256`, `RLIMIT_CORE = 0`; `ZEN_SKIP_RLIMIT` opts out (test/gateway harness).

**Process hardening** (`crates/zen-core/src/process_hardening.rs`, first call in `main()`): strips `LD_PRELOAD`/`LD_LIBRARY_PATH`/`DYLD_INSERT_LIBRARIES`/`DYLD_LIBRARY_PATH` (exact matches only), denies debugger attachment (`PT_DENY_ATTACH` on macOS / `prctl` on Linux), and sets `RLIMIT_CORE = 0`. All steps best-effort, never panic.

**Tempfile lifecycle** (`crates/zen-core/src/tempfile_lifecycle.rs`) — `TempfileDropGuard` RAII removes `.tmp`/`.bak` on drop (panic/cancel safe); `boot_time_sweep` clears stale artifacts left by crashed operations. Atomic writes (`zen_core::atomic_file::write_atomic`, tmp→fsync→rename) cover the JSON sidecars above.

**Signal drain** — `zen serve` handles SIGTERM/SIGINT with a drain window (<=10s daemon drain, 5s external wait, `crates/zen-gateway/src/daemon.rs:1329`) that cancels in-flight turns with audit lines and exits 0; the TUI uses a 5s drain.

**Plugin framework** — the `Plugin` trait + `PluginApi` (`crates/zen-plugin/src/lib.rs`) register tools, hooks, and read-only workspace access; plugin hooks append after the built-in 5-hook pipeline (confidentiality → budget → seatbelt → audit → approval) and are wrapped by `IsolatedPluginHook` (`crates/zen-plugin/src/hook_isolation.rs:57`) so a hook error denies **that invocation only** and never aborts the round. Hardening: ids must match `^[a-z0-9_-]+$`, reserved namespaces (`fs`, `web`, `system`, `plugin`, `shell`) are rejected, and enable/disable persists to `{plugin-dir}/state.json` (effective next session, no hot-unload); non-Confidential plugin tools are MCP-exposed by default; manifest `sha256` is verified (macOS `.dylib` codesign-checked). The WASM sandbox (`crates/zen-plugin/src/wasm_sandbox.rs`) gates every invoke on permissions, caps memory (`StoreLimits`, default 64 MiB / 5000 ms) and lazily precompiles the `Module` via `OnceLock`; real WASI fs preopens are gated behind TODO-011 (the `[sandbox.wasm]` booleans currently attach no preopens — a spec-only capability). `[agents.tools]` (`crates/zen-agents/src/delegate_tools.rs`) overlays builtin grants with exact / `prefix.*` / `plugin:*` patterns (FR-046). Extension map: `docs/specs/003-agentic-plugin/extension-seams.md`.

**launchd persistence** (macOS) — `zen serve install` writes `~/Library/LaunchAgents/dev.zen.serve.plist` (`RunAtLoad`, `KeepAlive`, 60s throttle); `render_plist()` is pure/unit-tested and `gui_domain()` resolves the UID via `id -u`. Upgrade handling (binary vs running daemon staleness, `[gateway] upgrade_policy`) is documented in §5 (Phase 30 entry).

**Plugin confidentiality** — plugin hooks are invisible to `Confidential` invocations (FR-048); the built-in confidentiality hook runs first and gates them out, so a plugin never observes Confidential content.

**Sandbox CLI surface** — `zen sandbox run|status|policy|test` (`crates/zen-cli/src/cmd/sandbox_command.rs`) exposes the active mode, the effective policy, and an isolation self-check. Sandbox decisions and every rejected invocation are appended to `<logs>/audit.jsonl` (append-only JSONL).

**Health surface** — `zen doctor` runs 9 liveness probes (config, `state.db`, memories, daemon, daemon-version, loop, provider, vault, outbox) with exit 0 all-green / 1 any-fail; the outbox and daemon-version probes surface staged-but-undelivered briefs and stale daemons.

**Tool sensitivity summary**: `shell.exec` is the sole **Confidential** builtin (structured binary+args, env-scrubbed, process-group timeout); `fs.write`/`fs.edit`/`fs.delete`/`fs.move`/`fs.copy`/`web.*`/plugin WASM are Private; read/list/grep/glob and most `system.*` are Public.
### 3.9 Data Layer

Storage philosophy: **markdown is the canonical source of truth** (`~/.zen/vault/`,
Obsidian-compatible, `[[wikilinks]]` + YAML frontmatter); SQLite (`state.db`) holds
**derived indexes only** (FTS5, vec0, graph, sessions, workflow); personal trackers
stay file TOML (`habits.toml`, `goals.toml`, `budgets.toml`, `routines.toml`).

**`SqliteClient`** (`crates/zen-repo/src/client.rs`) is the single unified data layer:
one `tokio_rusqlite::Connection` **writer** (serialized, WAL) + one `sqlx` read
**pool** (`max_connections(4)`); `sqlite_vec::sqlite3_vec_init` registered once via
`sqlite3_auto_extension` at open; `PRAGMA journal_mode=WAL / foreign_keys=ON /
synchronous=NORMAL / busy_timeout=5000`. Migration is guarded by a **cross-process
flock** on `{db}.migrate.lock` because sqlx-sqlite's own `Migrator::lock` is a no-op.

| Repository | Owns | Notes |
|---|---|---|
| `NotesRepo` | `notes_meta` + FTS5 search | `index_note` INSERT OR REPLACE; snippet read from canonical file (T222) |
| `NotionsRepo` | `notions` / `relationships` / `notion_aliases` | BFS, PageRank, personalized PPR, temporal edges, `normalize_alias` |
| `EmbeddingsRepo` | vec0 `note_embeddings` / `notion_embeddings` | KNN similarity search |
| `SelfModelRepo` | `self_nodes` | identity graph |
| `GoalsRepo` | `goal_nodes` / `path_nodes` | goal/path projections |
| `BeliefsRepo` | `belief_nodes` (Bayesian) | prior/posterior projection |
| `DispatchRepo` | `dispatch_tasks` queue | background dispatch |
| `SessionsRepo` | indexed sessions | session metadata |
| `WorkflowRepo` | `workflow_plans` / `workflow_tasks` | plan-DAG checkpoints + `claim_plan` fence |

Each holds `&SqliteClient`, async methods only (Principle XII); satellite repos
`memvid_replay_offset_repo`, `qq_binding_repo`, and the `communities` module follow
the same shape.

Schema highlights (all forward-only additive — Principle XIII):

- `notes_fts USING fts5(id,title,content,tags, content='', tokenize='porter')` —
  **contentless** FTS5 (migration 001) stores no text, so tier-2 snippet text is
  filled from the canonical markdown file (`NotesRepo::search`).
- `note_embeddings USING vec0(note_id TEXT PRIMARY KEY, embedding FLOAT[4096])`
  (migration 002) — 4096-dim max; 384-dim models (nomic/fastembed) zero-padded in
  the application layer (`zen-vault/src/tindy/embeddings.rs`).
- `relationships` gains a validity window `t_valid`/`t_invalid` (migration 008) —
  contradictions **soft-invalidate**, never DELETE; `relationships_as_of(ts)` queries
  point-in-time; `personalized_pagerank(seeds)` (HippoRAG) ranks in the fused tier.
- `notion_communities` / `notion_community_members` (migration 010) — deterministic
  Louvain over open edges, **recompute-replaces**, so the table never grows monotonically.
- `workflow_plans` / `workflow_tasks` (migration 006) + `workflow_plans.owner`
  (migration 007) — plan-DAG persistence; `claim_plan` fences resume (one live resume
  per plan, stale >3600s stealable).
- `note_stages` (migration 009) — durable `(file_path,content_hash)` stage projection
  written after a successful commit; separate from `notes_meta` so re-indexing cannot
  re-open a finished note.
- `memvid_replay_offsets` (migration 005) — per-session jsonl-to-mv2 replay checkpoints
  (byte offset + blake3 idempotency) for the 30s GC incremental replay.

Migration discipline: files in `crates/zen-repo/migrations/` applied by
`sqlx::migrate!()` on every open, tracked in `_sqlx_migrations`, each with a paired
`tests/migration_NNN_*.rs`. Additive only (constant NOT NULL DEFAULT; deprecated
columns stay); FTS trigger changes require `INSERT INTO <fts>(<fts>) VALUES('rebuild')`;
**vec0 has no ALTER**, so destructive change needs backup-recreate-reimport. Runtime
lock files (`gateway.lock`, migration lock, scheduler lease, reward lock) are runtime
state, never schema.

### 3.10 CLI & TUI Surface

Binary/library split: `crates/zen/src/main.rs` (13-line wrapper) loads `.env` +
config then calls `zen_cli::shell()`; the library's `Cli` clap `Parser` dispatches
`Commands` (23 variants) in `crates/zen-cli/src/cli.rs` to per-command
`execute_command()` functions in `crates/zen-cli/src/cmd/`.

| Command | Purpose | Dispatch file |
|---|---|---|
| `chat` | Interactive LLM chat | `chat_command.rs` |
| `clean` | Cleanup (all/trash gated by y/N; `--yes` bypasses) | `cleanup_command.rs` |
| `starter` | Dev tools / workspace init | `starter_command.rs` |
| `wps` | Work-process utilities | `wps_command.rs` |
| `version` | Show version | `cli.rs` (inline) |
| `session` | Session lifecycle | `session_command.rs` |
| `serve` | Gateway daemon (start/status/stop/restart/mcp/install/uninstall) | `serve_command.rs` |
| `agent` | Agent registry | `agent_command.rs` |
| `workspace` | `.zen/` structure | `workspace_command.rs` |
| `config` | Config layers | `config_command.rs` |
| `provider` | LLM provider management | `provider_command.rs` |
| `audit` | Audit log operations | `audit_command.rs` |
| `logs` | Structured log viewer | `logs_command.rs` |
| `wiki` | Wiki ops (list/show/reindex/lint/distill/rebuild-memory/loop/export-skill/rollback) | `wiki_command.rs` |
| `model` | Model metadata + routing | `model_command.rs` |
| `plugin` | Plugin management | `plugin_command.rs` |
| `auth` | Auth / keychain ops | `auth_command.rs` |
| `habit` | Habit tracking | `habit_command.rs` |
| `goal` | Goal management | `goal_command.rs` |
| `skill` | Skill management | `skill_command.rs` |
| `discover` | Self-learning gate surface (run/stage/confirm/report/arena) | `discover_command.rs` |
| `sandbox` | OS-level sandbox surface (run/status/policy/test) | `sandbox_command.rs` |
| `doctor` | System health (9 liveness probes, `--json`) | `doctor_command.rs` |

Nine manual commands were removed from the CLI surface (2026-08-28): `note`, `search`,
`similar`, `notion`/`graph`, `research`, `ingest`, `routine`, `brief`, `dispatch` —
capabilities live via ZenScheduler workers and the distill loop; files stay on disk.

**TUI** (`crates/zen-cli/src/tui/`, ratatui + crossterm). Inline stream mode is the
default; the fullscreen path is via `ZEN_TUI_FULLSCREEN`. Finalized history goes to
**native terminal scrollback** (mouse-selectable); only the tail, input, footer, and
pickers redraw in a fixed-height bottom viewport. Key surfaces:

- `StreamCollector` splits committed blocks from the pending tail; tool activity
  renders as collapsible one-line blocks with `/tools` toggling expansion.
- Slash palette is Codex-parity (registry-order groups, dimmed description column,
  match highlight, exact>prefix hoist, `›` glyph) plus `DismissedToken` memory.
- `Shift+Enter` inserts a newline (Ctrl+Enter / Ctrl+J fallbacks); `Enter` submits;
  `diff`/`patch` fences render explicit red/green deltas at the markdown fence layer.
- Inter-token gaps >200 ms append `tui.stream.stall` audit episodes (surfaced in
  `zen discover report`); keystroke-to-echo measured ~7 ms.

**First-run experience**: `zen` initializes `~/.zen` (vault, state.db, memories, config)
and paints a welcome banner; `zen doctor` runs 9 liveness probes (config, state.db,
memories, daemon, daemon-version, loop, provider, vault, outbox) with JSON/human output.

**Gateway lifecycle**: a scheduler probe gate (`tui/scheduler_gate.rs` + `prewarm.rs`)
spawns the in-app scheduler only when no daemon-hosted scheduler is live; the daemon is
spawned on demand and states (spawning / degraded / reconnect / version-mismatch) render
in the banner slot. Stale-daemon detection (`[gateway] upgrade_policy`, default `warn`) surfaces a hint; `auto-restart` may drive one drain-restart (§5, Phase 30 entry).

## 4. Cross-Cutting Design Principles

- **Local-first.** Private and Confidential data stays on local models unless a path
  is explicitly allowed to reach the cloud (`allow_cloud`, `distill_allow_cloud`).
- **Gates-closed defaults.** New features land disabled; no uncalibrated threshold
  ships (V13-A.3). A threshold is honored only from
  `logs/decision-audit/thresholds.json` (Clopper-Pearson certified) or an explicit
  override — absent that, pre-gate behavior is preserved (T138/T168/T192).
- **Report-only eviction.** Retention and memory-strength surfaces quarantine or list
  candidates; they never delete user data.
- **Fail-open vs fail-closed.** Availability paths (retrieval, lint, injection) fail
  open so a turn never wedges; sensitivity and security paths (confidentiality hook,
  sandbox validator, network policy) fail closed.
- **Single-writer ownership.** The memvid store has one RW owner (the gateway daemon);
  `placeholders.json` is read-merge-write under flock; the belief surface is
  markdown-owned with one writer, so second writers are structural violations.
- **Additive-only migrations + paired tests.** Schema evolves forward-only with a
  paired migration test each; deprecated columns and tables stay (Principle XIII).
- **Byte-identity pin discipline.** A feature behind a closed gate is pinned
  byte-identical to its absence (e.g. `strength_floor=0.0`, `search_rerank` off), so
  flipping the gate is the only behavior change.
- **Reuse-over-novelty.** Constitution XI: reuse `rig-compose`, `memvid-core`,
  `rig-core` via thin zen-owned domain layers rather than reinventing engines.
- **Observability.** Structured `tracing` plus append-only `logs/audit.jsonl` lines;
  conventional exit codes; JSON/human dual output on every CLI surface.
- **Documentation discipline.** Principle XV: scope-logic blocks, public-API docs,
  and same-PR AGENTS.md sync, enforced by docs-consistency rot gates.
## 5. Design Decision Registry

Each entry: decision (one line) | code anchor | status. Entries marked **[consolidated]** were absorbed from per-topic design docs removed 2026-10-13 — their detail lives in §3 plus these entries; the surviving `../specs/` records remain historical.
Status values reflect landed reality; "dormant-off" = implemented but shipped with its gate closed by default.

### doc-conversion — all-format document ingestion (T2/T3/T4) **[consolidated]** — all-format document ingestion (T2/T3/T4)

| Decision | Code anchor | Status |
|---|---|---|
| PDF via in-process `pdf-extract 0.10` pinned to memvid-core's minor (zero new version island), `catch_unwind`-contained | `crates/zen-vault/src/ingest/convert.rs::pdf_to_markdown` | landed (`1e8d73c`) |
| Office (docx/xlsx/pptx) via `office_oxide` after mini-audit GO; legacy binary .doc/.xls/.ppt rejected | `crates/zen-vault/src/ingest/convert.rs::office_to_markdown` | landed (`c5d8123`) |
| Long-tail formats (epub/odt/rtf) via `$PATH` pandoc sidecar; structured argv, 30s deadline; `Unavailable` stays staged, `Failed` quarantines | `crates/zen-vault/src/ingest/convert.rs::pandoc_to_markdown` | landed (`172f2bb`) |
| Promote path reuses the single `is_convertible_extension` predicate (staging/dispatch drift structurally impossible) | `crates/zen-vault/src/ingest/convert.rs` + `source_ingest.rs::promote_incoming` | landed (2026-10-01 fix) |
| Dispatcher `convert_to_markdown`/`is_convertible_extension`; plain-text path stays byte-identical (no lossy UTF-8) | `crates/zen-vault/src/ingest/convert.rs` | landed |

### e1-semantic-lint-writers — E1 second-writer analysis **[consolidated]** — E1 second-writer analysis

| Decision | Code anchor | Status |
|---|---|---|
| Semantic lint is READ-ONLY over wiki pages; findings flow through the existing lint report writer (E2 invariant preserved, no new wiki surface) | `crates/zen-vault/src/tindy/semantic_lint.rs` -> `reports/lint-*.md` | landed (`ecf42aa`) |
| Gate ships closed: `[agentic.lint] semantic` default false, env `ZEN_LINT_SEMANTIC` (T168 shadow-mode precedent) | `crates/zen-core/src/config.rs` | dormant-off by design |
| Bounded input (40 pages x 2000 chars), spawn_blocking guard, fail-open on unconfigured/unreachable/garbled output | `crates/zen-vault/src/tindy/semantic_lint.rs` | landed |
| Writer inventory is a point-in-time snapshot (20 entries); the read-only decision is the load-bearing output | this record | count corrected 2026-10-02 |

### gateway-upgrade-management — Phase 30, T201-T205 **[consolidated]** — Phase 30, T201-T205

| Decision | Code anchor | Status |
|---|---|---|
| G1 pid record carries `version`/`protocol`; `zen serve status` shows Binary vs Daemon + STALE marker | `crates/zen-gateway/src/daemon.rs`; `crates/zen-cli/src/cmd/serve_command.rs` | landed (T201) |
| G2 `zen serve restart` (launchd kickstart / graceful drain + start); G3 third doctor probe `daemon-version` | `crates/zen-cli/src/cmd/serve_command.rs`; `doctor_command.rs` | landed (T202-T203) |
| `[gateway] upgrade_policy = warn\|auto-restart` (D-A: default `warn`, gate-closed; invalid value warns, never silent) | `crates/zen-core/src/config.rs` | landed (T203) |
| D-C daemon binary shares the CLI's single binary (deliberately unlike codex's package decoupling) | `crates/zen-cli/src/cmd/serve_command.rs` | accepted |
| G4 migration-race audit PASS (zero `SELECT *`, one fixed positional projection); G5 contract addendum | `crates/zen-repo/src/notions_repo.rs:1050`; `../specs/004-agentic-gateway/contracts/00-protocol-overview.md` | landed (T204-T205) |
| G6 independent updater + package decoupling deferred (D-B; revisit trigger = distribution channel change) | -- | deferred |

### memory-strength-knowledge-wave — Phase 31 waves + Phase 32 fixes (largest) **[consolidated]** — Phase 31 waves + Phase 32 fixes (largest)

Owner decisions: **D-E** skill-root precedence (zen-native first), **D-F** knowledge inject default **ON**, **D-G** one `search_rerank` flag guards both consumers, **D-H** fan-out hard cap 8.

| Decision | Code anchor | Status |
|---|---|---|
| Pure-function Ebbinghaus strength `R = 0.5^(t/(HL*(1+log2(1+n))))`; read-time derived, zero storage/migration | `crates/zen-memory/src/strength.rs` | landed (T206) |
| `memory_strength.enabled` + `search_rerank` gates default **off**; byte-identity pins; `strength_floor` default 0.0 (clamp, env `ZEN_MEMORY_STRENGTH_FLOOR`) (T224) | `crates/zen-core/src/config.rs`; `crates/zen-memory/src/strength.rs` | dormant-off (preconditions met) |
| Indexer additive `source_path`/`tier` provenance via `with_provenance_metadata`; consumer contract uses key presence, never map-empty | `crates/zen-memory/src/memvid_index.rs` | landed (T208) |
| D-F knowledge inject default ON (owner revision): empty-knowledge precondition, Conversation skip, 4000-char budget, 2000ms fail-open timeout, `loop.knowledge.injected` audit | `crates/zen-agents/src/orchestrator.rs` | landed, gate default ON (T214) |
| Strength consumers: gateway `memory/search` sort + additive `strength`; `select_cards_with_strength`; enriched-path reward symmetry (T221) | `crates/zen-gateway/src/server/memory.rs`; `crates/zen-agents/src/zen_agent.rs` | landed, gated off |
| Nightly report-only bottom-N (`logs/memory-strength-report.json`) + `loop.memory.strength` audit; never deletes (retention quarantine precedent) | `crates/zen-agents/src/scheduler/workers/memvid_indexer_worker.rs` | landed, gated off |
| D-E SkillLoader multi-root (`~/.zen/skills` > project `.agents/skills` > `~/.agents/skills`); `export-skill --target zen\|agents` | `crates/zen-agents/src/skill_loader.rs`; `crates/zen-vault/src/wiki/skill_export.rs` | landed (T212/T213) |
| D-H fan-out hard cap 8 in `parse_requests` (structured model-readable rejection) | `crates/zen-agents/src/delegate_task.rs` | landed (T216) |
| Sub-agent context inheritance `[agentic.delegate] context_inheritance` (default false; single `compose_sub_session` seam, depth-halved budget) | `crates/zen-agents/src/delegate_task.rs` | landed, gate default off (T215) |
| T219 nightly index fix (`index_incremental`, dedup(true) rejected), T220 substantive-knowledge predicate, T221 enriched reward symmetry, T222 canonical-file tier-2 snippet, T223 rebuild provenance, T224 strength_floor | `memvid_indexer_worker.rs`; `orchestrator.rs`; `notes_repo.rs`; `daemon.rs`; `strength.rs` | landed Phase 32 (2026-10-11) |

### openkb-compile-hygiene — Phase 1-4 (items 1-6 + E-series) **[consolidated]** — Phase 1-4 (items 1-6 + E-series)

| Decision | Code anchor | Status |
|---|---|---|
| (1) OKF conformance lint (`[agentic.compile] okf_lint` default true) | `crates/zen-vault/src/tindy/lint.rs` | landed (`0ac2a8f`) |
| (2) Wikilink whitelist + ghost-strip (NFC, `ghostlink_enforcement` default strip) | `crates/zen-vault/src/tindy/wiki_compile.rs` | landed (`61b98a6`) |
| (3) Prompt-cache breakpoints (`[agentic.cache] breakpoints` default true, anthropic-only) | `crates/zen-provider/src/cache.rs` | landed (`2785d9d`) |
| (4) Crash replay of stale `.txn-*.jsonl` on `run_scoped` entry | `crates/zen-vault/src/distill/recovery.rs` | landed (`b158003`) |
| (5) Skill trigger eval (report-only dual-metric, zero labels) | `crates/zen-agents/src/skill_trigger_eval.rs` | landed (`6cb448e`) |
| (6) Tree index for long docs (`[agentic.loop] tree_index_pages` default 20) | `crates/zen-vault/src/distill/tree_index.rs` | landed with T046b (`a9855aa`) |
| E1 Semantic lint (opt-in, gate closed) | `crates/zen-vault/src/tindy/semantic_lint.rs` | dormant-off (`ecf42aa`) |
| E2 Page-level `sources:` provenance (code-managed, LLM never writes) | `crates/zen-vault/src/tindy/wiki_compile.rs` | landed (`3d55a36`) |
| E3 Delete cascade (`redirect_page_source` + `sweep_dead_sources`, two-round grace) | `crates/zen-vault/src/distill/cascade.rs` | landed (`7bf3dc4`) |
| E5 Page versioning + `zen wiki rollback`; E7 SKILL.md export | `crates/zen-vault/src/wiki/iterations.rs`; `skill_export.rs` | landed Phase 4 (`e8ef9e2`/`697daee`) |
| Config surface: `[agentic.compile] ghostlink_enforcement`/`okf_lint`, `[agentic.cache] breakpoints`, `[agentic.loop] tree_index_pages`, `[agentic.lint] semantic` (all 5-layer + env) | `crates/zen-core/src/config.rs`; `config/config.toml` | landed |

### search-mode-defaults — T138 decision memo **[consolidated]** — T138 decision memo

| Decision | Code anchor | Status |
|---|---|---|
| T138 option A adopted: add `ZEN_TUI_KNOWLEDGE_SEARCH` (fast\|full\|off) parity; default stays Fast. Options C (fused agent tool) and B (flip default) remain open follow-ups | `crates/zen-core/src/config.rs::apply_tui_env` | landed (2026-09-30) |
| Premise correction: `KnowledgeSearchMode` gates exactly ONE call site (TUI chat injection); `zen chat` and TUI `/search` already pass `tiers=None` and fuse | `crates/zen-cli/src/tui/chat.rs`; `chat_command.rs` | verified 2026-09-30 |

### 005 calibrated decision layer (T168-T174)

| Decision | Code anchor | Status |
|---|---|---|
| Rung substrate: embedding router -> local classifier -> LLM; thresholds only from calibration artifact (`logs/decision-audit/thresholds.json`) or explicit config; `None` = gate closed (byte-identical pre-T168) | `crates/zen-agents/src/decision/mod.rs` | landed, gates closed absent calibration |
| De-anchored judge (local commits before seeing the draft); binary classifiers `classify_binary`; correction/citation emitters | `crates/zen-agents/src/review/deanchored.rs` | landed, τ-gated |
| `zen discover calibrate` (report-only unless `--write`); Clopper-Pearson + Bonferroni over a fixed K=8 grid | `crates/zen-vault/src/distill/decision_audit.rs` | landed (awaiting labels) |

### 006 orchestration config surface

| Decision | Code anchor | Status |
|---|---|---|
| `[agentic.delegate] max_depth` (default 1, clamp 1..=3, env), `max_concurrent` (1..=8), `context_inheritance`; `[agentic.orchestrator] surface` full\|delegation-only; `FANOUT_MAX_TASKS=8` | `crates/zen-agents/src/delegate_task.rs`; `crates/zen-core/src/config.rs` | landed (T373/T374/T378/T216) |
| `plan.execute` Sisyphus-only plan-DAG (Kahn layering, per-task checkpoints, claim-fenced resume) | `crates/zen-agents/src/plan_task.rs` | landed (T375/T376) |

### Specification and ADR records

| Decision | Code anchor / link | Status |
|---|---|---|
| 001 DESIGN.md: M0-M5 six-tier memory model is the canonical implementation model | [`../specs/001-agentic-foundation/DESIGN.md`](../specs/001-agentic-foundation/DESIGN.md) | canonical |
| 001 memory-architecture: FR-MEM-001..004 layered memory + session context | [`../specs/001-agentic-foundation/memory-architecture/spec.md`](../specs/001-agentic-foundation/memory-architecture/spec.md) | Phase 1A/1B/2 landed, Phase 3 partial |
| 001 data-model (Draft) — superseded in practice by the code + later migrations | [`../specs/001-agentic-foundation/data-model.md`](../specs/001-agentic-foundation/data-model.md) | Draft (code ahead of doc) |
| 002 spec + TUI ADRs: scroll-position / bottom-pane / slash-popup (Superseded) / gateway sole-owner / inline-stream / fixed viewport | [`../specs/002-agentic-tui/spec.md`](../specs/002-agentic-tui/spec.md); [`adr/`](../specs/002-agentic-tui/adr/) | Implemented (amended 2026-09-21) |
| 003 extension-seams: plugin trait/`PluginApi`, 5-hook append point, config-driven grants | [`../specs/003-agentic-plugin/extension-seams.md`](../specs/003-agentic-plugin/extension-seams.md) | landed (FR-032..051) |
| 004 design.md: gateway daemon API, transport layering, single contract source of truth; contracts 00-05 (protocol overview, naming, method catalog, CLI/TUI mapping, qqbot carrier) | [`../specs/004-agentic-gateway/design.md`](../specs/004-agentic-gateway/design.md); [`contracts/`](../specs/004-agentic-gateway/contracts/) | implemented through Phase 13.1 |
| 005 coverage dispositions + archived task ledger | [`../specs/005-agentic-loop/tasks.md`](../specs/005-agentic-loop/tasks.md); [`tasks-archive-2026-09-12.md`](../specs/005-agentic-loop/tasks-archive-2026-09-12.md) | Delivered (partial dispositions recorded) |
| 006 ADR-001: blackboard superseded by direct-return delegation (`blackboard.rs` deleted) | [`../specs/006-agentic-orchestration/adr-001-blackboard-supersession.md`](../specs/006-agentic-orchestration/adr-001-blackboard-supersession.md) | Accepted (2026-09-07) |
| 006 spec Convergence Record C1-C6: 001 vision convergence, PD-01 loop, Phase-31 hardening, D1 guard, quality-gate refinements, dormant L1 probe activation | [`../specs/006-agentic-orchestration/spec.md`](../specs/006-agentic-orchestration/spec.md) | Implemented (converged 2026-10-13) |

## 6. Architecture Evolution Timeline

- **2026-06** — 001 foundation: agentic architecture design; remediation splits the conflated `Sensitivity`/`SafetyLevel`/`AgentClearance` enums and unifies `LlmPreference`.
- **2026-07** — CLI consolidation (Occam): `task` stub deleted, `reindex`/`lint`/`distill` folded into `zen wiki`; 33 -> 29 commands.
- **2026-08** — 002 agentic TUI (codex-style inline stream) + 003 agentic plugin (WASM sandbox, MCP) + 004 agentic gateway (sole-owner UDS/JSON-RPC daemon). ADR-004 locks the single-writer model.
- **2026-08-28** — Personal-agent scope trim: 9 manual commands removed (29 -> 20); capabilities survive internally via scheduler workers and the distill loop.
- **2026-09 (early)** — 005 agentic loop: RLVR reward sidecars, memory/RSI, in-app scheduler + cross-process lease; PD-01 AgentRun state-machine migration of the tool loop.
- **2026-09-07** — 006 orchestration fusion lands (real-LLM `delegate.task` sub-turn, QualityPipeline wired into `execute`, blackboard deleted per ADR-001).
- **2026-09-18** — RLVR/RSI feedback loops wired (correction increments, loss-aversion boost, wake-up brief); scheduler role lease + reward-sidecar flock hardening.
- **2026-09-19/20** — Phase 25-27 review fixes (belief decay idempotency, T129/T130 provenance-gated beliefs, L1 decision-layer substrate T168-T174) + multi-instance concurrency hardening (`StartupLock`, migration lock, `write_atomic`).
- **2026-09-20** — Phase 28 retention subsystem T190-T194 (audit/log rotation, age-deletes, `zen clean` confirmation gate).
- **2026-09-27..29** — Compile-hygiene phases 1-4: OKF lint, ghostlink whitelist, prompt-cache breakpoints, crash replay, sources provenance, delete cascade, skill eval, semantic lint, tree index, page versioning, SKILL.md export.
- **2026-09-29** — All-format document ingestion T2/T3/T4 (PDF, Office, pandoc sidecar).
- **2026-09-30** — Eng-review remediation: docs-rot gate, NFC alias backfill, real distill/provider cost visibility, T138 option A env parity.
- **2026-10-01** — Design-verification audit: pandoc sidecar reachability fix + OKF lint premise fix (compiler made compliant).
- **2026-10-10** — Phase 30 gateway upgrade management + workspace-path re-root fix T195 (`MemvidIndexer` Path Spec v2).
- **2026-10-11** — Phase 31 memory-strength x knowledge wave 1-3 (T206-T218) + Phase 32 review fixes T219-T224 (index/inject/rerank correctness).
- **2026-10-13** — D1-class async routing guard (`OllamaProvider::health_check` dedicated-threaded) + dormant L1 probe activation (C6); 006 spec converged to Implemented (3115 tests).

## 7. Source Index

| File | Contents |
|---|---|
| `docs/designs/doc-conversion.md` *(consolidated here; removed 2026-10-13)* | All-format -> Markdown ingestion design (PDF/Office/pandoc tiers, dependency audit) |
| `docs/designs/e1-semantic-lint-writers.md` *(consolidated here; removed 2026-10-13)* | Grep-verified wiki writer inventory + read-only semantic-lint decision |
| `docs/designs/gateway-upgrade-management.md` *(consolidated here; removed 2026-10-13)* | Phase 30 stale-daemon detection, restart, upgrade policy, migration-race audit |
| `docs/designs/memory-strength-knowledge-wave.md` *(consolidated here; removed 2026-10-13)* | Phase 31/32 retention strength, knowledge inject, sub-agent inheritance, chain hardening |
| `docs/designs/openkb-compile-hygiene.md` *(consolidated here; removed 2026-10-13)* | OpenKB-derived compile hygiene program (OKF lint .. SKILL export, E-series) |
| `docs/designs/search-mode-defaults.md` *(consolidated here; removed 2026-10-13)* | T138 decision memo: TUI knowledge-search mode surface map + env parity fix |
| `docs/specs/001-agentic-foundation/spec.md` | Original personal-agentic feature spec (FRs; Draft status, code ahead) |
| `docs/specs/001-agentic-foundation/architecture.md` | Agentic architecture design (layers, module map) |
| `docs/specs/001-agentic-foundation/DESIGN.md` | Canonical M0-M5 memory/learning/evolution/decision design |
| `docs/specs/001-agentic-foundation/data-model.md` | Early data model (Draft; superseded in practice by migrations) |
| `docs/specs/001-agentic-foundation/memory-architecture/` | Layered memory + session context sub-spec (`spec.md`/`plan.md`/`tasks.md`) |
| `docs/specs/001-agentic-foundation/V8.md` | V13 vision finalization (agentic orchestration positioning, PD-01 decision) |
| `docs/specs/002-agentic-tui/spec.md` + `adr/` | TUI inline-stream spec; 6 MADR ADRs (scroll, bottom-pane, slash-popup, gateway, inline UX, viewport) |
| `docs/specs/003-agentic-plugin/spec.md` + `extension-seams.md` | Plugin/tool/MCP spec; developer extension map (trait, hooks, grants) |
| `docs/specs/004-agentic-gateway/spec.md` + `design.md` + `contracts/` | Gateway daemon spec; final design; frozen protocol contracts 00-05 |
| `docs/specs/005-agentic-loop/spec.md` + `tasks.md` + `data-model.md` | Wiki processing loop spec, task ledger (archived Phases 1-19), data model; review artifacts `review-2026-10-11-phase31.md`, `plan-ceo-review-*.md` |
| `docs/specs/006-agentic-orchestration/spec.md` + `adr-001-blackboard-supersession.md` | Orchestration fusion spec (Convergence Record C1-C6) + blackboard supersession ADR |
| `AGENTS.md` | Landed-truth ledger: crate map, conventions, tool inventory, Recent Changes timeline |
| `TODOS.md` | Review backlog ledger: resolved/open/deferred items with ledger pointers |
