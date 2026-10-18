# PROJECT KNOWLEDGE BASE

**Generated:** 2026-06-01
**Commit:** workspace
**Branch:** main

## OVERVIEW

Zen is a Rust CLI productivity suite with agentic workspace architecture. Edition 2024, 12 workspace crates, knowledge-base-first design with multi-provider LLM routing, vector search, and session management.

The system follows a binary/library split: `zen` (thin binary wrapper) delegates to `zen-cli` (the library), which orchestrates domain crates.

## PROJECT CONSTITUTION

**Reference:** `.specify/memory/constitution.md` — All development MUST adhere to these principles:

### Core Principles Summary

| Principle | Enforcement | Impact on Development |
|-----------|-------------|----------------------|
| **I. CLI-First** | Every feature via CLI subcommands | All agentic features exposed as `zen <subcommand>` |
| **II. Robust Error Handling** | `thiserror` for types, `anyhow` for propagation | `ZenError` + `AgenticError` taxonomy |
| **III. Observability** | Structured logging via `tracing` | Spans for agent execution, LLM calls |
| **IV. Configuration** | `.env` via `dotenvy` | 5-layer config inheritance |
| **V. Template-Driven** | Embedded templates via `include_dir` | `tera` for scaffold generation |
| **VI. Code Quality** | `cargo clippy` + `cargo fmt` mandatory | Zero warnings, `unsafe` blocks justified |
| **VII. Architecture** | Single responsibility, stable interfaces | 12 crates with clear boundaries |
| **VIII. Testing** | Unit + integration tests required | ZenTest harness for CLI end-to-end |
| **IX. UX Consistency** | Consistent output, conventional exit codes | JSON/human-readable dual output |
| **X. Performance** | <500ms cold start, <50MB footprint | Async I/O for blocking ops |
| **XI. Design-First & Reuse** | **MANDATORY**: Design before coding, reuse frameworks | **Prohibited**: Custom impl when library exists |
| **XV. Code Documentation** | Code blocks + scope logic MUST be documented | AGENTS.md sync required |

### XI. Design-First & Reuse Priority (Critical for Agentic)

**All agentic implementation MUST follow this sequence**:
1. **Design before coding** — No implementation without documented design decisions
2. **Reuse existing frameworks** — Search community best practices before custom solutions
3. **Avoid reinventing the wheel** — Use established libraries, patterns, and frameworks
4. **Simplicity over novelty** — Prefer proven solutions over clever implementations

**Enforcement**:
- Every PR MUST document design rationale (why this approach, alternatives considered)
- Custom implementations MUST justify why existing solutions insufficient
- Framework/library selection MUST reference community adoption metrics
- "Simple reuse" is default; "Custom implementation" requires explicit approval

**Prohibited patterns**:
- Implementing from scratch when well-maintained library exists (e.g., custom orchestrator when `rig-compose` provides primitives)
- Creating new abstractions without searching existing patterns
- Preferring novel solutions without documented advantages
- Skipping design phase and jumping to implementation

**Reference Systems** (reuse patterns from):
- `rig-compose` for agent orchestration (GenericAgent, CoordinatorAgent, DelegateTool)
- `memvid-core` for context management (engine only; the store wrapper `zen-memory::memvid_store::MemvidStore` is zen-native)
- `rig-model-meta` for model abstraction (traits, telemetry)
- Claude Code's system prompt assembly (18-section architecture, cache boundary)
- LangChain's ChatPromptTemplate (role-separated messages)
- Semantic Kernel's IPromptTemplateFactory (template factory pattern)

### Technology Stack

- **Language**: Rust (edition 2024)
- **CLI Framework**: clap 4.5
- **Logging**: tracing with env-filter
- **Database**: SQLite (FTS5 + sqlite-vec for agentic module)
- **Error Handling**: thiserror + anyhow
- **Agent Orchestration**: rig-compose 0.5
- **LLM Abstraction**: rig-core 0.42 (+ rig-agent 0.42 classic runtime — AgentRun state machine, PD-01 B target)
- **Vector Store**: sqlite-vec + rig-sqlite 0.42
- **Template Engine**: tera + include_dir
- **Configuration**: dotenvy + 5-layer inheritance

See `.specify/memory/constitution.md` for full principles, rationale, and governance process.

## AGENTIC ARCHITECTURE

Zen routes operations through a layered agentic pipeline: notes -- consolidation -- knowledge graph -- search. Each crate handles one concern.

### Crates

| Crate | Role | Path |
|-------|------|------|
| zen | Binary entry (13-line wrapper) | `crates/zen/` |
| zen-cli | CLI library (23 commands, TUI, dispatch) | `crates/zen-cli/` |
| zen-core | Config layers, error taxonomy, path scoping, constants (13 modules) | `crates/zen-core/` |
| zen-service | Starter/wps/cleanup business logic | `crates/zen-service/` |
| zen-repo | Unified data layer: SqliteClient + 9 domain repositories (FTS5, vec0, graph) | `crates/zen-repo/` |
| zen-vault | 10+ services: note, wiki, search, distill, tindy, notion, ingest, graph_verify, intent | `crates/zen-vault/` |
| zen-agents | 13 agents, 4-tier registry, QualityPipeline, plan-DAG executor | `crates/zen-agents/` |
| zen-provider | 13 providers, 3 protocol types, DefaultRouter factory, auth resolution | `crates/zen-provider/` |
| zen-auth | Keychain + SecretRef resolution | `crates/zen-auth/` |
| zen-plugin | Agent tools, WASM sandbox (wasmtime), MCP client, plugin registry | `crates/zen-plugin/` |
| zen-gateway | Sole-owner UDS daemon: JSON-RPC 2.0 method registry, hosted agent sessions (US4), guards/approvals, HTTP `/api/v1` + MCP stdio, QQBot channel (`channel/qqbot/`: WS gateway + HTTP bridge) | `crates/zen-gateway/` |

### Dependency Graph

```
zen (binary)            -- 13-line main.rs -- loads .env, config, calls zen_cli::shell()
 └── zen-cli (library)  -- clap Parser, TUI (ratatui), command dispatcher
      ├── zen-service   → zen-core
      ├── zen-gateway   → zen-core
      ├── zen-vault → zen-repo   → zen-core
      │                 └── zen-provider → zen-auth → zen-core
      ├── zen-agents    → zen-memory → zen-core
      │                 └── zen-provider
      │                 └── zen-vault
      │                 └── zen-repo (scheduler workers open state.db directly — pre-existing dep)
      ├── zen-core      (13 public modules: audit, config, constants, definition,
      │                 errors, paths, platform, review, sandbox, sanitize,
      │                 secrets, types, validate)
      └── (direct deps: clap 4.5, ratatui, crossterm, tracing, uuid, chrono, serde)

zen-plugin → zen-core
zen-auth   → zen-core
zen-provider → zen-core
```

### Binary/Library Split

- **Binary**: `crates/zen/src/main.rs` (13 lines) -- loads `.env`, calls `zen_core::config::load_config()`, then `zen_cli::shell().await`
- **Library**: `crates/zen-cli/` -- exports `shell()` via `lib.rs`, contains clap Parser, TUI runner, 20 subcommand dispatchers
- **TUI**: Runs on main thread (ratatui/crossterm) when `cli.command.is_none()`

### Data Flow

```
zen wiki distill → zen-vault (DistillationPipeline) → notion extraction → wiki compile → archive
zen wiki loop run → ZenLoopWorker → budget gate → distill (normalize/txn/checkpoint) → ORAV self-correction → graph verify → hypothesis incubation → wisdom hooks → report/gaps/git
zen wiki rebuild-memory → RPC to zen-gateway daemon → MemvidIndexer full reindex → replay from jsonl
zen session start → zen-agents (AgentOrchestrator: INTENT_SIGNALS route → tool loop → quality gate) → executor
zen wiki reindex → zen-vault (Reindexer, checksums, embeddings)
zen wiki lint    → zen-vault (Linter, orphan pages, broken wikilinks)
zen serve start  → zen-gateway daemon + ZenScheduler (cron workers: journal, dream, wisdom, subconscious)
```

## STRUCTURE

```
zenspace/
├── crates/                     # 12 workspace crates (binary/library split)
│   ├── zen/                    # Binary entry (13-line wrapper)
│   ├── zen-cli/                # CLI library: 23 commands, TUI, clap derive
│   │   ├── src/
│   │   │   ├── lib.rs          # pub use cli::shell
│   │   │   ├── cli.rs          # clap Parser/Subcommand (20 variants), shell() dispatcher
│   │   │   ├── tui/            # ratatui TUI interface
│   │   │   ├── session.rs      # Session helpers
│   │   │   ├── sandbox.rs      # Sandbox helpers
│   │   │   └── cmd/            # 34 *_command.rs dispatchers + mod.rs
│   │   └── tests/              # Integration tests (ZenTest harness)
│   ├── zen-core/               # Core infrastructure (13 public modules)
│   │   └── src/
│   │       ├── config.rs       # 4-layer config (Default/embedded/global/env)
│   │       ├── errors.rs       # ZenError (8 variants), AgenticError (20+ variants)
│   │       ├── paths.rs        # ZenPaths (global_root + workspace_root dual-scope)
│   │       ├── constants.rs    # Directory constants + 13 provider URLs/models
│   │       ├── secrets.rs      # SecretRef (keychain/env resolution)
│   │       ├── types.rs        # Shared types (Sensitivity, AgentTier)
│   │       ├── validate.rs     # Input validation
│   │       ├── sanitize.rs     # Output sanitization
│   │       └── definition.rs   # AgentDefinition
│   ├── zen-service/            # Business logic (starter, wps, cleanup)
│   ├── zen-repo/               # Unified data layer (SqliteClient + 9 domain repos)
│   │   ├── migrations/         # sqlx::migrate!() SQL files (001_initial, 002_vec, 003_entity_graph)
│   │   └── src/
│   │       ├── client.rs       # SqliteClient (tokio_rusqlite writer + sqlx pool), run_migrations()
│   │       ├── types.rs        # Row types + request structs (IndexNoteRequest, InsertRelationshipRequest, etc.)
│   │       ├── traits/         # Repository trait interfaces (NotionsRepository)
│   │       ├── notes_repo.rs   # NotesRepo (FTS5 search + index_note)
│   │       ├── notions_repo.rs # NotionsRepo (entities, relationships, aliases, BFS, PageRank, shortest-path)
│   │       ├── embeddings_repo.rs # EmbeddingsRepo (vec0 INSERT + similarity search)
│   │       ├── self_model_repo.rs # SelfModelRepo (self_nodes)
│   │       ├── goals_repo.rs   # GoalsRepo (goal_nodes, path_nodes)
│   │       ├── beliefs_repo.rs # BeliefsRepo (Bayesian belief_nodes)
│   │       ├── dispatch_repo.rs # DispatchRepo (dispatch_tasks queue)
│   │       └── sessions_repo.rs # SessionsRepo (indexed sessions)
│   ├── zen-vault/          # 10+ knowledge services
│   │   └── src/
│   │       ├── note.rs         # Note, NoteService, frontmatter parsing
│   │       ├── wiki.rs         # WikiPage, WikiIndex, AtomicWikiWriter
│   │       ├── wiki/           # AtomicWikiWriter implementation
│   │       ├── search/         # 5-tier search (ripgrep → FTS5 → vec0 → graph → LLM)
│   │       ├── distill/        # DistillationPipeline (extract → compile → merge → archive; transaction/checkpoint/recovery)
│   │       ├── notion/         # NotionService: DB-backed entity graph via NotionsRepo
│   │       ├── tindy/          # Linter, LearningLoop, WikiCompiler skill, Reindexer, embeddings, checksums
│   │       ├── ingest/         # FeedEntry, RssFetcher, ingest_local_file
│   │       ├── graph_router.rs # Graph query router
│   │       ├── graph_verify.rs # Graph integrity verification
│   │       ├── notion.rs       # NotionService entry point
│   │       └── intent.rs       # Intent detection
│   ├── zen-agents/             # Agent system (13 agents, 4 tiers)
│   │   └── src/
│   │       ├── registry.rs     # AgentRegistry, DefaultAgentRegistry
│   │       ├── agent_profile.rs # Profile, Role, SensitivityLevel, LlmPreference
│   │       ├── zen_agent.rs    # ZenAgent, IdentityContext, load_identity_files
│   │       ├── orchestrator.rs # AgentOrchestrator (INTENT_SIGNALS routing, delegate.task, quality gate)
│   │       ├── delegate_task.rs # DelegateTaskTool (006: real-LLM sub-turn delegation, depth-1 guard)
│   │       ├── executor.rs     # AgentExecutor, RetryPolicy, ErrorCategory
│   │       ├── execution.rs    # AgentExecution, ToolCall
│   │       ├── review.rs       # QualityPipeline (Metis→Momus→Hermes→Zeus)
│   │       ├── sandbox.rs      # WasmSandbox (wasmtime), ResourceLimits
│   │       ├── scheduler/     # ZenScheduler: cron-driven ~30s tick, 17 background workers (session-journaler, dream, memory-curator, memvid-indexer, subconscious, notion-extractor, wiki-compiler, commitment-tracker, reflection, wisdom-synth, decision-tracker, express, evidence-gatherer, promotion, zen-loop, morning-brief, retention)
│   │       └── wiring.rs       # ZenWiring (DI wiring)
│   ├── zen-provider/           # Multi-provider LLM routing
│   │   └── src/
│   │       ├── providers/      # 7 protocol-specific providers (13 named configs)
│   │       ├── router.rs       # DefaultRouter, LlmRouter trait, auth resolution
│   │       ├── chat.rs         # MessageRole re-exported from zen-core
│   │       ├── model_meta.rs   # ModelMetadata, ModelRouter, routing metrics
│   │       └── stream.rs       # StreamResponse
│   ├── zen-memory/             # Identity context (SOUL.md, MEMORY.md)
│   ├── zen-auth/               # Keychain + SecretRef resolution
│   ├── zen-plugin/             # WASM sandbox + MCP server
│   └── zen-gateway/            # Sole-owner daemon: protocol/, transport/, channel/ (qqbot carrier), client/, server/ (hosting+guards+approval), daemon.rs
├── config/                     # Embedded config.toml (provider definitions)
├── docs/specs/                 # Architecture specs (~400KB)
├── assets/                     # Static assets
├── templates/                  # Tera templates
└── bin/                        # build, test, lint, release scripts
```

## WHERE TO LOOK

### CLI Commands

| Task | Location | Notes |
|------|----------|-------|
| Add CLI command | `crates/zen-cli/src/cmd/_command.rs` | Add to `mod.rs`, wire in `cli.rs` Commands enum |
| Modify TUI | `crates/zen-cli/src/tui/` | ratatui interface |
| Change dispatch | `crates/zen-cli/src/cli.rs` | Add enum variant, match arm in shell() |

### Core Infrastructure

| Task | Location | Notes |
|------|----------|-------|
| Modify config layers | `crates/zen-core/src/config.rs` | merge_configs() handles inheritance |
| Add error variant | `crates/zen-core/src/errors.rs` | AgenticError for domain errors, ZenError top-level |
| Change path scope | `crates/zen-core/src/paths.rs` | ZenPaths (global_root + workspace_root) |
| Add provider constant | `crates/zen-core/src/constants.rs` | URL + default model constants |
| Secret resolution | `crates/zen-core/src/secrets.rs` | SecretRef (keychain/env) |

### Data Layer

| Task | Location | Notes |
|------|----------|-------|
| Add sqlx migration | `crates/zen-repo/migrations/NNN_name.sql` | Forward-only additive (Principle XIII); ship with `tests/migration_NNN_name.rs` |
| Modify FTS5 schema | `crates/zen-repo/migrations/NNN_name.sql` | Additive only; use `INSERT INTO <fts>(<fts>) VALUES('rebuild')` when triggers change (Principle XIII #8) |
| Modify vec0 schema | `crates/zen-repo/migrations/NNN_name.sql` | vec0 has no ALTER — destructive change requires backup-recreate-reimport protocol (Principle XIII #4) |
| Modify graph schema | `crates/zen-repo/migrations/NNN_name.sql` | ALTER TABLE ADD COLUMN with NOT NULL DEFAULT; deprecated columns stay (Principle XIII #5) |
| Add repository | `crates/zen-repo/src/<domain>_repo.rs` + export in `lib.rs` | Hold `&SqliteClient`; async methods only (Principle XII) |

### Knowledge Services

| Task | Location | Notes |
|------|----------|-------|
| Add search tier | `crates/zen-vault/src/search/tierN.rs` | + register in search/service.rs |
| Modify distill pipeline | `crates/zen-vault/src/distill/pipeline.rs` | Stages: extract → normalize → compile → merge → archive; transaction/checkpoint/recovery |
| Add lint rule | `crates/zen-vault/src/tindy/lint.rs` | Linter (page-level: orphans, broken wikilinks) |
| Add scheduler worker | `crates/zen-agents/src/scheduler/workers/` | ZenWorker trait, cron schedule, register in scheduler/mod.rs |
| Note format change | `crates/zen-vault/src/note.rs` | frontmatter, Domain, write_note |

### Agent System

| Task | Location | Notes |
|------|----------|-------|
| Register agent | `crates/zen-agents/src/registry.rs` | DefaultAgentRegistry |
| Agent definition | `crates/zen-agents/src/zen_agent.rs` | ZenAgentBuilder |
| Plan-DAG tool change | `crates/zen-agents/src/plan_task.rs` | plan.execute workflow executor (T375) |
| Quality gate | `crates/zen-agents/src/review.rs` | Metis→Momus→Hermes→Zeus pipeline |
| Sandbox extension | `crates/zen-agents/src/sandbox.rs` | WasmSandbox (wasmtime) |

### Provider Routing

| Task | Location | Notes |
|------|----------|-------|
| Add provider | `crates/zen-provider/src/providers/` | impl Provider trait |
| Routing logic | `crates/zen-provider/src/router.rs` | DefaultRouter, LlmRouter |
| Model metadata | `crates/zen-provider/src/model_meta.rs` | ModelRouter, metrics |
| Auth resolution | `crates/zen-provider/src/router.rs` | resolve_api_key(), 4-tier resolution |

## TYPE TAXONOMY (Structural Concept System)

All data types across workspace crates follow a 7-layer hierarchy. Each layer has a
single responsibility; types must not leak across layer boundaries except through
explicit dependency injection.

### Layer Overview

```
配置层 (Config)       ProviderConfig, AgentConfig, ModelEntry, ModelOptions,
                      VariantConfig, FallbackStep, RetryPolicy

核心层 (Core)         Sensitivity, MessageRole, Message, SessionStatus,
                      Session, SessionContext, RetrievedNote

Agent 层 (Agent)      AgentSpec, AgentProfile, Role, Capability,
                      AgentClearance, ToolPermission, CostPerToken, LlmPreference

执行层 (Execution)    Turn, TurnState, TurnEvent, TurnRegistry,
                      SessionHost, TurnExecutor

路由层 (Routing)      ModelMetadata, ModelRouter,
                      ComplexityLevel, TaskType, Task, SemanticEntropy

事件层 (Events)       SessionEvent (Session | Message)

工具层 (Tools)        ToolSchema (external dependency, rig_compose)
```

### Key Design Relationships

- **`AgentSpec ⊂ AgentProfile`**: `AgentProfile` is the canonical identity managed by
  the registry; it embeds `AgentSpec` as `definition: Option<AgentSpec>`. `AgentSpec`
  is the static definition (prompt, permissions, constraints); `AgentProfile` adds
  role, capabilities, LLM preferences, and clearance. Both have a `name` field —
  duplicates are tracked for cleanup (ADR-013).

- **`Session`**: Domain model for session metadata persisted as the first
  `session/meta` event in `<id>.jsonl`. Contains identity, lifecycle status,
  and timestamps. Analogous to Codex's `Session`.

- **`Message`**: Domain model for conversation turns inside `SessionContext`.
  `{ role: MessageRole, content: String, timestamp: Option<DateTime<Utc>> }`.
  Also used as `SessionEvent::Turn(Message)` payload for JSONL persistence.
  `MessageRole` serializes as lowercase strings (`"user"`, `"assistant"`) via
  `#[serde(rename_all = "lowercase")]` for backward-compatible JSONL format.

- **TurnState lifecycle**: `Submitted → Running → Streaming → AwaitingApproval →
  Completed | Cancelled`. Errors emit `"turn_error"` structural events but transition
  to `Completed` (the turn terminates; the error is in the event payload, not the
  state). Watchdog timeouts transition to `Cancelled`. `TurnState` intentionally lacks
  a `Failed` variant — terminal states represent lifecycle completion, not outcome.

### Triple-Enum Safety Taxonomy (2026-06-01 remediation)

Three deliberately separate enums model orthogonal safety concerns. They were
historically conflated under a single "SensitivityLevel" and were split to prevent
accidental cross-domain mixing.

| Enum            | Axis                | Domain                | Variants                      | Ord  | Crate          |
| --------------- | ------------------- | --------------------- | ----------------------------- | ---- | -------------- |
| `Sensitivity`     | Data classification | zen-core (universal)  | Public / Private / Confidential | Yes  | types.rs       |
| `SafetyLevel`     | Action validation   | zen-core (validate)   | Safe / Warning / Protected     | No   | validate.rs    |
| `AgentClearance`  | Agent permission    | zen-agents (agent)    | Low / Medium / High            | Yes  | agent_profile  |

- **`Sensitivity`**: Classifies data (notes, sessions, tool invocations). Used for LLM
  routing decisions (`enforce_sensitivity()`), tool gating (`ConfidentialityHook`),
  and session policies. Ordered so `max()`/`max_of()` can compute ceilings.

- **`SafetyLevel`**: Classifies *actions* (path modifications, command execution) by the
  `RoleSeparationValidator`. Isolated to `zen-core/src/validate.rs` — never imported
  by other crates.

- **`AgentClearance`**: Determines which agents may handle which data levels. Compared
  against `Sensitivity` at the orchestrator level by convention (no `From` impls
  between the two). `AgentProfile::can_handle_sensitivity()` takes `AgentClearance`.

No `From`/`Into` implementations exist between any pair. This is intentional — cross-
domain bridging is explicit at the orchestration layer, never implicit via the type
system.

## CONVENTIONS

- **Workspace deps**: All shared deps in `[workspace.dependencies]`, inherit via `workspace = true`
- **Error flow**: thiserror for library → anyhow for app → ZenError as top-level; AgenticError auto-categories via ErrorCategory
- **CLI pattern**: `main.rs` (bin) → `zen_cli::shell()` (lib) → clap Parser → `execute_command()` dispatch
- **Config**: 5-layer merge (Default → embedded → global `~/.zen/` → workspace `.zen/` → `ZEN_*` env vars)
- **Path scoping**: Path Spec v2 — single root `ZEN_HOME` (production `~/.zen`, dev/test `~/.zentest`). Config is global-only (4-layer: Default → embedded → global `~/.zen/config.toml` → env). `.zen/` directory is project context marker + sandbox allowlist only (no config/user_data/output workspace-awareness). Workspace-root detection (`ZenPaths::find_workspace_root`): `ZEN_WORKSPACE` env override (any existing dir) wins; else walk up from cwd (or from `ZEN_HOME`'s parent when set) for a `.zen/` marker, stopping at `$HOME`/global_root — `None` when no marker (launchd daemon cwd=/ ⇒ None is the normal hosted case; global tree stays fully functional, project-workspace writes from hosted turns are not supported — Phase 29 T200 contract).
- **Tests**: Integration only (no inline `#[cfg(test)]` in most crates). Custom ZenTest/ZenOutput harness.
- **Lint**: `bin/lint` → `-D warnings` + `--allow dead_code`
- **Commit messages (human-style, hook-enforced)**: subject `type(scope): 功能点`（Conventional Commits，≤120 字符）；正文只写功能点——改了什么能力、为什么（≤5 个短 bullet）。**禁止**文件清单、diffstat、测试计数、门禁结果（如 "3088 passed"、"files changed (+…)"、"Gate:"、"baseline …"、≥3 处 `crates/` 路径）——`bin/ship` 安装的 commit-msg hook 拦截这些明细模式（单次放行 `SHIP_ALLOW_DETAIL=1` 或 `./bin/ship --allow-detail`）。实现明细归档到 `docs/designs/` 设计记录或 tasks.md ledger，不进 git 历史。
- **Command files**: Pattern `src/cmd/{name}_command.rs` with `pub fn execute_command(...)` dispatcher (note: correctly spelled now)
- **Async traits**: Object-safe async traits MUST use `#[async_trait::async_trait]` over manual `Pin<Box<dyn Future>>` plumbing (引入 async-trait 减少代码量). Prefer owned-payload callbacks (`&mut dyn FnMut(String)`) so the macro has no lifetime to hoist; if borrowed tokens are unavoidable, annotate HRTB explicitly (`for<'s>`) at the trait site. Exemplar: zen-gateway `TurnExecutor`.

## CODE DOCUMENTATION & SCOPE LOGIC (Principle XV)

**Code without documentation is incomplete code.**

### Standardized Code Block Format

All code blocks in documentation MUST follow this structure:

```markdown
```<language>
# <PURPOSE>: One-line description of what this code does
# <USAGE>: When to use this code
# <EXPECTED>: What the user should see after running
# <ERRORS>: Common failure modes and fixes

<actual code>
```
```

Example:
```bash
# PURPOSE: Run zen sandbox test to verify sandboxing is working
# USAGE: After installing bubblewrap/sandbox-exec, verify sandbox works
# EXPECTED: ✅ Sandbox test passed — echo executed successfully
# ERRORS: Sandbox binary not found → install bubblewrap or use --skip-check

zen sandbox test

# Expected output:
# ✅ Sandbox test passed — echo executed successfully
#   Output: zen-sandbox-test-12345
```

### Scope Logic Documentation

All scope-defining code (feature flags, mode switches) MUST include:

1. **Functionality**: What the code enables/disables
2. **User impact**: How the scope change affects behavior
3. **Default behavior**: What happens when not explicitly set
4. **Interaction**: How this scope interacts with other modes/flags

Example (from zen sandbox):
```
--sandbox <MODE>
  Functionality: Override sandbox mode for this invocation
  User impact: Controls what filesystem/network access the command has
  Default: workspace-write (from ZEN_SANDBOX_MODE env var)
  Values: read-only, workspace-write, ask, danger-full-access
  Interaction: --sandbox overrides ZEN_SANDBOX_MODE env var
```

### Public API Documentation

Every public function, struct, enum, and trait MUST have:
- **Description**: What it does (not what it is)
- **Parameters**: Each parameter's purpose and constraints
- **Returns**: What the return value represents
- **Errors**: When and why errors occur
- **Examples**: At least one usage example

### CLI Command Documentation

Every CLI command and subcommand MUST have:
- **Description**: One-line purpose
- **Usage**: Common invocation patterns
- **Examples**: Both command and expected output
- **Scope logic**: All flags with functionality, user impact, and defaults

### AGENTS.md Synchronization

AGENTS.md MUST be updated in the same PR that adds or modifies:
- New CLI commands or subcommands
- New feature flags or scope logic
- New code blocks in documentation
- Changes to existing functionality affecting user behavior

### Documentation Checklist

When adding new CLI commands:
- [ ] Add command to COMMANDS (CLI) table below
- [ ] Add code block with language identifier
- [ ] Add scope logic documentation for all flags
- [ ] Update CONVENTIONS section if patterns change
- [ ] Verify AGENTS.md matches actual implementation

## ANTI-PATTERNS (THIS PROJECT)

- **Typo history**: `excute_command` appeared in all 24 cmd files; corrected to `execute_command` across all command files
- **Tests/main.rs**: Unusual pattern -- only declares modules, no `#[test]` functions
- **Stale files**: `crates/zen-cli/src/cmd/workspace.rs`, `session.rs`, `config.rs`, `agent.rs`, `audit.rs` (no extension) are stale/unused
- **FTS5 table**: Virtual table named `notes_fts` (not `fts_notes`); search queries use `notes_fts`
- **GitHub Actions CI exists**: `.github/workflows/rust.yml` (test gate on `ubuntu-22.04`, cargo nextest), `release.yml`, `docs.yml`. Linux-only failures are invisible to a macOS-local `bin/lint` — verify Linux pre-push via OrbStack (see Development commands).

## COMMANDS (CLI)

### Development

```bash
cargo build              # Build all crates
bin/test                 # THE test entry: nextest full suite (+ `tui` TUI pipeline L1-L4, `bench`/`bench-smoke` divan micro-benches; launched `bin/test-tui` merged in as the `tui` subcommand family 2026-10-18)
cargo test               # Run integration tests
cargo bench -p zen-vault -p zen-cli --features zen-cli/bench --bench '*'  # divan micro-benches: search RRF fusion + tier routing + TUI markdown render (codex precedent: no bench wrapper script; `-- --test` smoke form)
bin/lint                 # fmt --check + clippy -D warnings
orb run bash -c 'unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY; cd "$PWD" && CARGO_TARGET_DIR="$HOME/zen-linux-target" bin/lint'  # Linux lint gate in OrbStack VM (CI parity; arm64; isolates target dir from macOS). Proxy unset is MANDATORY: OrbStack injects the macOS host proxy into the VM; a dead host proxy leaves http_proxy=0.0.0.0:1087 and every cargo registry fetch fails with "curl failed … via 0.0.0.0" — the VM's NAT networking needs no proxy (verified: direct curl to index.crates.io returns 200 with proxy vars unset)
orb run bash -c 'unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY; cd "$PWD" && CARGO_TARGET_DIR="$HOME/zen-linux-target" cargo test -p <crate> --lib <filter>'  # Run tests under real Linux git/glibc (same proxy-unset requirement)
cargo fmt --all          # Format
bin/ship -m "type(scope): 功能点" [-b "功能点正文"]  # Gated commit: lint + Conventional Commits + AI-signature/detail blockers (--suggest for a draft, --allow-detail one-pass)
cargo test -p zen --test load_harness -- --ignored --nocapture  # SC-004 load test: 100-note batch, isolated ZEN_HOME (#[ignore]d; minutes-long — never in bin/test default gate; codex/pi precedent: no dedicated wrapper scripts)
bin/zentest              # Any zen command against ~/.zentest instead of ~/.zen
bin/release patch        # Bump version, tag, push
```

### Agentic Commands (23 commands)

Personal-agent scope (2026-08-28, 005-agentic-loop): zen focuses on the personal memory/knowledge pipeline. Nine manual commands (`note`, `search`, `similar`, `notion`/`graph`, `research`, `ingest`, `routine`, `brief`, `dispatch`) were removed from the CLI surface — their capabilities live on internally via ZenScheduler workers and the distill loop; the command files remain on disk uncompiled, restorable when business scenarios require.

| Command | Description | Dispatch File |
|---------|-------------|---------------|
| (no args) | TUI (ratatui interactive) | `tui/run()` |
| `zen chat` | Interactive LLM chat | `chat_command.rs` |
| `zen clean` | Cleanup (cache ungated; all/trash gated behind a y/N confirmation — they destroy data OUTSIDE ~/.zen: system Trash via Finder, other apps' logs, IDE caches; `--yes`/`-y` bypasses for scripts) | `cleanup_command.rs` |
| `zen starter` | Dev tools/workspace init | `starter_command.rs` |
| `zen wps` | Work process utilities | `wps_command.rs` |
| `zen version` | Show version | `cli.rs` inline |
| `zen session` | Session lifecycle | `session_command.rs` |
| `zen serve` | Gateway daemon: start [--foreground] [--http] / status [--json] / stop / restart / mcp / install / uninstall (macOS launchd LaunchAgent); `status` shows binary vs running-daemon version with a `STALE`/`STALE-unknown` marker (JSON: `versionState`/`stale`/`binaryVersion`/`daemonVersion`); `restart` = `launchctl kickstart -k gui/{uid}/dev.zen.serve` when the LaunchAgent plist exists, else graceful stop (SIGTERM drain ≤10s + audited cancels) + start under StartupLock/readiness — a stopped daemon makes restart a plain start; `--http` (or `ZEN_GATEWAY_HTTP_ENABLED=1`) additionally mounts the loopback HTTP carrier `/health` + `/api/v1/{chat,agents,ws,mcp}` on the same dispatcher; SIGTERM drains in-flight turns ≤10s then cancels with audits (exit 0) | `serve_command.rs` |
| `zen agent` | Agent registry | `agent_command.rs` |
| `zen workspace` | `.zen/` structure | `workspace_command.rs` |
| `zen config` | Config layers | `config_command.rs` |
| `zen provider` | LLM provider mgmt | `provider_command.rs` |
| `zen audit` | Audit log ops | `audit_command.rs` |
| `zen logs` | Structured log viewer | `logs_command.rs` |
| `zen wiki` | Wiki ops: list, show, reindex, lint, distill, rebuild-memory, loop (run/status/gaps/enable/disable), export-skill [--output] [--target zen\|agents] (T213: `agents` writes the vendor-neutral `~/.agents/skills/zen-wiki/SKILL.md`; explicit `--output` wins over both), rollback | `wiki_command.rs` |
| `zen model` | Model metadata + routing | `model_command.rs` |
| `zen plugin` | Plugin management (install, enable, disable, rehash, tools list) | `plugin_command.rs` |
| `zen auth` | Auth/keychain ops | `auth_command.rs` |
| `zen habit` | Habit tracking | `habit_command.rs` |
| `zen goal` | Goal management | `goal_command.rs` |
| `zen skill` | Skill management (list, run, progress, show, precipitate, confirm) | `skill_command.rs` |
| `zen discover` | Self-learning gate surface (PD-06): run (one zen-loop cycle now — stages 5b/5c/5d), stage/queue, confirm/reject (Hybrid C promotion gate; BeliefEvidence applies `Belief::update` on confirm), report (discover metrics, reads `loop-last-report.json`; additive `memory_strength` section reads `logs/memory-strength-report.json` — missing/corrupt renders `{"recorded": false}`, replay-section fail-open precedent), arena (distill regression gate vs baselines/external CLIs — every lost case is staged as an improvement hypothesis under `wiki/wisdom/hypotheses/`) | `discover_command.rs` |
| `zen sandbox` | OS-level sandbox surface: `run` (exec a binary under the active sandbox mode), `status`, `policy`, `test` (verify isolation works) | `sandbox_command.rs` |
| `zen doctor` | System health: 10 liveness probes (config, state.db, memories, daemon, daemon-version, loop, provider, vault, outbox — staged briefs + unconfigured qqbot ⇒ FAIL with actionable message; daemon-version: running explicit daemon whose pid-record version mismatches/is unknown ⇒ FAIL with "run `zen serve restart`", daemon-down or implicit daemon ⇒ pass; schema-compat (W4.3): read-only applied-vs-embedded migration comparison — skew (db ahead of binary) ⇒ informational PASS "tolerated (additive-only); upgrade zen when convenient", missing DB/table ⇒ fresh-install PASS, corrupt ⇒ FAIL; never migrates, never takes the flock); `--json` machine output; exit 0 all-green / 1 any-fail | `doctor_command.rs` |

## AGENT TOOL INVENTORY (v0.0.6)

All tools registered in `ZenWiring::new()` (`crates/zen-agents/src/wiring.rs`), dispatched through the 5-hook pipeline (confidentiality → budget → seatbelt → audit → approval) plus plugin hooks.

| Tool | Sensitivity | Source | Notes |
|------|-------------|--------|-------|
| `fs.read` | Public | zen-plugin/tools/fs_read.rs | Byte-range + max_bytes 1MB streaming (FR-023), base64 binary mode (FR-027) |
| `fs.write` | Private | zen-plugin/tools/fs_write.rs | SandboxValidator-gated |
| `fs.edit` | Private | zen-plugin/tools/fs_edit.rs | diffy unified diff, atomic write + TempfileDropGuard (FR-021/040) |
| `fs.delete` | Private | zen-plugin/tools/fs_delete.rs | Workspace-root guard + clear_contents mode (FR-026) |
| `fs.move` | Private | zen-plugin/tools/fs_move.rs | Source+dest validated |
| `fs.copy` | Private | zen-plugin/tools/fs_copy.rs | Dest validated |
| `fs.list` | Public | zen-plugin/tools/fs_list.rs | depth/glob/include_hidden, symlink-consistent is_dir (FR-025) |
| `fs.grep` | Public | zen-plugin/tools/fs_grep.rs | Regex content search |
| `fs.glob` | Public | zen-plugin/tools/fs_glob.rs | Pattern matching |
| `web.fetch` | Private | zen-plugin/tools/web_fetch.rs | NetworkPolicy-validated (FR-036), Jina fallback |
| `web.search` | Private | zen-plugin/tools/web_search.rs | DDG/Brave/Tavily tiers, NetworkPolicy-validated |
| `shell.exec` | **Confidential** | zen-plugin/tools/shell_exec.rs | Structured binary+args (no shell string), env scrubbed, timeout SIGTERM→SIGKILL process-group, excluded from external MCP (FR-028) |
| `system.*` (5 tools) | Public/Private | zen-plugin/platform/ | health/notifications/calendar/daemon/fs_watcher — fs_watcher capped at 8 (FR-045), seatbelt arg-registry mediated (FR-035) |
| `plugin.wasm_sandbox` | Private | zen-plugin/wasm_sandbox.rs | Permission gate on every invoke (FR-029), StoreLimits memory cap (FR-030), single impl (FR-031) |
| `tier2_search`/`tier3_search`/`tier4_search`/`compute_embeddings` | Private | zen-vault | KB search tools via ZenTool adapter. `tier3_search` (vec0 semantic KNN) closes the gap where semantic retrieval was reachable only from the gateway `knowledge/search` RPC; granted to the search-capable agents (tier2+tier4 holders) in `delegate_tools::AGENT_TOOLS` |
| `delegate.task` (006/T373/T374) | inherits session | zen-agents/delegate_task.rs | Model-driven sub-agent delegation: args `{agent, prompt, description}` single-task or `{tasks:[...]}` fan-out (≤8/batch, batch width ≤4 passes the consumer gate); sub-turn runs a real LLM loop (≤4 rounds) on a sub-agent whose grants keep `delegate.*` only below the depth cap (`max_depth` default 1, clamp 1..=3, tier matrix: O→O 拒, P→P/O 拒, Specialist/Worker leaf); `spawn_allowed` + task-local depth chain; delegation gates are `evaluate_gates` (advisory: independent/consumer-decision/bounded≤32k chars/worth-it, logged as `loop.delegate.gates`) + `validate_task` (hard: known-agent/tier-matrix/depth-cap — the recorded `should_delegate`/`tool_loop_over_budget` names never existed as functions, D14 drift correction); optional `output_format`/`task_boundaries` contract args + `reasoning`/`verification` report markers (D9, W1.2) and `artifact` output-path convention (D12, W2.2 — sandbox-validated, fs.write-grant required); fan-out results are slot-ordered (results[i] ≡ tasks[i] with zero/empty weights, mixed reject+run batches included; every result carries `task_index` for re-attribution when D10 weight ranking reorders); concurrency `[agentic.delegate].max_concurrent` (default 4, clamp 1..=8, env `ZEN_DELEGATE_MAX_CONCURRENT`); lazily registered at first orchestrator turn, kill-switch `[agentic.delegate].enabled`; structured Ok-error outputs, never panics the round; fan-out hard cap `FANOUT_MAX_TASKS=8` enforced in `parse_requests` with structured rejection (T216 — the recorded ≤8 value, previously advisory-only); `[agentic.delegate].context_inheritance` (default false, T215): on ⇒ sub-session inherits the parent's retrieved knowledge block via `compose_sub_session` (budget halved per depth, gates-audit additive `{inherited_notes, inherited_chars, skill_hits}`) |
| `plan.execute` (T375/T376) | Private | zen-agents/plan_task.rs | Sisyphus-only plan-DAG executor: args `{name?, tasks:[{id, agent, prompt, depends_on?}], resume_plan_id?}` (checkpoint replay matches task_id AND agent — agent-drift reruns); Kahn layering ≤12 tasks/≤3 layers; per-task hard gates via the SHARED `DelegateTaskTool::validate_task` path (known-agent/tier-matrix/depth-cap/bounded-32k — Sisyphus is rejected as a task agent by the O→O rule); per-layer parallel batches through delegate `run_single` (token budget + wall-clock timeout); `build_sub_agent` strips `plan.execute` unconditionally; upstream failure → downstream skipped; merged deliverable through QualityPipeline (Hermes verdict in output + `loop.plan.completed` audit); persists to state.db `workflow_plans`/`workflow_tasks` (migration 006) with per-task idempotent checkpoints; `resume_plan_id` replays ok-checkpoints, re-runs pending; resume is claim-fenced (migration 007 `workflow_plans.owner` + `WorkflowRepo::claim_plan` conditional UPDATE — one live resume per plan, same-owner re-claim ok, stale claims >3600s stealable, lost race → `already being resumed`); terminal plans refuse resume; shares the `[agentic.delegate]` kill-switch (plans execute via delegation) |

### Host-OS Safety Hardening (v0.0.6, FR-035..045)

- **Symlink canonicalization** (FR-024): `SandboxValidator` resolves symlinks, rejects escapes to protected paths/workspace-exit; macOS `/tmp → /private/tmp` root-canonicalization handled
- **Per-tool arg registry** (FR-035): `ToolArgRegistry` maps tool → path/command-bearing arg keys; no tool bypasses seatbelt
- **Network egress policy** (FR-036): `zen-core/src/network_policy.rs` — denies link-local/loopback/RFC1918 + metadata hostnames; allowlist via `[sandbox.network_policy]`
- **Env scrubbing** (FR-037): `zen-core/src/env_scrub.rs` — child processes never inherit `*_API_KEY`-style vars; `shell.exec` `env` param is the only injection path
- **RLIMIT wiring** (FR-038): `apply_resource_limits()` called in `ZenWiring` construction (NPROC=50/NOFILE=256/CORE=0)
- **Tempfile lifecycle** (FR-040): `zen-core/src/tempfile_lifecycle.rs` — DropGuard + boot-time sweep
- **Signal drain** (FR-041): SIGINT/SIGTERM 5s drain window in TUI session
- **Process hardening** (FR-044): `zen-core/src/process_hardening.rs` — PT_DENY_ATTACH/prctl, RLIMIT_CORE=0, LD_*/DYLD_* strip at startup
- **Plugin framework** (FR-032..034): `Plugin` trait + `PluginApi`; `ZenWiring::with_sandbox_mode` accepts `Option<&PluginRegistry>` (FR-033 bridge); PluginKind pruned to Tool/Hook
- **Plugin integrity** (FR-043): manifest `sha256` verified (HashMismatch rejection); macOS `.dylib` codesign-verified

### launchd Persistence (macOS-only)

`zen serve install` / `zen serve uninstall` manage a macOS LaunchAgent for the gateway daemon:

**Scope logic — `[gateway] upgrade_policy`** (Phase 30 G3):
```
[gateway] upgrade_policy
  Functionality: selects what the stale-daemon detection points (`zen serve status` human mode,
    TUI prewarm) do when the running daemon binary ≠ invoking CLI binary (post-`brew upgrade` case)
  User impact: `warn` surfaces only (status STALE marker, doctor `daemon-version` FAIL, TUI banner
    hint); `auto-restart` additionally runs the `zen serve restart` drain-restart on the spot —
    in-flight turns drain ≤10s then cancel with audit lines; one restart + one reconnect, never a loop
  Default: `warn` (gate-closed, owner decision D-A); invalid values fall back to `warn` with a
    tracing warning (never silent — cron-timezone discipline)
  Interaction: env `ZEN_GATEWAY_UPGRADE_POLICY` (5th layer) overrides config layers; applies ONLY to
    explicit/launchd daemons (implicit TUI-spawned daemons run the current binary and never trigger);
    `zen doctor` daemon-version probe is diagnose-only regardless of policy; `zen serve status --json`
    reports without acting so machine consumers decide
```

```
zen serve install
  Functionality: writes ~/Library/LaunchAgents/dev.zen.serve.plist with:
    - Label: dev.zen.serve
    - ProgramArguments: zen serve start --foreground
    - RunAtLoad: true (auto-start on login)
    - KeepAlive: true (respawn on crash)
    - ThrottleInterval: 60 (crash-loop throttle)
    - EnvironmentVariables: HOME + PATH (/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin)
    - StandardOutPath/ErrorPath: {global_root}/logs/serve.{out,err}.log
  User impact: daemon starts automatically on login; after install, `zen serve start` may cause a second instance (socket guard prevents conflicts)
  Default: not installed — user must run explicitly
  Interaction: `zen serve uninstall` reverses; explicit `zen serve start` works independently of launchd

zen serve uninstall
  Functionality: launches `launchctl bootout gui/{uid}/dev.zen.serve` (tolerates "not loaded") then removes the plist file
  User impact: daemon stops starting on login; running instance unaffected
  Default: no-op when not installed (tolerant)
  Interaction: after uninstall, manual `zen serve start` works as before
```

Implementation: `render_plist()` is a pure function (unit-testable); `gui_domain()` resolves UID via `id -u` (not `process::id()` which returns PID). Non-macOS platforms return a clear `ZenError` at runtime.

### zen doctor System Health

`zen doctor` runs 10 liveness probes with JSON/human dual output (Principle IX):

```
zen doctor [--json]
  Functionality: runs 10 isolated health checks:
    1. config: load_config() succeeds (4-layer global)
    2. state.db: exists + readable (file header check)
    3. memories: memvid store under {global}/memories/ exists (count/size)
    4. daemon: gateway socket alive (UDS connect or file-exists)
    5. daemon-version (Phase 30 G3): running explicit daemon's pid-record version vs binary —
       mismatch/unknown (pre-version record) ⇒ FAIL "run `zen serve restart`"; daemon-down or
       implicit daemon (no pid record — runs current binary by construction) ⇒ pass; diagnose-only
    6. loop: last cycle evidence (loop-last-report.json mtime <2× interval)
    7. provider: configured provider resolves API key OR ollama-local
    8. vault: {global}/vault/ exists, git work-tree, inbox/wiki/raw counts
    9. outbox (T189): staged morning-briefs + unconfigured [channels.qqbot] ⇒ FAIL naming the fix
   10. schema-compat (W4.3): read-only (mode=ro) applied-vs-embedded migration comparison; skew
       (db ahead of binary) ⇒ informational PASS; missing DB/table ⇒ fresh-install PASS; corrupt ⇒
       FAIL; never migrates, never takes the flock
  User impact: exit 0 all-green / exit 1 any-fail; --json for machine parsing
  Default: human-readable one-line-per-check output
  Interaction: exit code follows Principle IX conventional exit codes
```

### Plugin Runtime Hardening (v0.0.6, FR-046..051)

- **Config-driven agent grants** (FR-046): `[agents.tools]` overlay in 5-layer config merges builtin defaults with user patterns; supports exact names, `prefix.*`, and `plugin:*` wildcards (excludes reserved namespaces and builtin collisions)
- **Strict sha256 + rehash recovery** (FR-049): manifest `sha256` is required for `*.wasm` entries; missing hash rejected with `MissingHash`; `zen plugin install` auto-writes hash; `zen plugin rehash <id>` recomputes after deliberate updates
- **state.json persistence** (FR-047): `zen plugin enable/disable` writes to `{plugin-dir}/state.json` (`{"disabled": [...]}`); takes effect next session, no hot-unload; corrupt file fails open with a loud `warn`
- **Plugin hook isolation** (FR-048): every plugin hook wrapped in fail-closed adapter; hook `Err` denies that invocation only (audit-correlated record), never aborts the round; plugin hooks invisible to `Confidential` invocations
- **Plugin id validation + reserved namespaces** (FR-050): ids must match `^[a-z0-9_-]+$`; reserved prefixes (`fs`, `web`, `system`, `plugin`, `shell`) rejected at registration; spoofed builtin names blocked per-tool with `warn`
- **Lazy WASM precompile** (FR-051): `WasmPluginTool` caches compiled `Module` via `OnceLock`; first invoke compiles, subsequent invocations reuse; wasm bytes held as `Arc<Vec<u8>>`

## FRAMEWORK PATTERNS

### clap Derive API (CLI)

- `Parser` derive on `Cli` struct with `#[command(author, version, about)]`
- `Subcommand` derive on `Commands` enum (21 variants)
- Subcommand structs in `cmd/` modules with `Subcommand` derive (e.g., `SessionCommands`, `NoteCommands`)
- `clap_verbosity_flag::Verbosity<InfoLevel>` for global `--verbose`
- `Option<Commands>`: `None` triggers TUI, `Some` triggers dispatch

### rig-core Routing (Provider)

- `rig_core::client::Client<Ext, H>` as base for provider clients
- `CompletionModel` trait for chat completion abstraction
- `LlmRouter` trait (factory + routing) implemented by `DefaultRouter`
- Provider trait: `OllamaProvider`, `OpenAIProvider`, `AnthropicProvider`, etc.
- Model fallback chain: task requirements → complexity level → model metadata match

### rusqlite FTS5 (Data)

- `SqliteRepo` wraps `rusqlite::Connection` with WAL mode, transactions
- FTS5 virtual table: `CREATE VIRTUAL TABLE notes_fts USING fts5(...)` with porter tokenizer
- Embeddings: `vec0` virtual table via sqlite-vec extension (4096-dim max, zero-padded for 384-dim models per Principle XIII #7)
- Extension loading: `sqlite_vec::sqlite3_vec_init` auto-registered via `sqlite3_auto_extension` at `SqliteClient::open()`
- Unified data layer: `SqliteClient` (tokio-rusqlite writer + sqlx pool); 9 domain repositories hold `&SqliteClient`

### async_trait (Object-Safe Async Traits)

```rust
// PURPOSE: Declare object-safe async traits without manual Pin<Box<dyn Future>> plumbing —
//          the macro desugars `async fn` into a boxed-future signature implementors can match.
// USAGE: Any trait meant for `Arc<dyn Trait>` / dyn dispatch across crates or test fakes.
// EXPECTED: Implementors write plain `async fn`; dyn dispatch and spawned tasks work as-is.
// ERRORS: Elided lifetimes inside trait-object params (`Box<dyn FnMut(&str)>`) get hoisted to a
//         concrete lifetime param by the macro, breaking HRTB demands. Two escapes: (1) BEST —
//         make the callback payload owned (`&mut dyn FnMut(String)`) so no lifetime exists to
//         hoist; (2) if borrowed tokens are unavoidable, annotate explicitly (`for<'s>`) at the
//         TRAIT site.

// Owned-payload callback: `&mut (dyn FnMut(String) + Send)` keeps the
// signature lifetime-free under the macro — no `for<'s>` HRTB, no boxing;
// call-site closures coerce to `&mut dyn FnMut` automatically.
#[async_trait::async_trait]
pub trait TurnExecutor: Send + Sync {
    async fn execute_stream(
        &self,
        session: &mut SessionContext,
        prompt: &str,
        callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String>;
}
```

- Rule: prefer `#[async_trait]` over hand-written `Pin<Box<...>>` signatures (Constitution XI — reuse over novelty; cuts ~10 lines of boxing boilerplate per method)
- Rule 2: owned-payload `callback: &mut (dyn FnMut(String) + Send)` is the canonical callback shape — borrowed payloads (`&str`) force HRTB annotations; newtype wrappers add boilerplate with no benefit. Static-dispatch layers (`impl FnMut(&str)`, e.g. zen-agents) may keep borrowed payloads; owned is mandatory only under `#[async_trait]` dyn signatures. Param name is `callback` everywhere (never `on_token`).
- Caveat: the macro gives EVERY argument its own `'lifeN`; elided lifetimes inside trait-object parameters become concrete. Callbacks that must stay higher-ranked need explicit `for<'s>` at the trait definition (avoid by using owned payloads).
- Exemplar: `crates/zen-gateway/src/server/hosting.rs` (`TurnExecutor`: orchestrator adapter + scripted test fakes)

### Agent Quality Pipeline

```
Metis (correctness) → Momus (quality) → Hermes (safety) → Zeus (escalation)
```

## AGENT SYSTEM DETAILS

### 13 Agents

Agents are registered in `DefaultAgentRegistry` across 4 tiers:

| Tier | Role | Example |
|------|------|---------|
| Orchestrator | Session coordination, routing | `AgentOrchestrator` (Sisyphus) |
| Planner | Task planning, decomposition | `AgentOrchestrator` |
| Specialist | Domain expertise (search, consolidate) | Agent-specific |
| Worker | Execution, tool calling | `AgentExecutor` |

### 3-Layer Permission Model

1. **Sensitivity filtering** (`SensitivityLevel`): Private / Internal / Public
2. **Role-based tool gating** (`Role`): controls which tools an agent can invoke
3. **Static assignment**: tool permissions assigned at agent registration

### Session Lifecycle

5 states: `Active → Paused → Archived → Error → Complete`

### Plan-DAG Workflows (T375-T376)

`plan.execute` (Sisyphus-only) executes a model-drafted DAG `{name?, tasks:[{id, agent, prompt, depends_on}]}`:
Kahn layering (≤12 tasks, ≤3 layers), per-layer parallel batches via `run_single`
(inherits all delegate gates), upstream failure → downstream skipped, merged
deliverable through QualityPipeline (Hermes verdict = completion signal), one
`loop.plan.completed` audit line. Persistence (T376): `workflow_plans` +
`workflow_tasks` (migration 006, `WorkflowRepo`); checkpoints idempotent on
(plan_id, task_id); `resume_plan_id` replays ok-checkpoints and re-runs the rest;
plans stuck in `running` are resumable, terminal plans refuse.

Superseded: the 001 ADR-009 Blackboard mandate — see
`docs/specs/006-agentic-orchestration/adr-001-blackboard-supersession.md`
(direct tool returns + batch aggregation replaced in-memory channels; blackboard.rs deleted).

## PROVIDER TAXONOMY

### 13 Named Providers / 3 Protocol Types

| Protocol | Named Providers | Implementation |
|----------|----------------|----------------|
| rig-native | ollama, openai, anthropic, cohere, gemini, mistral | Direct rig-core clients |
| openai-compatible | deepseek, aliyun, groq, perplexity | `rig_openai.rs` wrapper |
| anthropic-compatible | moonshot | `rig_openai.rs` with anthropic-compatible adapter |

### Auth Resolution (4-Tier)

1. `api_key` (SecretRef) -- keychain lookup via `zen_auth::resolve_secret_ref()`
2. `api_key_env` (legacy env var name from config)
3. Default env var: `{PROVIDER}_API_KEY` (e.g., `OPENAI_API_KEY`)
4. For ollama: no auth required (local)

### Model Routing

`DefaultRouter::route()` → `TaskRequirements` → `ModelRouter::select()` → `CompletionModel` instance

## NOTES

- **Codex-style owned domain layers (T121, 2026-09-12)**: `rig-tap` + `rig-memvid` dropped; zen owns both thin domain layers in-house (openai/codex pattern: own the domain schema/client layer, keep engines external — codex AGENTS.md "resist adding to core", PR #667 remove-unneeded-dep, PR #4252 own-the-client):
  - (a) `zen-agents/src/telemetry.rs` — `TelemetryEvent{version, conversation_id, occurred_at_millis, kind}` + 3-variant `EventKind` (prompt_started/completed/failed, serde internally-tagged snake_case, None fields omitted) + `ErrorClass::Unknown` + `emit_kind()` → `tracing::info!(target: "zen_tap", SCHEMA_VERSION = 1)` (replaces rig-tap's 1386-line event schema — zen only ever emitted 3 variants; `ObservabilityEvent`/`extract_event` had zero consumers)
  - (b) `zen-memory/src/memvid_store.rs` — `MemvidStore` = `Arc<Mutex<memvid_core::Memvid>>` + `from_memvid`/`open`/`open_or_create`/`open_read_only`/`put_text`/`put_memory_card`/`search`/`entity_memories`/`frame_count`/`stats` + `select_cards(CardSelection::ForPrincipal)` (recency-ordered; replaces rig-memvid's 5.7k lines incl. its unused `rig::vector_store` trait integration and 832-line cards_context; memvid-core engine dep unchanged)
  - Result: `cargo tree --duplicates` = single `rig-core 0.42.0` + single `rig-compose 0.5.0` — the 0.37/0.38.2 islands and the rig-compose 0.4.3 duplicate eliminated at the root (rig-memvid + rig-tap were the only 0.37 pullers)
  - Tests: 2363/0/19 (baseline 2357, +6: 4 telemetry serde + 2 store roundtrip)
- Project uses Rust edition 2024 (stable toolchain, MSRV 1.80+)
- zen-repo uses the unified `SqliteClient` (tokio-rusqlite writer + sqlx pool) with 9 domain repositories
- **Agentic gateway (004)**: sole-owner daemon owns the memvid store RW; chat/TUI route through it via `SurfaceClient` (`ZEN_SANDBOX_MODE=ask` enables Q3 approval routing to the originating surface). Protocol frozen by `docs/specs/004-agentic-gateway/contracts/`; error catalog -32000..-32099 is closed/additive-only. Guards: watchdog 900s (`ZEN_TURN_WATCHDOG_SECS`), circuit breaker 5→60s, doom-loop 20 turns/10min, stale-client GC 30s — rejections are `-32020 guard-rejected{guard,reason}` + audit line in `<logs>/audit.jsonl`; turn lifecycle audit kinds `gateway.turn.started` (turnId/sessionId/agent, once per registration) and `gateway.turn.completed` (outcome completed|cancelled, exactly once at first terminal transition; `tokens` omitted until `TurnExecutor` reports usage) emitted from `server/hosting.rs` (T054). LLM streaming budgets (zen-agents `completion_model`): first-token 600s (`ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS`, covers cold local-model load+prefill), inter-token 120s (`ZEN_STREAM_INACTIVITY_TIMEOUT_SECS`); client turn ceiling 960s (`ZEN_TURN_TIMEOUT_SECS`) — invariant: turn ceiling > watchdog > first-token budget
- **Session replay & memory rebuild (Phase 11)**: GC tick (30s) runs incremental jsonl→mv2 replay (`zen-memory::SessionReplayer`, blake3 turn-key idempotency; checkpoints in state.db `memvid_replay_offsets` via T049 migration 005); skipped-corrupt-line ratio >5%/tick logs ERROR. `health/status.replay` reports `{replayed, skipped, lastOffset}`. `memory/rebuild` RPC (since 1.1, additive MINOR bump — registry now 21 rows) = MemvidIndexer full reindex + checkpoint reset-to-0 + immediate full replay; CLI surface `zen wiki rebuild-memory` errors with "run `zen serve start` first" when daemon offline (D3=A RPC model)
- **Dual-write decision (T051, 2026-08-26)**: rig-memvid `MemvidPersistHook` removed — it was instantiated in `AgentOrchestrator` but never attached to any execution pipeline (dead code; zen calls completion models directly, bypassing rig-core's PromptHook mechanism). `ZenAgent::persist_turn` is the sole active mv2 writer; `create_persist_hook`/`default_memory_config` deleted from zen-memory
- **QQBot channel (004 Phase 13)**: `channel/qqbot/` bridges QQ official-bot events onto the gateway's own loopback HTTP surface — one daemon carries UDS + HTTP(`/api/v1`+MCP) + qqbot concurrently (channel test `qqbot_channel.rs`). Config scope logic:
  ```
  [channels.qqbot]           # config.toml only (no CLI/env surface of its own)
    Functionality: enables the QQ official-bot WS gateway client + v2 REST sender
    User impact: allowlisted QQ chats converse with persistent agent sessions; /new resets a chat's session, /status renders gateway health
    Default: absent → channel off; present-but-incomplete credentials → disabled with WARN
    Values: app_id, client_secret (required); allowed_users (deny-by-default; matches BOTH namespaces — group `author.member_openid` AND C2C `author.id` — so allowlist entries may mix group-member and C2C user openids); home_channel (unused yet); outbox_drain_interval_secs (optional, default 300, clamped 60..=3600 — morning-brief outbox active-send tick; `OutboxDrainer` pushes staged `morning-brief-<date>.json` to live `qq_bindings` chats, group-first/C2C-fallback, Public-only unless allowlisted, never deletes undelivered)
    Interaction: enabling qqbot implies the loopback HTTP carrier (bind 127.0.0.1:9876 default) since it bridges via POST /api/v1/chat; ZEN_QQBOT_APP_ID/ZEN_QQBOT_CLIENT_SECRET env layer applies per standard 5-layer config
  ```
- Schema migrations are forward-only additive (Principle XIII); `_sqlx_migrations` tracks applied versions; `sqlx::migrate!()` runs all pending on every `SqliteClient::open()`
- `docs/specs/001-agentic-foundation/` has extensive architecture docs (~400KB)
- **Cron timezone (E11, 2026-09-06)**: worker cron wall-clock fields evaluate in `[cron].timezone` (IANA name; `CronConfig::default()` ships `Asia/Shanghai`; env `ZEN_CRON_TIMEZONE`, 5-layer) via `CronConfig::timezone_or_default()` → `ZenScheduler::with_timezone` (wired in `create_configured_scheduler`, the serve/TUI path); unparsable name → Utc + warn (a typo must never silently shift schedules). `create_default_scheduler` (dormant, config-free) stays Utc by design. Tick fire decision anchors on the passed tick instant (`Schedule::after`), never the system clock (`upcoming`) — deterministic; worker `ctx.now` remains an absolute Utc instant
- Karpathy guidelines skill installed at `.opencode/skills/karpathy-guidelines/`
- zen-llm exists in directory but is not a workspace member (staged for integration)

## Active Technologies
- Rust edition 2024 (MSRV 1.80+, stable toolchain) (003-agentic-plugin)
- No new database tables. Tool audit records → existing `logs/audit.jsonl` (append-only JSONL). MCP server config → existing 5-layer config inheritance (config.toml `[mcp_servers]` section). Jina/Brave/Tavily API keys → `.env` via `dotenvy`. (003-agentic-plugin)

 - Rust edition 2024 (stable toolchain, MSRV 1.80+) + clap 4.5 (CLI derive), tokio 1.47 (async runtime), rusqlite 0.32 (SQLite FTS5 + sqlite-vec), rig-core 0.42 (LLM abstraction; + rig-agent 0.42 AgentRun runtime, PD-01 B), rig-compose 0.5 (agent kernel), rig-sqlite 0.42 (vector store), rig-mcp 0.2 (MCP bridge), rmcp 0.1 (MCP server), wasmtime 24 (WASM sandbox), security-framework 3 (macOS Keychain), serde/serde_json 1.0, tera (template engine), include_dir (embedded templates), ratatui 0.30 + crossterm 0.28 (TUI), axum 0.8 (gateway), sqlx 0.8 (async SQLite), ort 2.0 (ONNX runtime for embeddings)
- SQLite for derived indexes (FTS5, vector embeddings via sqlite-vec, entity graph, habits, finance), Markdown files as canonical source of truth, TOML for config (config.toml), habits (habits.toml), goals (goals.toml), budgets (budgets.toml), routines (routines.toml)
- Binary/library split: `zen` binary (13 lines) → `zen-cli` library (exporting `shell()`)

## Recent Changes

- **测试入口收敛 + divan 微基准脚手架（2026-10-18，`bin/test` 单一入口）** — `bin/lint` 0；`bin/test` **3324 passed / 0 failed / 25 skipped**（本轮门禁复跑确认此前 2 失败为负载 flake：`bench_retrieval_100_frames`/`test_compile_extracts_wikilinks` 孤立跑全绿）。修改文件与新增功能：
  - **`bin/test-tui` 并入 `bin/test` 成为 `tui` 子命令族**（快/单元/守卫/pty [CASE]/tmux/copy/list，无参 = 全流水线 unit→guard→pty→tmux；`ZEN_SKIP_PTY`/`ZEN_SKIP_TMUX` 跳过语义不变）：治 test-tui 既有两缺陷——原脚本无 `ZEN_HOME=~/.zentest` 兜底（tmux smoke 由真实 `$HOME` 起 `./target/debug/zen` 的污染敞口）且无线程上限（PTY 用例时序敏感，lag 是 flake 温床）；合并后继承 bin/test 的环境防线（兜底 + `CARGO_BUILD_JOBS`/`RUST_TEST_THREADS=2`）。`bin/test-tui` 删除（不再保留 shim）；test-design.md 活合同节/`tui_pty.rs` docstring 引用改指 `bin/test tui …`（案例表 C/D 历史行原样保留）。
  - **divan 0.1.21 微基准脚手架**（codex 先例：no wrapper script，cargo 一行命令）：workspace dev-dep + `zen-vault/benches/search_rrf.rs`（harness=false；4 bench：RRF 4-lists 重叠矩阵 ×50/×200 + TierSelector 路由）+ `zen-cli/benches/markdown_render.rs`（`render_markdown` 一次性 ~1.5ms / `StreamingMarkdown::append` 60 分片 ~15.7ms；修复 `chunks()` 切 CJK 多字节 UB）、`benches` 目录 = 独立二进制只见 `pub` ⇒ zen-cli `lib.rs` 加 `bench` feature cfg 双声明缝（默认构建字节等同，lint `--all-features` 覆盖编译）。基线（fixture 提出闭包后实测）：`rrf_4lists_x50_overlap30` 55.6µs / `x200_full_overlap` 78µs / `zero_overlap` 189µs / `tier_selector` 67ns。
  - **命令**：`bin/test bench`（全量数字）、`bin/test bench-smoke`（`-- --test` 1-iter 干跑，CI gate 形状）、`bin/test tui quick`（~30s 门禁）、`bin/test tui pty e10`（单案例）；或 cargo 直跑 `cargo bench -p zen-vault -p zen-cli --features zen-cli/bench --bench '*'`。
  - divan 选型证据（divan 0.1.21 vs criterion 0.8.2）：stable-only / MSRV 1.80 匹配 workspace（criterion MSRV 1.86 冲突）/ 冷构建 10.4s vs 17.4s / codex-rs 先例（`just bench` + `just bench-smoke`，`Cargo.toml` workspace dep `divan="0.1.21"`，`utils/image/benches/prompt_images.rs`，CI 只跑 smoke）。

- **Embedding 引擎平台分轨（2026-10-18，方案 B 落地）** — `bin/lint` 0。**根因**：ort-sys 自 2.0.0-rc.12 起无 `x86_64-apple-darwin` 预编译（Microsoft ONNX Runtime 1.24.1 后停止 x64 macOS 官方发布；pykeio/ort#556），`ort-download-binaries` 静态链接策略在 intel mac **编译期**即失败（CI macos-15-intel + 用户 `cargo install`）。修改与行为：
  - **默认路径不变**：workspace `fastembed` 特性 `ort-download-binaries-rustls-tls`（编译期下拉预编译 ORT 并静态链接；arm mac/linux x64/arm64 用户单 binary 自包含，runtime 零依赖）。
  - **唯一 per-target 例外**（`zen-provider/Cargo.toml`）：`[target.'cfg(all(target_os = "macos", target_arch = "x86_64"))']` 叠加 `ort-load-dynamic`（implies `ort-sys/disable-linking`，build script 在 prebuilt 分支之前检查 ⇒ 顺序正确就永不下载）——intel mac 编译永远成功；运行时无 `libonnxruntime.dylib` ⇒ `FASTEMBED_BROKEN` ⇒ embedding 降级 ollama/hash（doctor 可见，报错文案 "loading the bundled onnxruntime"）。验证：跨目标 `cargo check --target x86_64-apple-darwin` exit 0 + `cargo tree -e features` 双特性可同时解析；arm/linux 构建不受影响。
  - intel mac 用户可选装 brew `onnxruntime`（≤1.24.1，注意 ort rc.13 minimum api 版本差异可能报错 ⇒ 表现为降级，非崩溃）。
  - 性能基线（决策依据）：进程内 ort 3052 eps vs Ollama HTTP 553 eps（同机 M4 Max，HTTP 边界 ~10ms/次）；ollama 线有 open 正确性 bug 群（NaN/全零向量/静默截断）——ollama 仅建议作为生成层外包，不作为 embedding 默认。

- **FR-009 TUI live stream + scheduler 并发闸（2026-10-18，Momus review 收敛 backlog#1/#2；`bin/lint` 0 双平台；`bin/test` 3315/0/25）** — 修改文件与新增功能：
  - **TUI live event stream**（`zen-gateway/src/client/surface.rs` + `zen-cli/src/tui/chat.rs`）：additive `turn_with_recovery_with_id`（caller-minted turnId，内铸版 delegate 重试梯/bookkeeping 逐行保留）；`start_async_chat` hosted turn 前 `subscribe_events()` + forwarder 任务（biased select + terminal 事件 break + stop 后 200ms `EVENT_DRAIN_GRACE` bounded drain 关闭 pump-vs-RPC-return 竞态）将 `delta`/`tool_started`/`tool_completed` 经纯函数 `event_to_stream_token` 翻译进 tokens 流；**delta-gate 收尾**（agent 亲验发现的全量 response 双写 hazard）：forwarder 报告 saw-delta ⇒ 终文本只走 done_tx（token-compose 契约，沿 ECHO 路径先例），无事件路径回落单 token 字节等同；`🔧 <tool>\n` 标记止于工具名（collector 语法 `🔧 <tool>[ args…]` 由 parser 自渲染 `🔧 {tool}: …`——发 `🔧 {tool}: …` 会被 parse 成 `tool:` 双冒号）。钉：`caller_minted_turn_receives_session_events` + chat.rs 翻译 5 钉。
  - **scheduler 并发闸**（`zen-agents/src/scheduler/mod.rs` + `zen-core` CronConfig）：`[cron] max_concurrent_workers`（默认 3，clamp 1..=32，env `ZEN_CRON_MAX_CONCURRENT_WORKERS` 非法值 E11 纪律 warn-not-silent 回落）+ `with_limiter` builder（dormant `create_default_scheduler` 保持无 limiter 字节等同）+ fire spawn 任务内 permit acquire（决策/InFlight claim 保持同步非阻塞；InFlightGuard 保证不跳过）；tokio Semaphore FIFO-fair ⇒ `*/5` 4-worker co-fire 按注册序自然错峰。钉：`limiter_caps_cross_worker_concurrency`（watch release 屏障观测 peak，无 sleep flakiness）+ clamp/env 配置钉；config 样本 `[cron]` 已注 key 文档。
- **Phase 33 Convergence（2026-10-17，`/speckit-converge` + oracle 验证；005 tasks.md Phase 33 T225-T233 append-only；006 tasks.md 16 个 W1.4/W4.5/W4.6 复选框 + W3 partial 注记）** — 门禁最终 `bin/lint` 0；`bin/test` **3296 passed / 0 failed / 25 skipped**（基线 3282，+14）。修改文件与新增功能：
  - **T225（CRITICAL, Constitution XIV）敏感度路由**（`crates/zen-agents/src/orchestrator.rs` `resolve_completion_provider` + `zen_agent.rs` `build_with_provider`、`completion_model.rs` "mock"→`Provider::Mock`）：Private/Confidential 生成路由优先本地（此前恒用云 default）；无本地可达 ⇒ 显式报错（`Provider::Mock` 为 executor.rs:184 离线替身仍允许）；Public 回落 default + warn-once。测试 `private_session_errors_when_no_local_provider_is_reachable` 等。
  - **T226（HIGH）会话水合**（`crates/zen-gateway/src/server/hosting.rs` `hydrate_session` + `tests/session_hydration.rs` ×3）：daemon 重启/idle 后 `session/start` map-miss 从会话 jsonl 回填 ≤40 轮（`HYDRATION_MAX_TURNS` 常量）；fail-open；活会话不二次水合。
  - **T227（HIGH）`--agent` 合约**（`hosting.rs` `resolve_agent_name` + `orchestrator.rs` 移除两处 sticky 提升写回）：registry 有效名覆盖路由、未知名结构化错误、空名 ladder 字节等同。
  - **T233（HIGH）W3 shadow 观察者接线**（`decision/mod.rs` 三 `run_*_shadow` 在 orchestrator execute/execute_stream + delegate run_single 完成点发射）：`rung:"shadow"` record-only，never-mutates 钉保持绿。
  - **T228/T229/T231/T232（UX 批）**（`zen-cli/src/tui/inline_handler.rs` Ctrl+C 流中取消复用 `session/cancel` RPC、二按退；`tui/chat.rs` 无 provider 错误 banner 追加 `Tip: zen provider …`；`cmd/wiki_command.rs` `zen wiki list` walkdir 全树分组枚举（walkdir 加入 zen-cli workspace deps）+ 空状态 Tip；`zen-agents/src/scheduler/mod.rs` zen-loop 注册 config 失败 `warn!`）。
  - **T230 文档**：README/README_zh quickstart 补 `zen serve start` + zh 探针数 8→10 订正。
  - **工作流纪律**：3 个 general agent 文件集互斥并行；主会话独占 cargo 门禁；复选框已翻转（closure 说明 + `tasks.md` "Oracle verification" 行含 2 个 FIX-NOW 修法与 7 条非阻塞 follow-ups）。

- **O16 + O17 + O18 生产实测三修（2026-10-17 续；报告 §9.5/§9.6）** — `bin/lint` 0；`bin/test` 3271 → **3279 / 0 / 25**。修改文件与新增功能：
  - **O16 查询侧 degraded 可见性**（`zen-vault/src/search/`、`zen-gateway/src/server/knowledge.rs`）：`SearchDiagnostics{query_embedding_degraded, query_embedding_unavailable}`；`search_fused`/`search` 变字节一致薄 wrapper（逻辑在 `*_with_diagnostics`）；降级去重并入既有 G4-01 `degraded` 数组复用 TUI 渲染器；健康路径无 `degraded` 键。
  - **O17 tier-3 vec0 语义检索修复**（`crates/zen-repo/src/embeddings_repo.rs`）：①投影改 `nm.file_path AS file` + 去 contentless FTS join + Rust 侧 `read_canonical_snippet`（`notes_repo.rs`，改 `pub(crate)`，共享 `SNIPPET_FILE_MAX_BYTES=256KiB`）回填内容；②`insert_entity_embedding` 改 delete-then-insert（`INSERT OR REPLACE` 在 vec0 必失败）；3 个 vacuous vec0 测试换真断言。
  - **O18 移除 RLIMIT_NPROC=50**（`zen-core/src/sandbox.rs` + `tests/resource_limits_test.rs`）：软 NPROC 按 real UID 全会话计数 ⇒ 生产 daemon spawn 全 EAGAIN（rg/shell.exec 皆不可用）；改保留 NOFILE=256 + CORE=0。教训：O17/O18 同为「测试全绿但生产不可用」（vacuous 断言 / harness skip）。
- **state.db 索引累积根治 + 清理 + O13-O15（2026-10-17；报告 §9.1-§9.4）** — 生产 `state.db` 948 MiB → **16.7 MiB**（notes_meta/FTS 全部归真，二轮 reindex 零增长）。修改文件与新增功能：
  - **O7**（`reindex.rs`）：`EXCLUDED_DIR_COMPONENTS=["iterations"]` 按路径组件排除版本快照（曾占索引 95%）。
  - **O8**（`notes_repo.rs` + migration 011）：`index_note` 确定性 rowid（sha256 前 8 字节）治 rowid churn；迁移置 `contentless_delete=1` + 删除触发器改按 `old.rowid`；`--rebuild-fts` 修为 delete-all + 强制全量回填。
  - **O9**：新增 `zen wiki reindex --reset-embeddings` —— 进程内 DROP+重建 vec0 并同批清 `content_hash`（中断自愈）；**运维规则：必须先停 daemon**。
  - **O13（未修，非紧急）**：`auto-{stem}` id 回退跨目录同名冲突（生产 27 对），不造成 DB 增长；方向 = id 带路径判别。
  - **O14（选项 A+C）**（migration 012 `notes_meta.embedded_hash` + `NotesRepo::{mark_embedded, all_embedded_hashes}` + 新 worker `scheduler/workers/embedding_refresher.rs`）：向量新鲜度可检测（degraded 不标新鲜、行保持脏）；30min refresher worker 探针先行、degraded 即中止；审计 `loop.embedding.refresh` 仅在有事件时发。
  - **O15**（`zen-provider/src/embeddings.rs` tagged 计数 + zen-core 配置）：生产/测试切 `[embeddings] local_provider="ollama"`（qwen3-embedding 4096 维，此前全是 hash 伪向量）；`degraded` 计入 reindex 报告/日志/CLI。**运维规则：CLI 重嵌入必须 proxy-clean（死 Privoxy 劫持 loopback）**。
  - **运维陷阱**：launchd KeepAlive 会在 stop 后数秒重启 daemon ⇒ 动 state.db/换二进制前必须 `launchctl bootout gui/$UID/dev.zen.serve`（后 `bootstrap` 恢复）；`pgrep -f "zen serve start"` 为空才能动 state.db。
- **daemon 无限挂死双根因 + O19 + 部署修复（2026-10-16/17；报告 §9.5-§9.7）** — 修改文件与新增功能：
  - **A**（`zen-provider/src/embedding.rs`）：ollama embed 走 `reqwest::Client` no_proxy + 240s 超时（此前裸 Client 被死代理 stall，无限阻塞）。
  - **B**：fastembed init 改独立线程 + 60s `recv_timeout`（hf_hub/ureq 经死代理静默 stall，`catch_unwind` 捕获不了 hang）。
  - **C**：`embed_with_providers` 先 clone 再调用（跨 provider 持锁 ⇒ 全量 embed 永久阻塞）；回归测试峰值并发。
  - **O19 frontmatter 告警洪水（896,591 条 → 0）**（`zen-vault/src/note.rs` + `reindex.rs`）：`parse_frontmatter` 容忍 YAML 块序列行；新增 `has_frontmatter`，无 frontmatter 降级 `debug!`（纯 markdown 合法）。曾级 P3。
  - **部署**：`cargo install --path crates/zen --force --locked`（不带 `--locked` 必失败：zip yank 链）；launchd plist 必须指 `~/.cargo/bin/zen` 等稳定路径（`current_exe`→target/ 被清理会 crash loop）。
  - **次级**（`zen-cli`）：B13 `estimate_tokens`（`chars().div_ceil(4)`，修 `✓ 0 tokens`）；B7 reverse-i-search 抽共享 `tui/history_search.rs`，全屏/inline 双路接入。
- **tier1 rg 混合 + 路径锚定 + 并行 dispatch（2026-10-16）** — 修改文件与新增功能：
  - **P1 CJK panic**（`zen-vault/src/distill/pipeline.rs`）：`auto_link_wikilinks` 全 char 化遍历（中文笔记 char-boundary panic 每周期杀死 zen-loop）。
  - **P1 hypothesis 爆炸**（`graph_verify.rs` `concept_page_inventory` 排除 wisdom/communities + `distill/hypothesis.rs` `MAX_HYPOTHESES_PER_CYCLE=20`）：治 `missing_entity-` 链式垃圾（生产 26,566 → 289 md）。
  - **rg spawn**（`search/mod.rs`）：`resolve_rg_binary`（env→which→well-known，OnceLock 缓存）绝对路径 spawn + `search_in_process` walkdir 兜底 —— daemon launchd 极简 PATH 下 rg 快路径可用。
  - **路径锚定**（`zen-core/src/sandbox.rs` `resolve_agent_path` + 9 个 fs 工具接线，7 测试）：daemon cwd=/ 下 `fs.read vault/...` 锚定 user_root/GLOBAL_ROOT_SUBTREES。
  - **并行 dispatch**（`orchestrator.rs` `dispatch_timed`）：`DISPATCH_MAX_CONCURRENT=4` buffered；fastembed/ort panic 守卫（`FASTEMBED_BROKEN` OnceLock + catch_unwind）。
- **搜索语义化 + slash/审计路径修复（2026-10-15）** — 修改文件与新增功能：
  - `SearchService::search`：单词语 auto 路由也走 RRF fusion（原 tier-2 才融合）；`search_fused` 增第五路 tier-1 ripgrep 内容扫描（`TIER1_WEIGHT=1.0`，未索引内容可被找到）；`/search` 空结果附 `Tip: zen wiki reindex`。数据面重建：FTS 50→108 行、embeddings 0→62。
  - 审计路径全局化（`ZenWiring::audit_log_path` → `user_root()/logs/audit.jsonl`，三处 cwd 相对默认同步修）：tool 审计此前从未进全局轨迹。slash 余量 trim（`split_slash_input`）；inline Enter-with-popup 改为补全不提交。

- **006 前沿采纳波 W1-W4 + 运行时兼容守卫（2026-10-14，`/speckit-implement`；spec D8-D19 → tasks.md 全波次）** — 触发：工具链审计（4×P1/6×P2，用户报告的 tier1/tier3 工具失败 + 多工具链断裂）+ 283× migrate 错误事故。`bin/lint` 0；`bin/test` **3214/0/25**（基线 3115，+99）。新增功能：
  - **W4 运行时兼容（D16-D18，容忍+可观测）**：`run_migrations` 改容忍迁移器（ignore_missing，checksum/Dirty 守卫保留）+ `embedded_migration_max()`/`detect_schema_skew()` pub API；doctor 探针 #10 `schema-compat`（只读、skew ⇒ 信息性 PASS、绝不迁移/取锁）；4 个 worker open 失败 fail-loud-once。旧二进制继续正常工作。
  - **W4.5 dispatch 链 P1**（`orchestrator.rs`、`zen-plugin/tools/*`）：`dispatch_timed` 每调用错误隔离 + `partition_granted_invocations` 预过滤 + 有界 `resolve_invalid_tool_call` 循环（修「≥2 无效工具名整轮死亡」「单个未授权名压制全批」）；zen-plugin 46 处 fs.* Err→Ok 结构化（11 处 sandbox 拒绝刻意保留 Err）。
  - **W4.6 P2/P3**：多轮上下文累积（`accumulated_results` 有界历史注入每轮 CallModel）；tier 注册失败 fail-loud + `tier_tools_registered()`；授权表一致性钉（tier2=12/13 非 13）；`/search` degraded 可见性 + offline 谎言修正 + 内容截断。
  - **W1 编排改进**：AB/BA 位置互换均值合并 + 种子化 rubric 顺序；委派契约 `output_format`/`task_boundaries` + `REASONING:`/`VERIFICATION:` 标记；Laplace 平滑 agent 成功率；in-loop 预算门 + stream pre-loop 预留（G5-02 关闭）。
  - **W2 结构升级**：plan.execute `on_failure: retry|skip|halt`（默认 skip 字节等同）+ 回滚记忆 `wisdom/rewinds/` + artifact 输出路径约定（sandbox 校验 + fs.write 授权硬门）。
  - **W3 影子观察者（D13，零阈值零激活）**：QE cascade / Bayesian self-escalation / Minority Sentinel 三观察者 `rung:"shadow"` record-only + never-mutates 钉。
- **docs/designs 收敛为 architecture.md（2026-10-13）** — 六份设计文档（1067 行）并入 `docs/designs/architecture.md`（834 行，git 跟踪；§1 总览 · §2 框架 · §3.1-3.10 十子系统 · §4 跨切原则 · §5 决策注册表 · §6 时间线 · §7 源索引）；13 处活引用改指。specs/<feature>/ = 历史原样保留。
- **006 spec 收敛 + D1-class async 路由 hazard 修复（2026-10-13）** — 006 最后开放项关闭，spec Draft→Implemented + C1-C5 Convergence Record；`bin/test` 3113→**3115/0/25**。新增功能：
  - `OllamaProvider::health_check` 改 dedicated-thread 探测（原 `reqwest::blocking` 在调用线程 ⇒ Confidential 路由 panic；单点修复覆盖 delegate run_single / orchestrator / zen_loop / llm_distill / provider_command 全部 7 条 async 链）。
  - `impl LlmRouter for DefaultRouter` 增 `is_local_llm_available` override（接线 T170 local-hop + decision L1 双 rung；τ 门后今日字节等同）+ 钉 `zen-provider/tests/local_llm_probe_dyn.rs`。
- **Phase 32 审查修复（T219-T224，2026-10-11 `/speckit-implement`；oracle 6 项裁决全落地）** — `bin/lint` 0；`bin/test` 3088→**3109/0/25**（+21）。新增功能：
  - **T219**（`zen-agents/src/scheduler/workers/memvid_indexer.rs`）：夜间索引切 `index_incremental`（mtime-ms checksum sidecar `memories/.index-checksums.json`，缺失回退全量）——治每晚全语料重复追加；未用 `dedup(true)`（会阻断 provenance）。
  - **T220**（`orchestrator.rs`）：pre_supplied 判据收紧为 `has_substantive_knowledge`（≥1 条非空 content）——空 tier-2 笔记不再假阳性压制 knowledge_inject 门。
  - **T221**：additive `retrieve_with_entity_context_and_cards` —— enriched 路径在同一 fail-closed 门下跑同一 `increment_memvid_injection_rewards`（enriched 强化不再饿死）。
  - **T222**（`zen-repo/src/notes_repo.rs`）：tier-2 canonical content —— `NotesRepo::search` 按行从 `nm.file_path` 读 canonical 文件回填 snippet（contentless-FTS 结构性空内容的治本；共享 `SNIPPET_FILE_MAX_BYTES=256KiB`），四处消费者受益。
  - **T223**（`zen-gateway/src/daemon.rs`）：`memory/rebuild` 翻转 `.with_provenance_metadata`（同 `[agentic.memory_strength]` 门）。
  - **T224**（`zen-core` 配置 + 双 rank 站点）：`[agentic.memory_strength] strength_floor`（默认 0.0 字节等同，clamp 0..=1，env 同名缩写）—— `score × max(R, floor)`；`memory_strength_half_life` → `memory_strength_rank_params()`。
- **Phase 31 `/sdd-review` 六阶段深审（2026-10-11；对象 wave T206-T218）** — 三轨盲推 × GAN 对抗 × oracle 裁决 × Munger 审计；工件 `docs/specs/005-agentic-loop/review-2026-10-11-phase31.md`。核心 findings：R1（P1）夜间 memvid-indexer 每晚全语料重复追加（`index_all` 无 skip + `dedup:false`）→ T219；R2（P1）contentless-FTS 空 content × pre_supplied 合取压制唯一默认 ON 门 → T220/T222；R3（P1）enriched 路径饿死强化 → T221；R4 strength_floor → T224；R5 rebuild 丢 provenance → T223。教训（Munger）：测试全绿 ≠ 用户可见效果；先修数据层缺陷再建特性。
- **Phase 31 记忆强度 × 知识波（T206-T218，2026-10-11 三 wave 落地）** — 设计 `docs/designs/memory-strength-knowledge-wave.md`；owner 决策 D-F 修订 knowledge_inject 默认 ON、D-G 单一 search_rerank 门、D-H fan-out 上限 8。新增功能：
  - **Wave 1**（`zen-memory/src/strength.rs`）：`[agentic.memory_strength]` 配置节；纯函数派生强度 `R=0.5^(t/(HL×(1+log2(1+n))))`（读时派生零存储零迁移）；indexer additive `source_path`/`tier` provenance + `select_cards_with_strength`。消费者契约：以 source_path/tier 存在性判定，绝不以 metadata map 空判（memvid-core 注入自身键）。
  - **Wave 2**：gateway `memory/search` `score × R` 重排 + additive `strength` 锚点链（journal-uri > source_path mtime > frame created_at，不可解析 = R=1.0 中性）；T210 indexer 翻转 `with_provenance_metadata` + bottom-N `logs/memory-strength-report.json`（REPORT-ONLY 永不删除）+ 审计 `loop.memory.strength` + `zen discover report` additive 节；T212/T213 SkillLoader 三根发现（zen-native > 项目 > `~/.agents/skills`）+ `zen wiki export-skill --target zen|agents`（派生共用 `zen_core::paths::agents_skills_under`）。
  - **Wave 3**（`orchestrator.rs`/`delegate_task.rs`/`plan_task.rs`）：T214 `inject_knowledge_context`（双入口 intent ladder 后；guards=pre-supplied skip/Conversation 跳过/tier 名称契约映射/4000 字符预算/超时 fail-open/InputSanitizer）；T215 `SubAgentContext` + `compose_sub_session` 单缝（`context_inheritance` 门，预算 base>>depth）；T216 fan-out 硬上限 `FANOUT_MAX_TASKS=8` 结构化拒绝；T217 refund-on-early-error 回归钉；T218 resume 审计 additive `replayed_checkpoints`；T211 `retrieve_memories_structured` 写 reward 增量（uri-scoped 词法归属，m3/m4 miss ⇒ 保守欠计）。审计线：`loop.knowledge.injected`（每 gated turn 一行，含 `mode`/`skipped_reason`）。
- **Workspace-path 兼容 + gateway 升级管理 + 特性波设计（2026-10-10，四维审计落地）** — spec 186/186 tasks、FR 40/40。新增功能：
  - **T195**（`zen-memory/src/memvid/indexer.rs`）：MemvidIndexer 重扎根 Path Spec v2 —— 显式 `(memory_root, wiki_root)` 双根 + M3 递归 walkdir（此前夜间索引器生产静默 no-op：不存 workspace_root 能让 v2 两组 join 同中）。
  - **T196/T197/T199**：`build_env_info` 标签更正（`Git branch:`→实际 session、`Model:`→agent）；workspace_id 标签 None ⇒ 回落 `"global"`（全 workspace 零读者，审计证实）。**T198/T200**：`ZEN_WORKSPACE` env override；launchd daemon workspace_root=None = 正常 hosted 形态（全局树全功能，hosted turn 不支持项目 workspace 写入）。
  - **Phase 30 升级管理（T201-T205）**（`zen-gateway` pid 记录 + `zen-cli serve`）：pid 记录 additive `version`/`protocol`；`zen serve status [--json]` Binary vs Daemon 双版本 + STALE 标记；`zen serve restart`（launchd kickstart / drain stop + StartupLock）；`[gateway] upgrade_policy=warn|auto-restart`（默认 warn，三入口诊断）；契约 00 §Addendum + 升级 runbook。G6 独立 updater 推迟。
  - 同日完成 Phase 31 特性波设计（5 owner 决策待答 → 次日落地，见上）。
- **eng review 第二轮深读（2026-10-02）** — 5 个高风险 seam 逐行深读：**P1** `llm_distill.rs` token 补账在 `consume_tokens` 成功分支内 ⇒ 少报消耗 —— 改为无条件上报真实用量（记账=问责非闸门；+2 记账测试）；`replay_stale_transactions` "1h guard 保并发安全"为假陈述 —— 注释如实改为崩溃进程后备（真闸门 = 调度器 lease）。4 项（树索引 return、扩展名谓词、normalize_aliases、apply_tui_env）经审计确认无误。`bin/test` **2984/0/25**。
- **分支全量 eng review 修复（2026-10-02 `/plan-eng-review`，36 commits/70 files）** — 2 P1 + 7 P2/P3 全修复；`bin/test` 2975→**2982/0/25**。新增功能：
  - **P1 cached-token 计费**（`zen-provider/src/model_meta.rs`）：`CachedTokenUsage` 四桶显式类型 + `cached_usage_to_cost_usd`（uncached 1x / write 1.25x / read 0.1x）——ModelMetadata 单一 input_cost 结构上无法表达 Anthropic cache 倍率为根因；tokens 与 cost 语义分离。
  - **P1 orphan 删除护栏**（`zen-vault/src/distill/cascade.rs`）：删除先 capture 进 PageIterations（可经 `zen wiki rollback` 恢复）+ 两轮扫描宽限（首次只 capture）——原 liveness 判据在 vault 不可达时会批量删除整个已编译 wiki。
  - **P2/P3**：`complete_cached` 120s 超时；6 provider usage 实现抽 `used_completion`/`blocking_usage_call`（净 −105 行）；`Unavailable` 告警限流每文件每进程一次；`supports_prompt_cache()` 取代硬编码 Anthropic；计价配置告警指名缺失 key；`CACHE_BREAKPOINTS_PER_PROMPT` 生产 `debug_assert`。
- **审计遗留收尾 + rot 门禁（2026-10-02）** — AGENTS.md 命令面 3 处 "20 commands"→23 + 补 `zen sandbox` 行；`bin/lint` 门禁扩展：`cli.rs` `docs_consistency` 两测试（每个 clap 子命令必须在 `docs/src/cli-commands.md` 反引号列名 + "N subcommands" 计数限定在 COMMANDS section）+ `zen-core/tests/config_documented_keys_test.rs` sentinel 往返（高流量配置键的值必须真实落地）。E1 writer 清单补第 20 条目（PageIterations::restore）+ "13 writers" 订正为 20。
- **设计验证审计 + 两个 P1 修复（2026-10-01）** — 25 项设计确认落地；`bin/test` 2968→**2973/0/25**（+5）。修复：
  - **P1 pandoc sidecar 不可达**（`stage_host_dir` 暂存 epub/odt/rtf 但 `promote_incoming` 不收）：`promote_incoming` 改复用 `is_convertible_extension` 唯一谓词（列表各自维护即漂移根因）；`ConvertError::Unavailable` 留暂存待重试、`Failed` 终局隔离。
  - **P1 OKF lint 验收永不可达**（`render_page` 从不输出 description）：`Note`/`WikiPage` 增 `description`（frontmatter 优先，否则 `first_substantive_line` 派生）+ `type` 回退笔记自身分类——让编译器合规而非削弱 lint。



- **Provider usage plumbing + T138 decision memo（2026-09-30，commit `3aa4eb0`/`cfe3e6a`）** — `zen-provider` 三桶 `SyncUsage`/`UsedCompletion`/`reconcile_usage` + `call_with_usage`（rig 0.42 已归一 usage，6 provider 此前丢弃）+ `complete_metered`（真实 echo 计费）；`completion_model.rs` 接入 ⇒ AgentRun `run.usage()`/T096 预算非流式路径真实化。T138 memo：`KnowledgeSearchMode` 只门 TUI 注入一处；新增 `ZEN_TUI_KNOWLEDGE_SEARCH`（fast|full|off，最后无私有 env 面的键）；follow-ups C/B 记录待行。
- **Plan-eng-review remediation（2026-09-30，`c6067d9`/`b344472`/`093e68a`）** — ① docs-rot 门禁：`cli.rs docs_consistency`（每 clap 子命令必须反引号列名于 `docs/src/cli-commands.md` + 计数一致）+ `zen-core/tests/config_documented_keys_test.rs` sentinel。② NFC alias 回填（T143，`NotionsRepo::normalize_aliases_pass`，挂 `zen wiki reindex` 顶——re-extraction 不会自愈 NFD 行）。③ distill 成本可见：`LoopBudget.consumed_cost_usd` + `charge_cost`（无条件记账）+ `call_llm` 返回 `(reply, tokens, cost_usd)` + `LoopCycleReport.llm_cost_usd`——`[cron] llm_cost_cap_usd` 触达真实成本；bytes/4→真实 echo 记 TODOS 为延迟 P3。
- **All-format doc ingestion（2026-09-29，`1e8d73c`/`c5d8123`/`172f2bb`）** — `zen-vault/src/ingest/convert.rs`：`pdf_to_markdown`（`pdf-extract` 0.10 同 minor + `%PDF` 门 + `catch_unwind`）、`office_to_markdown`（`office_oxide`；legacy .doc/.xls/.ppt 显式拒）、`pandoc_to_markdown`（epub/odt/rtf，30s deadline poll）；统一 dispatcher `convert_to_markdown`/`is_convertible_extension`。`promote_incoming` 提升时转换 → frontmatter 包 `.md`；`Failed` 终局隔离、`Unavailable`（pandoc 未装）留暂存重试。纯文本路径字节不变（非 lossy）。`bin/test` 2951/0/25。
- **T046 LLM distill 落地（2026-09-29，`1e5d717`/`a9855aa`）** — `stages/llm_distill.rs`：enrichment-only bounded 提取（失败=byte-identical）；`[agentic.cache] breakpoints` 接入（Anthropic → `complete_cached` 真实计费）+ `[agentic.loop] distill_allow_cloud`（env `ZEN_LOOP_DISTILL_ALLOW_CLOUD`，默认 false）；merge assist fail-open。`tree_index.rs` 消费者 + `[agentic.loop] tree_index_pages`（默认 20，clamp 5..=200）——超阈值笔记按 heading 分节有界蒸馏；预算耗尽用 `return` 非 `break`。
- **Compile-hygiene Phase 1-4（2026-09-28，E⑤E1⑥ / ④E2E3 / ①②③ + E5E7，设计 `docs/designs/openkb-compile-hygiene.md`；`architecture.md` 现承）** —
  - ①（`0ac2a8f`）OKF lint：缺 `type:`/`description:` 页报告（advisory），`[agentic.compile] okf_lint` 默认 true。
  - ②（`61b98a6`）`WikiCompiler::compile` 两遍：whitelist（NFC `normalize_alias`）+ ghost `[[link]]` strip（`[agentic.compile] ghostlink_enforcement` 默认 strip）；审计 `loop.compile.ghostlinks`（零 ghost 不发）。
  - ③（`2785d9d`）`zen-provider/src/cache.rs` `CacheSegments::build` 三累积 checkpoint；`AnthropicProvider::complete_cached` 裸 `/v1/messages` 绕过 rig（client 无 cache_control API）⇒ cache_creation/read echo。
  - ④（`b158003`）crash replay：`RecoveryManager::replay_stale_transactions` ≥1h 旧 `.txn-*.jsonl` 回滚（age 是崩溃后备，真闸门 = scheduler lease）；txn 文件 age-delete@7d。
  - E2（`3d55a36`）`WikiPage.sources` frontmatter（code-managed，LLM never writes）+ lint advisory Source Provenance。
  - E3（`7bf3dc4`）`cascade.rs` delete cascade：`redirect_page_source`（archive hook txn-tracked）+ `sweep_dead_sources`；安全契约 = 无 `sources:` 键的手写页永不触碰、`sources: []` 保留。
  - ⑤（`6cb448e`）`skill_trigger_eval.rs` report-only 双指标 grader（trigger_accuracy/body_alignment，复用 `score_trigger` trigram）⇒ `zen discover report` `skill_eval` 节。
  - E1（`ecf42aa`）semantic lint：`[agentic.lint] semantic`（默认 false 门关）+ `tindy/semantic_lint.rs`（40 页×2000 字符 digest LLM 审计，fail-open 全链）+ writer 清单设计 `docs/designs/e1-semantic-lint-writers.md`（只读决策）。
  - E5（`e8ef9e2`）PageIterations：覆写前 capture 旧字节进 `vault/iterations/`（wiki 目录外—— sechs 消费者枚举 wiki 目录）+ diffy sidecar + `zen wiki rollback [--list][--to]`（rollback 自身可逆）；retention 90d。
  - E7（`697daee`）`zen wiki export-skill` → Agent Skills 技能文件（`~/.zen/skills` / `--target agents` → `~/.agents/skills`；240 条上限 + 截断标记）。
  - ⑥ tree index 与 T046 一起落地（无消费者不建原语）。
- **Agentic TUI polish（2026-09-22..24，002-agentic-tui）** — slash popup registry 组头 + 粘贴 pill（>3 行 `[Pasted N lines]`）+ `● working Ns` footer + Esc-dismiss DismissedToken + Ctrl+J 换行；流间隙 >200ms `tui.stream.stall` 审计；codex v0.155.1 对齐选择语法（exact 优先/匹配粗体/描述列稳定）；diff fence 红/绿渲染（`+`绿`-`红`@@` cyan，NO_COLOR-safe）+ PageUp/PageDown 钉。
- **四维 review + adversarial（2026-09-21，Phase 28 T190-T194；`bin/test` 2779/0/21）** —
  - **T190 retention**（`distill/retention.rs` + 每日 `RetentionWorker` `0 30 3 * * *` 双 profile）：audit.jsonl rotate@10MiB keep-3 等 + 各目录 age-delete（outbox>30d/journal>90d/virtue_logs>365d...）+ 驱逐 culled-beliefs keep-newest-200 + **quarantine 永不删用户数据**；`[agentic.retention] enabled/dry_run`。
  - **T191**：`zen clean all/trash` 门禁 y/N 提示（销毁 ~/.zen 外数据）；`--yes` 绕过。
  - **T192**：`config/config.toml` 剔除从未解析的键（13× `[agents.*]`、`[sandbox.rlimits]` 等）+ spec Configuration Compatibility Record。
  - **T193/T194**：README 撤销陈旧命令教学；`open_versioned`/`probe_latest_version` 按 owner 决策保留为降级路径。
- **Phase 27 closeout（2026-09-20，`bin/test` 2763/0/21）** — T180 placeholders.json flock 存储（`placeholders_store.rs::merge_save_placeholders`，union-by-slug merge）；LLM 成本真实化（`usage_to_cost_usd` + `WorkerCostLedger` `<logs>/worker-costs.json` + `complete_metered`）；T187 `zen wiki loop gaps` 暴露 user_questions；T189 doctor 第 8 探针 outbox（现已 10 探针，见 2026-10-10/17 条）；T185 SC-002/SC-005 度量（`attempts` 字段 + `status_read_ms`）；T186 相对路径测试泄毒修；T183 phantom WhatsApp/Telegram 配置删除。
- **多实例并发加固（2026-09-20，Phase 27 T175-T185；`bin/test` 2731/0/21）** — ① StartupLock（`daemon.rs`，bind 前取锁，TOCTOU 关闭，per-ZEN_HOME）；② pid 写入权移 daemon（CLI pre-write 删除，start 幂等预探测）；③ `acquire_migration_lock` 跨 `open/open_lazy`（sqlx `Migrator::lock` 是 no-op——load-bearing 事实）（`zen-repo/src/client.rs`）；④ `zen-core/src/atomic_file.rs::write_atomic`（tmp→fsync→rename）接线 9 站点。测试：`startup_lock.rs`/`atomic_file_tests.rs`。
- **Review-remediation（2026-09-19，Phases 22-26 共 60 项闭 52；后续 pass 2610/2722/2731）** — 两大 P1：belief 半衰幂等（`last_decayed_at` 锚）+ scheduler lease 饥饿（`health/status` additive `scheduler_pending` + TUI 让位 + daemon 30s 重试翻转 `scheduler_live`）。判读层修正：FR-036 解析结构化 error 字段 + 逐 dispatch 计时；correction 前导用户从句 + citation 指纹；死 entropy 腿删除（blast radius = Confidential 单驱动）。FR-035/040 闭环：wake-up brief 生产 writer + 256KiB 帽 + 读点 sanitize；`recurrence_count` 写者补齐。信念读侧分档（T129/T130 裁决）：`Belief::reliability() = min(provenance, content)`（无证据 = `UNPROVEN_PROVENANCE_WEIGHT=0.1`）+ `memories/demoted-beliefs/` 读回为 M2 工作层（无写侧门——证据面不空）；`Belief::noisy_or_candidate()` 为 surfaced probability 非 gate。robustness：`[agentic.loop] max_ingest_bytes`（默认 64 MiB）+ 原始字节 hash + MAP-elites 原子写 + `shell.exec kill_on_drop`。删除 11 个 on-caller 原语（grep 验证）+ `prune_beliefs` → `prune_stale_from_prompt`。
  - 同日 follow-up pass 新增：T173 calibration harness（`distill/decision_audit.rs`：`logs/decision-audit/dataset.jsonl` + labels 接口 + ECE/Brier 仅对标注记录 + `zen discover report` calibration 节）；T140 前置 = `discovery_tree.rs` 结构化日志（append-only jsonl，5b/5c/arena/rejection 全接线，记录 `incumbent`）+ `discovery_replay.rs` 零执行 argmax 评分器（advisory only，thin data ⇒ 显式 insufficient）；T141 双件：migration 010 `notion_communities` + `louvain_communities`（确定性、recompute-replaces）+ `run_community_summarization` → `wiki/communities/` + zen-loop Stage 4b（`[agentic.loop] community_resolution/min_size`）。
  - **L1 calibrated decision layer（T168-T174，`416e7b1`/`09c682c`/`f952ce7`/`073e0b4`）** — 每个阈值 τ 取自 calibration 工件（`thresholds.json`，T173）或显式 config override；**`None` = 门关（字节等同 pre-T168，逐 case 钉测试）**。T169 阶梯 `decision::classify_intent`（embedding router → LocalClassifierRung → LlmRung 0.7 门保留）；表达力反转修复（rung verdict 带可选 signal，validate against `INTENT_SIGNALS`，13 agents 全 rung 可达）；T170 de-anchored judge（`review/deanchored.rs`——先承诺后见稿，2607.05904）；T171 `classify_binary`（no-τ ⇒ not-called，T161 heuristics 未退休）；T174 vendor gate（`distill/vendor_eval.rs` 四轴，缺数据 ⇒ `insufficient_data` 拒绝伪造）。新 config（全默认缺省 = 门关）：`[agentic.intent] l1_threshold`、`[agentic.review] escalate_threshold`、`[agentic.classifiers] {correction,citation}_threshold`。
  - **标签供给 + 校准推导 + 基线（`ba05b3b`/`03daf0d`/`cd5e3e1`）** — `select_threshold`：Clopper–Pearson 单侧上界 + **Bonferroni** γ=δ/K（预固定 K=8 网格 `{0.00,0.15,0.30,0.45,0.55,0.65,0.80,0.90}`，2607.04430/2110.01052；**Wilson 反保守 + stop-at-first-rejection 非固定序列——两项复现被否**）；双卫 `UninformativeConfidence`（lift<0.05 不发 τ）+ `CannotMeetTarget`；certification 实际 ~120 labels 起步。`baselines.rs` 四基线（intent p95/LLM-intent 速率/routing 分布/frontier 速率，<30 ⇒ `InsufficientSample`）。标签：opt-in `[agentic.audit] decision_excerpt_chars`（默认 0 关，clamp ≤500，换行拍平防 JSONL 伪造）+ `zen discover label status|next|set`（`append_label` 校验存在 id + 词表 canonicalize）；`classify_binary` 每调用发一行 `loop.decision` calibration 记录（rung L1 带 confidence/L0 无）；ladder 记录补 `decision_kind` 键（否则 L1 记录永久不可见）；audit 置信度 `audit_score` 舍入。`zen discover calibrate [--write]` + report threshold/baselines 节。
  - **Linux CI 可移植（`09c682c`）**：zen-cli launchd helpers 真平台门控（非 macOS `-D warnings` 下 dead-code 编译失败的隐性缺口）。
- **V13 增补 A — 校准决策层/System 1 阶梯（2026-09-19，001 V8.md + 005 Phase 26）** — 触发 Jev/TypeSafe launch + 47 决策点清点。核心：001 原设计即双系统，V13 中间层落为 keyword/trigram heuristic = 全部判读 bug 集散地；只有 intent 分类与 SemanticReviewer 是 S1-shaped-but-LLM。目标架构 L0 确定性 Rust → L1 校准层（embedding router + local classifier，`DecisionFn` trait）→ L2 trust-or-escalate cascade（de-anchored per 2607.05904）→ L3 System 2 不变。校准不变量（V13-A.3）：永不只凭口头置信度门控；分类器门不取代 seatbelt；学习型 router 保 fallback + 滚动重校准；可证伪基线（intent p95<500ms、LLM-intent<10%、cloud review −70%）。
- **全架构 deep review（2026-09-19，GAN adversarial，17 findings/2 P1）** — T153（P1）belief 半衰每周期复利；T154（P1）scheduler lease 饥饿 daemon；假闭环纠正（FR-040 无 writer、FR-035 boost 无写者）；FR-036 遥测倒置（`starts_with("Error")` 漏结构化错误）；验证无损维度已记录（migrations 005-009 additive、004 不变量、006 七机制）。前沿裁决入 spec Deep Review Record（min(provenance,content) 信念可靠度 + de-anchored judge + HippoRAG-2 + UX 反 pattern）。修复 = Phase 25 (T153-T167)。
- **Review backlog 入 spec（2026-09-19，Phase 23 T130-T144 + Phase 24 T145-T152）** — 功能缺口（T130-T132 belief 阈值/evidence_gatherer 无写者/demoted-beliefs 无读者）、死原语（T133-T137）、可达性（T138-T139）、缺子系统（T140-T141）、spec 决策（T142-T144）；Phase 24 = T145 normalize_notion_name 双规范形、T146 零调用者、T147 `prune_beliefs` 名不副实、T148/T149 零 caller wire-or-delete、T150-T152 文档矛盾。收敛 pass：22/22 probes PRESENT。
- **RSI/frontier research 入 spec + wiring audit（2026-09-19）** — 七系统（AlphaEvolve/DGM/Voyager/Reflexion/SPIN-Self-Rewarding/Letta sleep-time/Dream-RSI 2609.14858）入 spec Research Record；Wired 验证：RRF+PPR、Reflexion 5b/5c、Archive refresh/prioritize、skill 沉淀、Letta 式闲置整合；Dead 发现：select_parents/stepping stones/embedding scorer 零生产 caller、community 检测缺席、Dream-RSI replay 前置 = discovery-tree 数据纪律。可达性注记：默认 TUI-chat 是 Fast mode ⇒ 不走 fusion；fusion 只在 gateway auto、`zen chat`、`/search`。
- **Spec re-analysis remediation（2026-09-19 `/speckit-analyze`，`bin/test` 2550/0/21）** — T121 load-harness（`bin/load-harness` + `crates/zen/tests/load_harness.rs`，100 笔记 60min 墙；发现 T041 假完成）；T122（CRITICAL）pending-pool 搁浅治本（`requeue_pending()` 回收 `archive/pending/`，审计 `loop.pending.requeued`）；T123 `[agentic.loop] max_steps/max_tokens`（默认 10/8000）⇒ 实测 100 notes ~18k/hour；T124 NFC 进 canonical `normalize_alias`；T125 `render_skill_md` `## Gotchas`；T126 migration 009 `note_stages`（成功提交后才写；独立于 notes_meta 免被 reindex 重置）；T127 `loop.note.archived` 审计三元组。T128 virtue-log 路径入 spec（不搬数据）；T129 FR-021 阈值未强制（→T130 裁决）。
- **Multi-agent orchestration fusion（2026-09-07，006）** — `delegate.task`（D1-D3）：真 LLM sub-turn（≤4 rounds，depth-1 剥 `delegate.*` 授权，`[agentic.delegate].enabled/timeout_secs`）；Quality gate（D4-D5）：Metis→Momus→Hermes + async SemanticReviewer（HIGH blast），Momus veto 一轮反馈；`ExecutionMetadata.quotes quality_notes/delivery_ready`；INTENT_SIGNALS 营救（D6）：删 `ZenCoordinator`，keyword 表寄存；D7 反思不变。false-alarm 平反：zen-provider 六 `complete()` 一直走 dedicated-thread（`sync_complete_no_panic.rs` 钉）；真 exception = `OllamaProvider::health_check`（2026-10-13 修）。
- **001 vision convergence（2026-09-08，T372-T379）** — T372 `intent.rs` LLM-first 意图分类；T373 `[agentic.delegate].max_depth` + tier 矩阵 + task-local 链；T374 fan-out collect mode + `max_concurrent`；T375/T376 plan.execute + migration 006/`WorkflowRepo`（resume claim-fenced）；T377 blackboard superseded（ADR-001）；T378 `[agentic.orchestrator] surface`。
- **Eng-review D1-D4（2026-09-11）** — D1 review_pipeline spawn_blocking；D2 plan-DAG 数据流（upstream 输出注入下游 prompt）；D3 gate 限 tool-mutating/Confidential turn（纯 chat 跳过）；D4 intent 路由契约文档化（LLM 路径 3 类目默认 agent）。
- **V13 vision finalization（2026-09-12）+ PD-01（T112-T119，`bin/test` 2357/0/19）** — V8.md → V13 终版（force-added to git trotz gitignore）；PD-01 DECIDED A+B（native ToolCall primary + rig AgentRun 迁移）；`bin/orchestration-stats`。T112 intent LLM telemetry（`LlmOutcome`）；T113 `rig-agent` 0.41（后 T120 升 0.42 lockstep）；T114 `resolve_invocations`（native wins，fenced fallback byte-identical）；T115 AgentRun sans-I/O 状态机（zen 保留全部 I/O）；T116 output_schema ≤2 重试；T117 `discover report` orchestration 节；T118 audit 隔离（zen-core `test-support` feature + `invalidate_config_cache()` note——env 突变集成测必须显式调用）。
- **架构早期记录** — Binary/lib split（zen 13 行壳 → zen-cli lib）；config 继承 4 层（现为 5 层 global-only，见 T18/T192）；`SqliteClient` 统一数据层 + 9 域仓库（Principle XII）；5 层搜索管线；13 providers × 3 协议；13 agents × 4 tiers；`excute_command`→`execute_command` 全 24 文件更正；clap derive / rig-core / FTS5 模式文档化。
- **Architecture remediation（2026-06-01）** — LlmPreference 归 zen-core（A3）；`SensitivityLevel` 拆 AgentClearance/SafetyLevel；删 zen-llm 目录与 rig_ollama/rig_openai stubs；宪法 XI 入册。
- **CLI consolidation（2026-07-22）** — 删 `zen task` stub（`dispatch cancel` 承接）；`reindex/lint/distill` 并入 `zen wiki`；33→29 top-level。
- **CLI personal-agent scope trim（2026-08-28，29→20 commands）** — 9 手动命令退 CLI（`note/search/similar/notion-graph/research/ingest/routine/brief/dispatch`），能力内化于 ZenScheduler workers + distill loop；命令文件保留未编译可复原。
- **Phase 8 wiring（2026-09-01，FR-028/030/031/032）** — refinement queue 5c + `reverify_older_than_days`；raw graph routing 3a；placeholders 5b + `@subgraph:` 注入；self-write-aware OCC（`cas_commit` 默认 on，raw `commit_conditional` 留为库原语）；`[agentic.loop]` 新键含 `host_stage_timeout_secs`（默认 60，macOS TCC 防悬挂）。
- **In-app scheduler（2026-09-15）+ 2026-09-18 review fix** — `SchedulerProfile::{Full, InApp}`（15 员工学习核；`worker_ids()` 测试缝）；TUI probe gate（双路 kill-switch→probe→spawn）；`health/status` additive `scheduler: bool`；`[cron] tui_scheduler`（默认 true）。9-18 加固：scheduler flock lease（`scheduler/lease.rs`，probe-then-spawn TOCTOU 关闭——**测试全绿仍需**: agent Verifiable claim 收口）；reward sidecar flock + tmp+fsync+rename + `MemoryReward #[serde(default)]`；`prune_expired_sessions` 30d；RFV 反馈回路 I1/I2/I5（增量 correction call sites + loss-aversion boost 读写闭环 + wake-up brief 注入）。
- **Industry memory/RSI patterns（2026-09-18 Wave 1）** — RRF fusion（`search/fusion.rs`，k=60，dedup (file,line)，auto mode only）；MAP-elites archive（`distill/archive.rs` 三轴 cell + stepping stones，`logs/hypothesis-archive.json`）；Reflexion reflection 文件（`wisdom/reflections/`）+ `generate_from_gaps_with_history`；Voyager skill embedding scorer（injectable，`with_scorer` 单调提升）；bi-temporal 边（migration 008，`t_valid/t_invalid`，恒定 default + backfill）+ `personalized_pagerank`（共享核心，PPR 之后 I10 补 wired）。
- **Review-finding closures I8/I10/PPR/I12（2026-09-19）** — I8 arena losses staged (`stage_losses` + `staged_losses`)；I10 `Archive::prioritize` occupancy-ordered refinement queue（deterministic，空档案=输入序）；PPR wired：`Tier4Search::seeded_ranking`（MAX_SEEDS=6，PPR_DAMPING=0.85）为第 4 路 RRF list（`PPR_WEIGHT=0.9`）；I12 判读= largely FALSE（wisdom_synth/reflection 真在推理），残缺口 = dream/zen_loop data-only + state.db belief/self nodes 无生产写者（T051 单写者决策约束）。
- **Tool-loop config + intermediates（2026-09-02，T050/T054/T055）** — `[agentic.tool_loop] max_rounds`（默认 8，clamp 1..=16，env `ZEN_TOOL_MAX_ROUNDS`）；`execute_stream` 前后发 `🔧`/`✅` intermediates。
- **Gateway structural tool frames（T056/T057）** — `hosting.rs::split_tool_intermediates` pub 语法 + `emit_tool_event` 结构化帧（ring 2048/60s，可 replay）；TUI `StreamCollector` 同语法 collapsible blocks + `/tools` 开关。
- **Host governance（T043-T045，FR-033）** — `[[agentic.loop.host_sources]]` per-entry 验证 + `_incoming` staging seen-set + `promote_incoming` hash 前缀；`graph_router.rs` 双规 extractor（Code 无 LLM provenance 页 / Doc M2 Fact-M3 Concept，Belief 仍 FR-025-only）+ `ensure_raw_copy`；`resolve_model_tier` 敏感度路由（`allow_cloud` per-source，审计 `loop.host.ingested`）。
- **Personal-agentic increment（2026-09-03，Phases 11-15）** — Shift+Enter 换行（FR-019）；NotionKind Person/Preference/Temporal/Goal（F1 dedup 零新表）；skill 沉淀（trigram 0.72/max 1，`zen skill precipitate/confirm`）；morning-brief worker（outbox staging）+ MEMORY.md 200 行帽（merge+supersede）+ memory nudge 每 10 turn。
- **Hardening closeout（2026-09-04，T091-T105；2307/0/19）** — T105 parse_tool_invocations_verbose `(invocations, errors)`（never swallow）；T103 approval affinity P0（task-local `APPROVAL_TURN` 精确 match，inverted firing order E2E）；T092 BlastRadius + SemanticReviewer + `[agentic.review]` 键；T101 outbox drainer；T093/096-099 stream/token/screening 系；T091 output_schema.rs；T104 5 个缺目标测试文件创建。


## Skill routing

When the user's request matches an available skill, invoke it via the Skill tool. When in doubt, invoke the skill.

Key routing rules:
- Product ideas/brainstorming → invoke /office-hours
- Strategy/scope → invoke /plan-ceo-review
- Architecture → invoke /plan-eng-review
- Design system/plan review → invoke /design-consultation or /plan-design-review
- Full review pipeline → invoke /autoplan
- Bugs/errors → invoke /investigate
- QA/testing site behavior → invoke /qa or /qa-only
- Code review/diff check → invoke /review
- Visual polish → invoke /design-review
- Ship/deploy/PR → invoke /ship or /land-and-deploy
- Save progress → invoke /context-save
- Resume context → invoke /context-restore
