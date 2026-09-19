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
| zen-cli | CLI library (20 commands, TUI, dispatch) | `crates/zen-cli/` |
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
│   ├── zen-cli/                # CLI library: 20 commands, TUI, clap derive
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
│   │       ├── scheduler/     # ZenScheduler: cron-driven ~30s tick, 16 background workers (session-journaler, dream, memory-curator, memvid-indexer, subconscious, notion-extractor, wiki-compiler, commitment-tracker, reflection, wisdom-synth, decision-tracker, express, evidence-gatherer, promotion, zen-loop, morning-brief)
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
- **Path scoping**: Path Spec v2 — single root `ZEN_HOME` (production `~/.zen`, dev/test `~/.zentest`). Config is global-only (4-layer: Default → embedded → global `~/.zen/config.toml` → env). `.zen/` directory is project context marker + sandbox allowlist only (no config/user_data/output workspace-awareness).
- **Tests**: Integration only (no inline `#[cfg(test)]` in most crates). Custom ZenTest/ZenOutput harness.
- **Lint**: `bin/lint` → `-D warnings` + `--allow dead_code`
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
- **No CI**: No `.github/workflows` yet (release automation incomplete)

## COMMANDS (CLI)

### Development

```bash
cargo build              # Build all crates
cargo test               # Run integration tests
bin/lint                 # fmt --check + clippy -D warnings
cargo fmt --all          # Format
bin/load-harness         # SC-004 load test: 100-note batch, isolated ZEN_HOME (ignored by default)
bin/zentest              # Any zen command against ~/.zentest instead of ~/.zen
bin/release patch        # Bump version, tag, push
```

### Agentic Commands (20 commands)

Personal-agent scope (2026-08-28, 005-agentic-loop): zen focuses on the personal memory/knowledge pipeline. Nine manual commands (`note`, `search`, `similar`, `notion`/`graph`, `research`, `ingest`, `routine`, `brief`, `dispatch`) were removed from the CLI surface — their capabilities live on internally via ZenScheduler workers and the distill loop; the command files remain on disk uncompiled, restorable when business scenarios require.

| Command | Description | Dispatch File |
|---------|-------------|---------------|
| (no args) | TUI (ratatui interactive) | `tui/run()` |
| `zen chat` | Interactive LLM chat | `chat_command.rs` |
| `zen clean` | Cleanup (trash, cache, all) | `cleanup_command.rs` |
| `zen starter` | Dev tools/workspace init | `starter_command.rs` |
| `zen wps` | Work process utilities | `wps_command.rs` |
| `zen version` | Show version | `cli.rs` inline |
| `zen session` | Session lifecycle | `session_command.rs` |
| `zen serve` | Gateway daemon: start [--foreground] [--http] / status / stop / mcp / install / uninstall (macOS launchd LaunchAgent); `--http` (or `ZEN_GATEWAY_HTTP_ENABLED=1`) additionally mounts the loopback HTTP carrier `/health` + `/api/v1/{chat,agents,ws,mcp}` on the same dispatcher; SIGTERM drains in-flight turns ≤10s then cancels with audits (exit 0) | `serve_command.rs` |
| `zen agent` | Agent registry | `agent_command.rs` |
| `zen workspace` | `.zen/` structure | `workspace_command.rs` |
| `zen config` | Config layers | `config_command.rs` |
| `zen provider` | LLM provider mgmt | `provider_command.rs` |
| `zen audit` | Audit log ops | `audit_command.rs` |
| `zen logs` | Structured log viewer | `logs_command.rs` |
| `zen wiki` | Wiki ops: list, show, reindex, lint, distill, rebuild-memory, loop (run/status/gaps/enable/disable) | `wiki_command.rs` |
| `zen model` | Model metadata + routing | `model_command.rs` |
| `zen plugin` | Plugin management (install, enable, disable, rehash, tools list) | `plugin_command.rs` |
| `zen auth` | Auth/keychain ops | `auth_command.rs` |
| `zen habit` | Habit tracking | `habit_command.rs` |
| `zen goal` | Goal management | `goal_command.rs` |
| `zen skill` | Skill management (list, run, progress, show, precipitate, confirm) | `skill_command.rs` |
| `zen discover` | Self-learning gate surface (PD-06): run (one zen-loop cycle now — stages 5b/5c/5d), stage/queue, confirm/reject (Hybrid C promotion gate; BeliefEvidence applies `Belief::update` on confirm), report (discover metrics, reads `loop-last-report.json`), arena (distill regression gate vs baselines/external CLIs — every lost case is staged as an improvement hypothesis under `wiki/wisdom/hypotheses/`) | `discover_command.rs` |
| `zen doctor` | System health: 7 liveness probes (config, state.db, memories, daemon, loop, provider, vault); `--json` machine output; exit 0 all-green / 1 any-fail | `doctor_command.rs` |

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
| `delegate.task` (006/T373/T374) | inherits session | zen-agents/delegate_task.rs | Model-driven sub-agent delegation: args `{agent, prompt, description}` single-task or `{tasks:[...]}` fan-out (≤8/batch, batch width ≤4 passes the consumer gate); sub-turn runs a real LLM loop (≤4 rounds) on a sub-agent whose grants keep `delegate.*` only below the depth cap (`max_depth` default 1, clamp 1..=3, tier matrix: O→O 拒, P→P/O 拒, Specialist/Worker leaf); `spawn_allowed` + task-local depth chain; `should_delegate` gates (independent/consumer-decision/bounded≤32k chars/worth-it) logged as `loop.delegate.gates` audit lines; fan-out results are slot-ordered (results[i] ≡ tasks[i], mixed reject+run batches included); concurrency `[agentic.delegate].max_concurrent` (default 4, clamp 1..=8, env `ZEN_DELEGATE_MAX_CONCURRENT`); lazily registered at first orchestrator turn, kill-switch `[agentic.delegate].enabled`; structured Ok-error outputs, never panics the round |
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

`zen doctor` runs 7 liveness probes with JSON/human dual output (Principle IX):

```
zen doctor [--json]
  Functionality: runs 7 isolated health checks:
    1. config: load_config() succeeds (4-layer global)
    2. state.db: exists + readable (file header check)
    3. memories: memvid store under {global}/memories/ exists (count/size)
    4. daemon: gateway socket alive (UDS connect or file-exists)
    5. loop: last cycle evidence (loop-last-report.json mtime <2× interval)
    6. provider: configured provider resolves API key OR ollama-local
    7. vault: {global}/vault/ exists, git work-tree, inbox/wiki/raw counts
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
`docs/specs/006-multi-agent-orchestration/adr-001-blackboard-supersession.md`
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

- **Review-remediation implementation pass (2026-09-19, `/speckit-implement`)** — closed 52 of 60 tracked tasks across Phases 22-26; gate at close: **`bin/test` 2585 passed / 0 failed / 21 skipped · `bin/lint` exit 0** (baseline 2559). The eight remaining are design-recorded deferrals, not silent gaps: T140 (Dream-RSI replay needs a structured discovery-tree logging discipline first), T141 (community detection subsystem), T168-T171 + T173-T174 (the L1 calibrated decision layer; T173's calibration harness is the explicit prerequisite for every threshold, per V13-A.3). Also recorded as scoped-out: BeliefMem Noisy-OR candidate probabilities (only the provenance gate landed at that point; the Noisy-OR half landed in the follow-up pass below), and `NotionsRepo::{shortest_path, shortest_paths_all, connected_components}` — kept caller-less at that point on the rationale that T141 would build on `connected_components`, a rationale **disproven once T141 landed** (`compute_communities` builds its own adjacency from `load_graph_core` and calls Louvain), so they were deleted with their trait methods, `ComponentResult` and their tests.
  - **Both P1 blockers fixed**: belief half-life decay is now idempotent (`last_decayed_at` anchor; N cycles == one application — previously the full 30-day factor re-applied every 5-min cycle, collapsing any stale belief to the 0.01 floor in ~7 cycles and irreversibly renaming it into the unread `memories/demoted-beliefs/`), and the scheduler lease no longer starves the daemon (health/status gains an additive `scheduler_pending` field; the TUI watches it and yields — with `ZenScheduler::run_with_shutdown` — so the daemon's Full profile acquires the lease and the daemon-exclusive `morning-brief`/`memvid-indexer` workers actually run).
  - **Judgment-layer correctness**: FR-036 success detection parses the structured `error` field (was `starts_with("Error")`, which missed every structured-JSON tool error, so the broken-tool flag could never fire) and times each dispatch individually; correction markers now require a leading user clause and citations use a content fingerprint (both were self-poisoning heuristics, the exact self-judge reward-hacking shape); the dead entropy leg is deleted (`SemanticEntropy`, `HIGH_BLAST_ENTROPY`, both blast-radius clauses) — blast radius is driven by `Confidential` alone, and the correct revival is a calibrated classifier feeding the P8 tiers (T169, deferred).
  - **Writer-less closures fixed**: the FR-040 wake-up brief now has a production writer plus a 256 KiB cap and sanitizer at the read site; `Correction::recurrence_count`/`last_recurrence_at` now have a writer, so `high_recurrence` can cross the threshold and `write_loss_aversion_boost` is reachable (it was unreachable while the `else` branch deleted the marker nightly).
  - **Belief tier gate enforced (read-side, T129/T130 decision)**: `Belief::reliability() = min(provenance, content)` — provenance is the weakest supporting `SourceType::default_weight()`; no-evidence beliefs get `UNPROVEN_PROVENANCE_WEIGHT = 0.1`, so a confidently-phrased poison cannot pass. `zen_agent.rs` splits durable wisdom (promotable AND reliability ≥ 0.9) from the needs-evidence surface — **no write-side gate**, so the low-confidence surface never empties — and `memories/demoted-beliefs/` is read back as the M2 working tier (demotion is no longer a one-way removal).
  - **Robustness**: new `[agentic.loop] max_ingest_bytes` (default 64 MiB, clamp 1 MiB..=1 GiB, env `ZEN_LOOP_MAX_INGEST_BYTES`) with oversized-file quarantine, plus raw-byte hashing (a lossy-UTF-8 checksum collision could previously delete a distinct new note as "already archived"); host-staging writes the durable destination before the ledger (a partial write no longer stalls a file forever) and one failure no longer discards a whole batch's counts; `note_stages` filters by stage and prunes absent paths; the MAP-elites archive writes atomically and quarantines corruption instead of silently reverting to input order; `shell.exec` sets `kill_on_drop`; session prune gates on session status and logs failures.
  - **Deletions (grep-verified zero non-test callers)**: `Archive::{select_parents, sample_stepping_stones}`, `Tier4Search::{connected_components, shortest_path, synthesize_context}`, `TierSelector::auto_select`, `Tier3Search::knowledge_doc_schema_*`, `ProcessingJob`/`JobState`, `ChatImporter`, `aggregate_tool_calls`, `write_metrics`, `get_stale_episodes`, `get_frequent_entities`, `should_correct`, `update_with_method`. Renames: `prune_beliefs` → `prune_stale_from_prompt` (name now matches behaviour).
  - **Follow-up pass (same day, user-directed)**: `bin/test` **2610 passed / 0 failed / 21 skipped** · `bin/lint` exit 0 (+51 tests vs the 2559 baseline). Three more items landed:
    - **Calibration + decision-audit harness (T173)** — `crates/zen-vault/src/distill/decision_audit.rs`: extracts one record per decision from the existing `loop.turn.review` audit lines into `logs/decision-audit/dataset.jsonl`, merges a `labels.jsonl` labeling interface, computes ECE + Brier **over labeled records only**, does windowed drift, and has a first-class `labels_required` state that refuses to print a number when labels are absent; surfaced additively as a `calibration` section in `zen discover report`. This is the enabler the L1 decision-layer tasks (T168-T171) were gated on — they remain blocked only on **label supply** (real usage + human adjudication; deriving labels from the heuristics under calibration would be circular).
    - **Noisy-OR M2 candidates** (the second half of the T129/T130 belief decision) — `Belief::noisy_or_candidate()` = `1 - Π(1 - w_i)` over supporting evidence source weights (no support ⇒ 0.0, clamped), surfaced in the M2 needs-evidence prompt line as `candidate: N%`; deliberately a surfaced probability, never a gate, until a threshold can be calibrated.
    - **Discovery-tree logging discipline (T140's prerequisite)** — `crates/zen-vault/src/distill/discovery_tree.rs`: typed `Policy`/`NodeKind`/`Outcome` + real `parent_id` links, append-only `logs/discovery-tree.jsonl` (fail-open on corrupt lines), wired at zen-loop stage 5b incubation, stage 5c reverify, arena losses and rejections, and records the per-cycle `incumbent` policy explicitly so a future replay scorer can include it in its candidate set.
    - **Offline replay scorer (T140's second half)** — `crates/zen-vault/src/distill/discovery_replay.rs`: a pure zero-execution scorer over the recorded tree that selects the argmax over `{observed policies} ∪ {incumbent}`, so the reported choice is never worse than the incumbent in-sample (ties prefer the incumbent; a policy with no recorded attempts is never chosen on a prior). Thin data yields an explicit `insufficient` report naming what is missing rather than a fabricated recommendation; the in-sample overfitting caveat is documented and surfaced; it is **advisory only** (it does not auto-switch the production policy). Surfaced additively as a `replay` section in `zen discover report`.
    - **T141 (community detection) — implemented (both parts)**, after its three blockers were settled 2026-09-19: part A in `zen-repo` — **migration 010** `notion_communities` + `notion_community_members` (forward-only additive, shipped with a migration test), `communities.rs::louvain_communities` over the **undirected weighted projection of open edges** (deterministic via sorted nodes + stable community ordering; documented resolution limit and Leiden upgrade path), and `NotionsRepo::{compute_communities, replace_communities, load_communities}` with **recompute-replaces** semantics so the table cannot grow monotonically. Part B — `[agentic.loop] community_resolution` (default 1.0, clamp 0.1..=5.0, `ZEN_LOOP_COMMUNITY_RESOLUTION`) and `community_min_size` (default 3, clamp 2..=50, `ZEN_LOOP_COMMUNITY_MIN_SIZE`); the consumer `zen-vault/src/communities.rs::run_community_summarization` writes `wiki/communities/<slug>.md` via `AtomicWikiWriter` (deterministic slug from sorted members, so recomputation updates rather than duplicates) for communities **above** the minimum size only; and zen-loop **Stage 4b** `run_community_stage` between verify and reindex (so pages are indexed in-cycle), fail-open with a `loop.communities.computed` audit line. The consumer is exactly what was missing when the task was deferred — a partitioner alone would have been dead code on arrival, the anti-pattern this pass deleted eleven instances of.

- **V13 增补 A — 校准决策层 / System 1 阶梯（2026-09-19, 001 V8.md 增补 A + 005 Phase 26 T168-T174）** — triggered by the Jev/TypeSafe launch (primary-source verified: parallel sampling/RLCD/typed outputs real; 193.6×/444.6× vendor-admitted high-end; no independent eval) and the 47-decision-point inventory from the deep review:
  - **Core finding**: the dual-system split was 001's ORIGINAL design (V8 Gate semantic-entropy → Flow/Crucible + P8 four-tier complexity routing) — V13 kept the skeleton but the middle "calibrated decision layer" fell to keyword/trigram heuristics, which is exactly where every judgment-bug of the review lives (FR-036 detection inversion, CORRECTION_MARKERS poisoning, dead entropy leg). Only 2 decisions are S1-shaped-but-LLM: intent classification (4-way choice ≤20s; the LLM path reaches 3 agents vs the keyword fallback's 12 signals — expressiveness inversion) and SemanticReviewer (binary verdict, per-turn cloud call)
  - **Target architecture**: L0 deterministic Rust (guards/tier-matrix/Kahn/budget — asset, unchanged) → **L1 calibrated decision layer** (embedding router reusing vec0 + GBNF-constrained local Ollama classifier behind one `DecisionFn` trait) → L2 Trust-or-Escalate cascade (local judge → calibrated escalation, de-anchored per 2607.05904) → L3 System 2 (unchanged). Local-first by construction; no vendor dependency
  - **Calibration invariants** (V13-A.3): never gate on verbalized confidence alone (2601.07767; probes ECE 0.044 vs 0.093); classifier gates never replace the seatbelt allowlist on high-stakes actions (Claude Code auto-mode 17% FNR is the honest anchor); learned routers drift — keep embedding/kNN fallback + rolling audit-set recalibration; falsifiable baselines (intent p95 <500ms, LLM-intent rate <10%, cloud semantic-review −70%)
  - Implementation: 005 tasks.md **Phase 26 (T168-T174)** — substrate, intent ladder migration, de-anchored local judge + cascade, correction/citation classifiers, dead-entropy-leg removal, calibration harness, vendor-model eval gate

- **Full-architecture deep review (2026-09-19, `/review` GAN adversarial, 004+005+006)** — 6 parallel audits + adversarial pass, 17 findings (2 P1 blockers), verdicts vs the 2025-2026 frontier, and SPEC updates landed:
  - **T153 (P1, pre-existing, blocker)**: belief half-life decay compounds every 5-min cycle (`belief.rs:277-289` never advances its anchor, no idempotency guard) — any belief ≥30d stale collapses to the 0.01 floor in ~7 cycles (~35 min) and is irreversibly renamed into the unread `memories/demoted-beliefs/`; the whole M4 wisdom layer self-destructs with the loop enabled, no error surfaced
  - **T154 (P1, blocker)**: scheduler lease lets a TUI permanently starve the daemon's Full profile (InApp excludes `morning-brief` + `memvid-indexer`) — QQ digest silently stops + mv2 recall goes stale while `zen doctor` stays green
  - **False "closure" claims corrected**: FR-040 wake-up brief has NO production writer (`dream.rs:723` test-only — reader runs an uncapped, unsanitized, always-failing read on both turn entry points every turn); FR-035 boost is inert (`recurrence_count` has no writer anywhere) AND the marker is deleted nightly with a fail-open `load_all` error path
  - **FR-036 telemetry inverted**: success detection `starts_with("Error")` misses every structured-JSON tool error (all fs.*/delegate tools) so broken-tool detection can never fire; per-tool latency is batch-wall/n (fabricated p50/p95); reward sidecar keyed by filename stem conflates unrelated notes with blocking flock+fsync per turn on the async path; `note_stages` full-table scan per cycle, unbounded
  - **Verified sound** (do not re-litigate): storage 兼容性/可迁移性 PASS (migrations 005-009 additive, 008 backfill correct, 009 true upsert, claim-fence conditional UPDATE, vec0 untouched); 004 sole-owner/auto-start/exactly-once-audit invariants hold (contract 02 event-kinds now reconciled with 005 streaming.md); 006 all 7 mechanisms verified (delegate reservations refund on drop — checked, not assumed); FR spot-audit 9/11 (FR-021 Temporal/Goal kinds never emitted; FR-025 second ungated belief writer at `session_journaler_signals.rs:816`)
  - **Frontier decisions recorded in spec.md "Deep Review Record"**: T129/T130 resolved by evidence — adopt provenance-capped belief reliability `min(provenance, content)` (Nous 2606.22030: Bayesian belief updating is inert without per-source reliability) + BeliefMem Noisy-OR M2 candidates; adopt de-anchored judging (self-judge commits its own answer first; false-positives 0.719→0.012, 2607.05904) for Hybrid C + QualityPipeline; HippoRAG-2 seed upgrades; SkillGLoW consolidation; MemStrata deterministic supersession; UX patterns (receipts+Undo, topics-as-files, review-before-keep, accept-or-ignore, visible progress events) and MUST-NOT anti-patterns codified
  - Remediation tracked as **Phase 25 (T153-T167)** in tasks.md; gate status at review time: bin/test 2559 passed / 0 failed / 21 skipped, bin/lint clean

- **Review backlog consolidated into the spec (2026-09-19, 005-agentic-loop)** — every review outcome is now in `tasks.md` as a "Review Summary" table plus **Phase 23 — Open Review Backlog (T130-T144)**, each item audit-verified rather than inferred. Categories: functional gaps (T130-T132), dead primitives to wire or retire (T133-T137), reachability/measurement (T138-T139), absent patterns needing a subsystem (T140-T141), and spec decisions (T142-T144). The load-bearing items:
  - **T130 (FR-021 belief tier gate)** — carried from T129; needs an explicit decision because the M4 dir is the only home for low-confidence beliefs and the "low-confidence beliefs (need evidence)" prompt section depends on it
  - **T131** `evidence_gatherer` reads a directory with **no writer** → the low-evidence research-suggestion loop never fires in production; **T132** `memories/demoted-beliefs/` has **no reader** → demotion is a one-way removal, not a working tier
  - **T133** `Archive::select_parents` + `sample_stepping_stones` test-only (parent recombination/stepping-stone sampling have no consumer; the wired path reads `load_rejected` directly); **T134** `EmbeddingSkillScorer`/`with_scorer` test-only (deliberate per T075 — activate off the per-turn path or mark dormant); **T135** `Tier4Search::connected_components` zero callers; **T137** `ProcessingJob` dead after T126 delivered the projection via `note_stages`
  - **T138** fusion+PPR reachable only via gateway `knowledge/search` auto mode (default `Fast` bypasses it; agent tools call single tiers) — decide whether that is the intended default; **T139** SC-004's p95 degradation clause is unmeasurable in a spawn-per-cycle harness
  - **T140** Dream-RSI replay blocked on a data discipline (structured per-node discovery-tree logging); **T141** community detection absent (a subsystem, not a tweak)
  - **T142/T143/T144** decisions: FR-040 trigger authority (spec "5+ tool_calls" vs Hybrid C), NFC alias backfill, and the writer-less `belief_nodes`/`self_nodes` tables (constrained by T051 single-writer)
  - Carried deferrals, not new work: I4 `read_reward` (RLVR Tier-2 Non-Goal), I13 dead CLI files (deliberate 2026-08-28), FR-039 converter (Non-Goal)
  - **Convergence pass (`/speckit-converge`, same date)**: the *fixed* review items are verified present in the tree (22/22 probes, including all Round-1 eight); the open work is the tracked backlog above plus **Phase 24 — Convergence (T145-T152)**, eight newly-found gaps that duplicate nothing in Phase 23: **T145** the leftover divergent `notion/service.rs::normalize_notion_name` still normalizes `upsert_entity` names while `insert_alias` uses the canonical `normalize_alias` (same name, two canonical forms — the SC-008 duplicate-entity risk); **T146** `get_stale_episodes`/`get_frequent_entities`/`should_correct`/`update_with_method` have zero production callers (the §8.3.3 episode-compression and entity→wiki promotion decisions never fire); **T147** `prune_beliefs` only filters the prompt vector, despite its name; **T148/T149** zero-caller API to wire or delete (`Tier4Search::shortest_path`, `Tier4Search::synthesize_context`, `TierSelector::auto_select`, `tier3` schema helpers, `ChatImporter`, `aggregate_tool_calls`, `write_metrics`); **T150/T151/T152** doc contradictions per Constitution XV (`contracts/worker.md` describes per-note RetryPolicy budgets and omits the pending re-queue and stages 3a/3b/5b/5c/5d; `data-model.md` omits the migration-009 `note_stages` table; `plan.md` still references the deleted `merge_invocations`)

- **RSI/frontier research recorded into the spec + wiring audit (2026-09-19, 005-agentic-loop)** — the seven-system research existed only in this file; it is now in the spec, and every adopted pattern was traced to a production caller (an audit, not an assumption):
  - **`spec.md` "Research Record — Adopted Industry Patterns & Implementation Status"** + **`research.md` D28**: the seven systems — AlphaEvolve (arXiv:2506.13131), Darwin Gödel Machine (arXiv:2505.22954, ICLR 2026), Voyager (arXiv:2305.16291, TMLR 2024), Reflexion (arXiv:2303.11366, NeurIPS 2023), SPIN/Self-Rewarding (arXiv:2401.01335 / 2401.10020, ICML 2024), Letta sleep-time compute (arXiv:2504.13171), and **Dream-RSI (arXiv:2609.14858, 2026-09-14 — verified against primary sources: Google/UMD/Google DeepMind/UVA, live site dream-rsi.com, code github.com/zhengkid/Dream-RSI; a non-peer-reviewed technical report)**. Each carries mechanism → source → transferable idea → weights-frozen applicability. Cross-cutting: the recurring pattern is *archive of diverse attempts + self-generated feedback + replay/idle compute*, and only SPIN/Self-Rewarding are inherently tied to weight updates (with the reward-hacking caveat from arXiv:2607.05904: a reference-free self-judge moved pass-rate 0.72→0.94 while true accuracy stayed 0.20)
  - **Verified WIRED**: RRF fusion + PPR `seeded_ranking`, Reflexion (stage 5c writes / 5b injects), `Archive::refresh` + `Archive::prioritize` (stages 5b/5c), skill precipitation incl. `## Gotchas` via `zen skill confirm`, and Letta-style idle consolidation (the scheduler workers)
  - **Verified DEAD / ABSENT — new findings**: `Archive::select_parents` and `sample_stepping_stones` are test-only (the wired stepping-stone path reads `load_rejected` directly, so archive-parent recombination and stepping-stone sampling have no consumer); `EmbeddingSkillScorer`/`SkillHitRouter::with_scorer` are test-only (deliberate — T075 keeps embedding work out of the per-turn routing path); `Tier4Search::connected_components` has **zero callers anywhere**; community detection (Leiden/Louvain) is absent; Dream-RSI's replay is absent and its prerequisite is a *data discipline* — structured per-node discovery-tree logging (parent links + per-attempt outcomes), which `logs/hypothesis-archive.json`/`loop-last-report.json` do not provide
  - **Reachability caveat worth knowing before claiming fusion is default**: `KnowledgeSearchMode` defaults to `Fast`, which pins `tiers=["fts"]` and bypasses fusion entirely. The only production fusion path is the gateway `knowledge/search` RPC in auto mode, so RRF+PPR fire for Full-mode TUI, `zen chat`, and `/search` — **not** the default TUI chat, and not the agent `tier2_search`/`tier4_search` tools (which call single tiers directly)
  - **T129 disposition (investigated, needs a decision)**: the FR-021 belief tier gate is unenforced (`memory_curator` writes every belief to `wiki/wisdom/beliefs` with `evidence_count=0`; `Belief::should_promote` only logs; the only demotion targets `memories/demoted-beliefs/`, which nothing reads). The investigation's key constraint: the M4 dir is currently the **only** home for low-confidence beliefs and the "🔍 Low-confidence beliefs (need evidence)" prompt section depends on it — so a write-side gate would silently empty the evidence surface. The least-breaking design is a read-side split with a real, readable M2 working tier. Not implemented here: it changes what `load_beliefs` injects into every turn, so it needs an explicit decision first

- **Spec re-analysis remediation (2026-09-19, 005-agentic-loop `/speckit-analyze`)** — 33 of 39 active FRs verified implemented; the harness built to close SC-004 immediately found a real defect:
  - **`bin/load-harness` + `crates/zen/tests/load_harness.rs`** (T121) — the SC-004 load harness. Seeds a 100-note batch into an isolated ZEN_HOME, drains it across cycles, and asserts the 60-minute wall limit, zero note loss (every seeded original must be findable in the archive), pending-pool drain, and the ≤5% quarantine ceiling. `#[ignore]` by default (load test, not a unit gate); run via `bin/load-harness [--nocapture]`. It **corrected a false completion claim**: task T041 was marked done for building this harness, but `bin/test-zen` did not exist (the isolation wrapper is `bin/zentest` — whose own header misnamed it) and no 100-note harness existed anywhere; the 2026-08-29 CEO review had flagged this as an unanswered question that was never dispositioned. SC-004 in spec.md now names the real harness and records the defect it found
  - **Pending-pool stranding fixed** (T122, CRITICAL) — budget-deferred notes were moved to `vault/archive/pending/` and **nothing ever read them back**: the pool had no consumer anywhere in the workspace, so a 12-note inbox silently stranded 7 notes forever and `pending_count` accumulated with no retry. `requeue_pending()` now moves `archive/pending/*.md` back into the inbox at cycle start (a failed move stays pooled and retries next cycle, so nothing is dropped), emitting a `loop.pending.requeued` audit line. Verified: 12 notes → 10 processed + 2 pending → next cycle re-queues and drains both
  - **Per-cycle budget is now configurable** (T123) — the worker hardcoded `LoopBudget::default()` (5 steps) with a comment admitting `LoopConfig` exposed no such fields, capping throughput at 60 notes/hour (5 × 12 cycles/hour at the default 5-minute interval) — below SC-004's 100. New keys, 5-layer inherited: `[agentic.loop] max_steps` (default **10**, clamp 1..=100, env `ZEN_LOOP_MAX_STEPS`) and `max_tokens` (default 8000, env `ZEN_LOOP_MAX_TOKENS`); `LoopBudget::with_limits()` constructs the per-cycle budget. Measured after the fix: 100 notes drain in 10 cycles, **~18k notes/hour**, 0 quarantined, 0 stranded
  - **Partial-FR follow-ups landed** — T124 FR-022: NFC now runs first in the canonical `zen-repo::normalize_alias` (it was trim/lowercase/suffix-strip only, with NFC living in a local `notion/service.rs` helper and inline in `tier4.rs` — so the two paths FR-022 actually names, FR-016 clustering and FR-015 verification, were not NFC-corrected); `tier4`'s inline pre-normalization became a redundant subset of `insert_alias` and was removed. Note aliases persisted before this change are non-NFC until re-indexed. T125 FR-040(e): `render_skill_md` now emits a `## Gotchas` section derived at render time from the draft's evidence trail via a narrow marker set (no schema change, so drafts already staged in `skill-confirmations.json` still confirm); the section is always present and says "None recorded" when the evidence holds no correction or failure. T126 FR-011: **new migration 009 `note_stages`** — a durable per-note projection keyed `(file_path, content_hash)`, recorded only after a successful commit (both CAS and non-CAS paths funnel through one `committed` check) and consulted at cycle start to skip content already archived; a skipped duplicate is removed from the inbox so FR-006's drain still holds. Deliberately a separate table: `NotesRepo::index_note` writes `notes_meta` with INSERT OR REPLACE and an explicit column list, so a stage column there would be reset by re-indexing and silently re-open finished notes. The new test caught two real bugs before they shipped — the projection was initially written only on the CAS path (so it never recorded with CAS inactive), and the lookup hashed `note.content` while the archive recorded full-file bytes (so dedup silently did nothing). T127 FR-018: each archived note appends `loop.note.archived` with cycle_id/source/dest/checksum — the triple an undo needs; page-body writes stay covered by the per-cycle git commit + CAS snapshot and quarantines by their gap, deliberately not duplicated
  - **Recorded, not implemented** — T128 FR-020: M5's runtime virtue-log path (`memories/virtue_logs/`, written by MemoryCurator and read by `load_virtue_logs`) was missing from the model — `wiki/virtues/*` is seed-only; reconciled in spec.md rather than relocating user data. The M3→M4 belief *creation* chain is a **spec tension with FR-033** ("Belief creation remains exclusively via FR-025"), so it needs a decision, not code. T129 (**new, open**): FR-021's "posterior<0.9 stays M2 / >0.9 promotes to M4" is not enforced — `memory_curator` writes every belief to `wiki/wisdom/beliefs` at creation and `Belief::should_promote` only logs, so the working-belief/durable-wisdom threshold does not exist
  - Gate: `bin/test` 2550 passed / 0 failed / 21 skipped; `bin/lint` clean; SC-004 harness passes

- **Multi-agent orchestration fusion (2026-09-07, 006-agentic-orchestration)** — dead parallel system replaced by model-driven delegation + real quality gate:
  - `delegate.task` (D1-D3): `DelegateTaskTool` runs a REAL LLM sub-turn (≤4 rounds, own `AgentExecutor` + `build_sub_agent`) instead of the deleted skills-carrier stub; depth-1 guard strips `delegate.*` grants (overlay included); `SharedSensitivity` propagates session policy to the sub-agent's `AgentContext`; kill-switch `[agentic.delegate].enabled` (default true), `timeout_secs` clamp 30..=1800 (env `ZEN_DELEGATE_ENABLED`/`ZEN_DELEGATE_TIMEOUT_SECS`); args `{agent, prompt, description}`; unknown agent / bad args / exhausted budget / timeout all return structured Ok-error output (never panic the round). Lazy-registered at first orchestrator turn (`ensure_delegate_tool`) so `with_approval_callback`'s `Arc::get_mut` stays viable
  - Quality gate (D4-D5): `execute()` runs `QualityPipeline` (Metis→Momus→Hermes + async `SemanticReviewer` on HIGH blast radius) after the tool loop; Momus veto triggers exactly ONE feedback round then re-review; `ExecutionMetadata` gains `quality_notes: Option<String>` + `delivery_ready: bool` (serde default_true — pre-gate payloads stay decodable); `execute_stream()` runs the gate post-hoc and appends a `⚠️ quality gate: delivery not ready` callback line on veto; audit line `loop.turn.review` (plan_approved/delivery_ready/feedback_rounds) appended to `logs/audit.jsonl` per turn
  - INTENT_SIGNALS salvage (D6): `ZenCoordinator` (948 lines), `AgentExecution::sub_agent_results`, `ZenWiring.delegates`, and the stub delegation block deleted; keyword routing survives as the `INTENT_SIGNALS` const table — `classify_intent` returns `(agent, signal)`; the signal rides the `loop.turn.review` audit entry; `route()` facade unchanged
  - Reflection (D7): unchanged — `SessionJournaler` remains the async post-turn writer
  - Known landmine (pre-existing, untouched): all six zen-provider sync `complete()` impls (`Runtime::new()` at ollama.rs:97 et al.) panic under async contexts when a live local model exists; the streaming path is unaffected. C1 (Phase C) owns the `spawn_blocking` fix
- **001 vision convergence (2026-09-08, 006 T372-T379 via /speckit-converge)** — the 001 multi-agent scheduling vision lands on the 006 kernel:
  - T372 intent: `zen-agents/src/intent.rs` — LLM-first classification into `Intent{category: Query|Action|System|Conversation, agent, signal, acl, confidence, source}`; confidence<0.7 → Conversation fallback; LLM failure/timeout (20s) degrades to the INTENT_SIGNALS keyword fast path; `router.route()` wrapped in `spawn_blocking` (ollama nested-Runtime panic safety); audit `loop.turn.review` gains intent_category/intent_source/intent_confidence/intent_acl
  - T373 depth: `[agentic.delegate].max_depth` (default 1, clamp 1..=3, env `ZEN_DELEGATE_MAX_DEPTH`); `spawn_allowed(parent_tier, child_tier)` matrix; task-local `DELEGATE_DEPTH`/`DELEGATE_PARENT` chain (root parent "Sisyphus"); `build_sub_agent(agent, allow_child_delegation)` keeps `delegate.*` grants strictly below the cap
  - T374 fan-out: delegate.task `tasks:[...]` collect mode — `should_delegate` gates (independent/consumer-decision/bounded 32k/worth-it advisory 50k tokens) audited as `loop.delegate.gates`; `max_concurrent` (default 4, clamp 1..=8) bounded batches via join_all; single-task keeps the legacy shape, multi-task returns `{results:[...]}`
  - T375 plan-DAG: `plan.execute` (see Agent Tool Inventory) — Kahn topological layers, per-layer parallel execution, failure propagation, Hermes verdict as completion signal, summary-only return (plan artifacts stay out of parent context)
  - T376 persistence: migration 006 `workflow_plans`/`workflow_tasks` + `WorkflowRepo`/`TaskCheckpoint`; per-task idempotent checkpoints ((plan_id, task_id) upsert); `resume_plan_id` replay; resumed plans close out to completed/failed
  - T377 blackboard superseded: `blackboard.rs` + tests deleted; ADR at `docs/specs/006-multi-agent-orchestration/adr-001-blackboard-supersession.md`; inter-agent transport = direct tool returns + batch aggregation
  - T378 surface profile: `[agentic.orchestrator] surface = full|delegation-only` (default full, env `ZEN_ORCHESTRATOR_SURFACE`); delegation-only strips every Sisyphus direct tool — only delegate.task + plan.execute remain (001 A.8 ultra surface); invalid values warn + fail open to full
  - T379 gates: bin/test 2291 passed / 0 failed / 19 skipped; clippy --workspace --all-targets -D warnings clean; fmt --check clean

- **Eng-review D1-D4 remediation (2026-09-11)**:
  - D1 panic guard: `AgentOrchestrator::review_pipeline` semantic-reviewer LLM call moved onto `spawn_blocking` — sync `route/call` hit the OllamaProvider nested-`Runtime::new` panic on the Confidential+local HIGH-blast path (same hazard guard as `intent::llm_classify`); join failure now maps to `LlmError::ProviderUnavailable` and fails open
  - D2 plan-DAG dataflow: `plan.execute` injects direct `depends_on` outputs into downstream task prompts (`compose_task_prompt`/`upstream_snippet`, per-dep 4000-char snippet taken from the delegate `response` field; ok-checkpoint replay injects the recorded value); tool description now instructs synthesis-style dependent prompts
  - D3 quality-gate scope: gate runs only for tool-mutating turns (`!tool_calls.is_empty()`) or Confidential sessions, in both `execute()` and `execute_stream()`; pure chat turns synthesize `PipelineResult { plan_approved: true, delivery_ready: true, review_notes: "quality gate skipped: no tool invocations this turn" }` (plan-shaped Momus heuristics vetoed ordinary conversational answers, burning one redraft round per chat turn)
  - D4 intent routing contract documented (accepted behavior, not a bug): LLM path routes to 3 category-default agents (Query→Explore, Action→Hephaestus, System/Conversation→Sisyphus); `delegate.task`/`plan.execute` are reachable on the LLM path only via Sisyphus fallback — see `intent.rs` module doc "Routing scope"

- **V13 orchestration vision finalization (2026-09-12)**: `docs/specs/001-agentic-foundation/V8.md` converged to **V13 终版** (industry synthesis × 13-agent positioning × orchestration decision tree; V8/V9/V11 drafts deleted, V12 kept as last historical evolution; file force-added to git despite `docs/specs` gitignore — authoritative vision must survive disk loss). **PD-01 DECIDED: A+B adopted** (native ToolCall primary channel + rig `AgentRun` loop migration, replacing `max_tool_rounds=8` hard cap; triple-evidence convergence: PD-02 four-way study codex/pi/hermes/dsh + 6-framework industry comparison + internal evaluation; T108 output-schema wiring rides step B); PD-03 TaskState design unblocked. V13 eight design principles carry falsifiable baselines (budget-trip <2% turns; veto rate 5-20% healthy). New `bin/orchestration-stats` — usage telemetry from `logs/audit.jsonl` (intent routing / delegate gates / plan-DAG / quality-gate distributions); first snapshot exposed T111 test pollution (E2E writes to global audit log) and T110 (fold into `zen discover report`)

- **PD-01 landed (2026-09-12, 005-agentic-loop T112-T119)** — orchestrator loop migrated to rig-agent; all on branch `005-agentic-loop`:
  - T112 intent LLM telemetry: `LlmOutcome` (ok|low_confidence|timeout|unavailable|error|skipped) + `has_configured_provider()` fail-fast gate; `loop.turn.review` gains `intent_llm_outcome`/`intent_llm_ms` (p50/p95 surfaced by discover report)
  - T113 deps: `rig-agent 0.41` in tree (lockstep with rig-core ^0.41; both superseded by the T120 0.42 upgrade — same lockstep rule, now ^0.42); misleading `rig = { package = "rig-core" }` alias dropped — provider files import `rig_core::` directly (upstream `rig` is a different facade crate)
  - T114 PD-01 A native-primary dispatch: `resolve_invocations` replaces `merge_invocations` (deleted) — native calls dispatch directly; fenced parse only when zero native; both-present → native wins + `degraded` warn/callback, never double-dispatch; fenced fallback byte-identical to pre-PD-01 (contract-pinned)
  - T115 PD-01 B AgentRun: `execute`/`execute_stream` driven through `AgentRun` sans-I/O state machine (`next_step` → CallModel/CallTools → `model_response`/`tool_results`); zen keeps ALL I/O — provider stack (T096 budgets, stream timeouts, spawn_blocking guards), 5-hook dispatch pipeline, T114 policy; `max_turns` = resolved max_tool_rounds + 2 headroom (config keys/env unchanged, clamp stays emergency governance); usage/completion telemetry from `run.usage()`
  - T116 T108 output_schema: `agent_output_schema(name)` → `AgentRun::with_output_validation(schema, max_output_schema_retries)` in both loops; `validate_output`/`build_retry_prompt` retained for fenced fallback only; ≤2 retries (T091 contract)
  - T117 T110: `zen discover report` now emits `orchestration` section (Rust aggregator `zen-vault::distill::orchestration_stats`, parity with local `bin/orchestration-stats` awk script — script stays local-only)
  - T118 T111 audit isolation: zen-core `test-support` feature (cfg-gated: `user_root()` reads `ZEN_HOME` fresh per call + per-process temp fallback in test builds; production LazyLock path untouched); dev-deps enable it in zen-agents/zen-cli/zen-gateway; gateway `sole_owner`/`legacy_surface` explicit `audit_path`; `audit_isolation_guard` test asserts real `~/.zen/logs/audit.jsonl` untouched. Pre-T118 test pollution (1597 lines) archived to `audit.jsonl.polluted-pre-t118` 2026-09-12; live log clean, V13 §6 baselines accumulate from zero
  - Gate: `bin/test` 2357 passed / 0 failed / 19 skipped (baseline 2307); `bin/lint` clean; discover-report ↔ awk-script parity verified
- Binary/library separation: zen (bin) + zen-cli (lib) architecture documented
- 29 CLI commands documented with dispatch file paths
- 4-layer config inheritance model (Default → embedded → global → env)
- Unified data layer: `SqliteClient` (tokio-rusqlite writer + sqlx pool); 9 domain repositories (Principle XII)
- 5-tier search pipeline: ripgrep → FTS5 → vec0 embeddings → entity graph → LLM
- Provider routing: 13 named providers across 3 protocol types (rig-native, openai-compatible, anthropic-compatible)
- Agent system: 13 agents in 4 tiers, 3-layer permissions, QualityPipeline (blackboard superseded by 006 ADR-001)
- Corrected `excute_command` → `execute_command` across all 24 command files
- Documented framework patterns: clap derive, rig-core Client/CompletionModel, FTS5 schema
- **Architecture remediation (2026-06-01)**:
  - Unified LlmPreference: zen-core defines with Serialize+Hash+Display, zen-agents re-exports (A3 resolved)
  - Renamed zen-agents SensitivityLevel → AgentClearance (distinguishes agent permissions)
  - Renamed zen-core/validate SensitivityLevel → SafetyLevel (distinguishes validation results)
  - Deleted zen-llm directory (legacy subset, no consumers)
  - Deleted rig_ollama.rs/rig_openai.rs stubs (T229/T230 legacy)
  - Constitution principle XI added: Design-First & Reuse Priority
- **CLI consolidation (2026-07-22)**:
  - Deleted `zen task` (stub) — `Cancel` subcommand added to `zen dispatch`
  - Merged `zen reindex` + `zen lint` + `zen distill` into `zen wiki` (subcommands: reindex, lint, distill)
  - Removed stale `zen hello` + `zen consolidate` from command table (never in Commands enum)
  - Command count: 33 → 29 (4 top-level commands eliminated, Occam's Razor)
- **CLI personal-agent scope trim (2026-08-28, 005-agentic-loop)**:
  - Removed 9 manual commands from CLI surface: `note`, `search`, `similar`, `notion`/`graph`, `research`, `ingest`, `routine`, `brief`, `dispatch`
  - Rationale: zen focuses on the personal memory/knowledge pipeline — these capabilities run internally via ZenScheduler workers + distill loop, not as manual commands; command files remain on disk uncompiled, restorable when business scenarios require
  - Command count: 29 → 20 (9 removed; 005's loop landed nested as `zen wiki loop` — no new top-level command)
- **Phase 8 wiring (2026-09-01, 005-agentic-loop)** — deferred library surfaces went live, no CLI-surface change:
  - FR-028: ZenLoopWorker Stage 5c — refinement queue persisted to `logs/refinement-queue.json`, hypothesis re-verify on `reverify_older_than_days` (default 7)
  - FR-030: Stage 3a — `vault/raw/` Code/Paper sources routed through `GraphRouter::route_and_join` each cycle (`raw_graph_routing`, default on); host-source `worker_type` awaits FR-033 (T043/T044)
  - FR-031: Stage 5b declares hypothesis slugs into `logs/placeholders.json`; distill compile downgrades reserved creates to updates (report field `placeholder_downgrades`) and merges slots; tier-5 synthesis prepends a 2-hop `SubgraphContext` render (`@subgraph:` result)
  - FR-032: self-write-aware OCC in `run_scoped` (`cas_commit`, default on) — pre-cycle `VersionSnapshot`, inbox-source removal deferred past the drift window, commit gated on EXTERNAL drift only (drift minus txn-tracked self-writes; raw `commit_conditional` counts self-writes as drift and would livelock every writing cycle — it stays the library primitive for scopes without self-writes); rolled-back cycles report `cas_rolled_back`/`cas_drifted` and skip the checkpoint; `log.md`/`index.md` churn is txn-tracked
  - New `[agentic.loop]` config keys (all default-on/7, 5-layer inherited): `hypothesis_refinement`, `reverify_older_than_days`, `raw_graph_routing`, `cas_commit`, `host_stage_timeout_secs` (default 60, env `ZEN_LOOP_HOST_STAGE_TIMEOUT_SECS`; per-host-source staging timeout — a source exceeding it is skipped with a 30-min backoff because macOS TCC denial can hang `opendir` indefinitely; `stage_host_dir` runs under `spawn_blocking`+timeout, never wedging the scheduler)
- **In-app scheduler (2026-09-15, 005-agentic-loop)** — hermes-style background learning while the app is in use, daemon-optional:
  - `SchedulerProfile::{Full, InApp}` (zen-agents `scheduler/mod.rs`): `create_configured_scheduler_with(config, profile)` registers per profile; legacy `create_configured_scheduler` = Full (unchanged signature). InApp = 14 learning-core workers (excludes `memvid-indexer` — gateway mv2 flock duty, and `morning-brief` — outbox staging consumed by the daemon drainer); `worker_ids()` is the test seam
  - TUI probe gate (`tui/scheduler_gate.rs`, both inline + fullscreen paths): kill switch → `prewarm::resolve_client()` → `SurfaceClient::health_status()` probe → spawn InApp only when no daemon-hosted scheduler is alive; unreachable/absent daemon fails open (spawn). `zen chat` unchanged (short-lived; next TUI session's scheduler picks up its journal)
  - `GatewayService.scheduler_hosted` (default false) exposed as additive `health/status` payload field `scheduler: bool` (response-field addition, no protocol version bump); explicit `zen serve start` sets it via `GatewayDaemonConfig.scheduler_hosted` aligned with its `scheduler_enabled()` gate; implicit daemons (TUI/chat-spawned, `ZEN_SERVE_NO_SCHEDULER=1`) report false without code change
  - **Scheduler role lease (2026-09-18 review fix)**: `zen-agents/src/scheduler/lease.rs` — cross-process advisory flock on `<logs>/scheduler.lock` held for the scheduler host's lifetime (kernel releases on crash). TUI gate fails closed when the lease is held (second TUI skips; learning resumes next session); daemon retries every 30s until free, then flips `GatewayDaemonConfig.scheduler_live: Option<Arc<AtomicBool>>` which `is_scheduler_hosted()` prefers over the static flag — `health/status.scheduler` never claims a scheduler still waiting out a TUI-held lease. Closes the probe-then-spawn TOCTOU (two TUIs / TUI-then-daemon double-fire)
  - **RLVR data hardening (2026-09-18 review fix)**: reward-sidecar increments (`increment_access/citations/corrections`) serialized across processes by flock on `memories/.reward/.lock` (fs2; 8×25 concurrent test pins zero lost updates); sidecar writes are tmp+fsync+rename, corrupt files warn + default, `MemoryReward` is `#[serde(default)]` (older sidecars stay decodable); `prune_expired_sessions` (mtime < 30d ⇒ session dir removed, called by dream worker before `aggregate_all_sessions`) keeps `logs/<session>/tool_calls.jsonl` bounded on disk
  - **RLVR/RSI feedback-loop wiring (2026-09-18 review fix, I1/I2/I5)**: closes the write-only loops found by the wiring audit — (a) `increment_corrections` gains production call sites via orchestrator `increment_reward_for_query` (bilingual `CORRECTION_MARKERS` heuristic): in-context memories get `correction_count` when the turn corrects prior output, on both `execute` and `execute_stream`, alongside the existing per-turn `access_count`; (b) FR-035 closure — dream worker writes `logs/loss-aversion-boost.json` when the 30-day correction scan finds high recurrence (>0.5 rate ⇔ `high_recurrence > 0`) and deletes it after a clean scan; `run_decision_quality_gate` reads it via `loss_aversion_boost_active` (fresh ≤30d) and calls `check_all_with_boost`, whose boosted loss-aversion check trips CRIT on unrecoverable sunk cost even when the recorded loss is affordable (`check_all` = unboosted, existing callers unchanged); (c) FR-040 closure — `inject_wake_up_brief` surfaces `logs/wake-up-<date>.md` as an M1 note once per date (idempotent because the note path carries the date), called at both turn entry points before `inject_skill_hits`
  - **Industry memory/RSI patterns (2026-09-18, Wave 1)**: adopted from the 2024-2026 agent-memory survey —
    - **RRF hybrid retrieval fusion** (Graphiti/Zep): `zen-vault/src/search/fusion.rs` implements `score(d) = Σ weight_t / (k + rank_t(d))` (k=60), dedup by `(file, line)` so multi-signal agreement accumulates; `SearchService::search_fused` runs FTS5 + vec0 + graph in parallel (`tokio::join!`), failing tiers degrade to nothing (warn-logged), and every hit carries provenance. **Auto mode only**: `search(tier=None)` routes the ordinary multi-word case (selector tier 2) through fusion; explicit tiers and the `similar:`/`graph:`/`summarize:`/single-word intents keep single-tier routing unchanged. Weights: fts5 1.0 / vec0 1.0 / graph 0.8 (`TIER2_WEIGHT`/`TIER3_WEIGHT`/`TIER4_WEIGHT`)
    - **MAP-elites archive + DGM stepping stones** (AlphaEvolve / Darwin Gödel Machine): `zen-vault/src/distill/archive.rs` classifies every hypothesis into deterministic cells on 3 independent axes — evidence (none/thin/solid from `evidence_refs`), gap-domain (structural/judgment/process from `GapKind`), freshness (fresh/revisited vs falsified claims) — persisted to `logs/hypothesis-archive.json` (corrupt ⇒ empty, never blocks a cycle); `select_parents(n)` round-robins across occupied cells (illumination + quality); `sample_stepping_stones(rejected_dir, kind, limit)` re-surfaces rejected claims as recombination material. `Archive::refresh` runs each `zen-loop` cycle at stage 5b
    - **Reflexion inter-cycle verbal feedback**: `zen-vault/src/distill/reflection.rs` writes a deterministic reflection (`claim`/`falsifier`/`because`/`next_attempt`) to `vault/wiki/wisdom/reflections/<slug>.md` on every rejection (wired in `record_rejected_hypotheses`); `generate_from_gaps_with_history(gaps, rejected_dir, reflections_dir)` appends per-gap-type stepping stones + recent reflection guidance to each exploration prompt (no history ⇒ byte-identical to `generate_from_gaps`). Note: reflections live in their own directory — `rejected/` remains the FR-040 negative-space archive only
    - **Voyager-style skill retrieval (injectable)**: `zen-agents/src/skill_embedding.rs` provides an on-disk skill-embedding cache (JSON, FNV-1a content hash for staleness, corrupt ⇒ empty) + `EmbeddingSkillScorer`; `SkillHitRouter::with_scorer` accepts any `SkillScorer`. Promotion is monotonic — a scorer may only raise a skill's score, never lower it, and `None` (no backend/cache/stale) falls back to today's trigram path byte-for-byte. Embedding work stays OUT of the per-turn routing path (the T075 constraint recorded in `skill_hit_router` module docs), so activation is a caller-side wiring decision, not a default behaviour change
    - **Bi-temporal graph edges + personalized PageRank** (Graphiti / HippoRAG): migration `008_bitemporal_and_ppr.sql` adds a system-side validity window to `relationships` — `t_valid TEXT NOT NULL DEFAULT ''` (RFC3339 UTC; `''` = valid since epoch) + `t_invalid TEXT` (NULL = open); legacy rows backfilled to `t_valid = created_at`. Constant default + backfill is deliberate: a non-constant `ADD COLUMN` default is re-evaluated on reads of pre-existing rows, and `datetime('now')`'s `YYYY-MM-DD HH:MM:SS` format does not sort lexicographically against RFC3339. `NotionsRepo` gains `insert_relationship_temporal` (contradiction-aware: same source+relation type, different target, open edge ⇒ soft-invalidate old at the new edge's `t_valid`), `invalidate_relationship` (soft, first-invalidation-wins), `relationships_as_of(ts)` (half-open `[t_valid, t_invalid)`), and `personalized_pagerank(seeds, iterations, damping, restart)` sharing the extracted core with the existing global `pagerank()` (dangling mass redistributed over the teleport vector; standard use is `restart = 1 - damping`). Graph traversals exclude soft-invalidated edges; **nothing is ever DELETEd** — history stays point-in-time queryable

  - `[cron] tui_scheduler` (default true, env `ZEN_TUI_SCHEDULER`, 5-layer inherited) — false disables all background learning outside an explicit `zen serve start`
- **Review-finding closures (2026-09-19, 005-agentic-loop)** — three findings that had the same shape (a primitive existed, nothing read it):
  - **I8 arena losses are staged, not just recorded**: `run_arena(contestants, logs_dir, cycle_id, hypotheses_dir)` now stages one `HypothesisSlug` per lost case (`arena-loss-<case>`, kind `LlmFailure`, status `Exploring`, confidence `0.7`) via `stage_losses`, recording the slugs on the report (additive `staged_losses`, `#[serde(default)]`). The `LlmFailure` kind is documented "not yet emitted" — these losses are its first emitter; they bypass the gap-driven generator because it deliberately filters that kind. `evidence_refs` carries the report path, which is what promotes the hypothesis into the actionable external-fetch half of `build_refinement_queue` rather than a bare user question, so the loss enters the existing stage 5c reverify loop and reaches promotion only after surviving it. Slugs are case-derived and `save` keeps the higher status ⇒ repeated losses converge on one hypothesis and a resolved one is never reopened. Docs corrected: the module doc had claimed the promotion worker staged losses (it stages only `Validated`), `run_arena` said "recorded in the report only", and the CLI summary echoed that
  - **I10 curriculum: the refinement queue is illumination-ordered**: `ArchiveCell::of(h, rejected)` is now the single shared cell classifier (used by both `Archive::refresh` and the new `Archive::prioritize`), and `Archive::prioritize(hypotheses, rejected)` orders candidates by cell crowdedness — archive occupancy plus same-batch claims — so under-explored regions of hypothesis space lead. `build_refinement_queue_prioritized(slugs, archive, rejected)` renders that order; zen-loop stage 5c uses it and records `occupied_cells` in `logs/refinement-queue.json`. Guarantees: deterministic, ties keep input order, empty archive preserves input order exactly (so the only behaviour change is ordering). This is the "what to learn next" signal the fixed cron schedule lacked — previously `select_parents`/`sample_stepping_stones`/reward counters were written but never read for selection
  - **W1's temporal/PPR primitives were dead — PPR now drives graph retrieval**: `personalized_pagerank`, `relationships_as_of` and `insert_relationship_temporal` had **zero production callers** (only their own tests) — the same shape as I8/I10, so the same fix now applies. `Tier4Search::seeded_ranking(client, query, limit)` resolves query terms through entity names then aliases (`MAX_SEEDS = 6`, 3+ char alphanumeric tokens, lowercased to match `COLLATE NOCASE`) and ranks the graph *relative to those seeds* (HippoRAG: `PPR_DAMPING = 0.85`, `PPR_ITERATIONS = 20`, `restart = 1 - damping`), excluding the seeds themselves and returning empty when nothing resolves. `search_fused` runs it as a 4th RRF list (`source: "ppr"`, `PPR_WEIGHT = 0.9` — graph-derived but query-relevance ranked, so between the graph's 0.8 and the lexical/semantic 1.0), which means a natural-language query now reaches neighbouring entities; the exact-name BFS seed in `Tier4Search::search` could not. `relationships_as_of`/`insert_relationship_temporal` stay unwired on purpose: point-in-time retrieval needs a caller that supplies an as-of timestamp, and no surface has one yet
  - **I12 verified, not implemented (reasoning exists)**: the review claim "consolidation is data-not-reasoning" is largely FALSE — `wisdom_synth` (weekly) reads ALL reflections + ALL beliefs and writes back `belief.update(...)` plus mental-model/anti-pattern candidates, and `reflection` (daily) synthesizes cross-session anti-patterns into MEMORY.md ledgers. Residual gap narrowed and recorded: the nightly `dream` consolidation and the 5-min `zen_loop` distill cycle remain data-only, `stages/llm_distill.rs` is an explicit T046 stub, and the state.db `belief_nodes`/`self_nodes` tables have **no production writer** (only tests call `upsert_belief_node`/`upsert_self_node`) — wiring them would create a second writer for the markdown belief surface, contradicting the T051 single-writer decision, so it stays a decision for a future PR rather than an implicit change
- **Tool-loop config + visible intermediates (2026-09-02, 005-agentic-loop T050/T054/T055)** — orchestrator tool loop surfaced and configurable:
  - `[agentic.tool_loop] max_rounds` (T050, zen-core `ToolLoopConfig`): default 8, clamped 1..=16, env `ZEN_TOOL_MAX_ROUNDS` (5th layer), 5-layer merged; replaces the former `const MAX_TOOL_ROUNDS = 4`. Token budget + turn watchdog unchanged
  - `AgentOrchestrator` reads the cap at construction (`resolve_max_tool_rounds()`, config-load failure falls back to 8 with warn); override via `with_tool_loop_config(n)` (clamped) or read via `max_tool_rounds()`
  - T055 streaming intermediates (contract `streaming.md` callback path): `execute_stream` emits `🔧 <tool> …` before each dispatch and `✅ <tool> done [N hits] <ms>ms` + 100-char output preview after (count shown only when the tool output exposes it, e.g. web.search; dispatch-block errors surface as `❌ tool dispatch blocked: …`) — always before the next `execute_stream_round`, zero protocol break for TUI
- **Gateway structural tool frames + TUI collapsible (2026-09-02, 005-agentic-loop T056/T057)** — tool intermediates are protocol events, not dropped text:
  - T056 (zen-gateway `server/hosting.rs`): `Turn::emit_tool_event(kind, tool, payload)` rides the never-dropped `emit_structural` path — ring 2048/60s, replayable via `session/resume`, delivered as `OutboundFrame::structural` with payload `{tool, args?, duration_ms?, count?, provider?, error?, preview?}` (omit-when-absent). The hosting turn callback intercepts 🔧/✅ lines via `split_tool_intermediates` (pub grammar: `🔧 <tool>[ args…]` / `✅ <tool> done [N hits] [M ms] [provider=P]` / `✅ <tool> failed: <msg>`, first non-marker line after ✅ = ≤100-char preview) and suppresses them from delta coalescing — gateway clients get structured kinds instead of double-rendered text. Contract tests pin `tool_started` precedes `delta` and `tool_completed` precedes `turn_completed` (ring + wire order)
  - T057 (zen-cli `tui/stream.rs`): `StreamCollector` parses the SAME grammar (reuses `zen_gateway::server::hosting::split_tool_intermediates` — single source of truth) into collapsible `tool_blocks`; collapsed one-liner `🔧/✅ <tool>: …`, expanded adds metrics (`N hits · Mms · provider P`) + preview lines. Drain watermark emits each block exactly once (idempotency contract preserved); `take_tool_lines()` flushes stragglers at completion (inline → scrollback, fullscreen → PlainCell); `/tools` slash command toggles expansion (lifecycle identical to `/thinking`: affects blocks not yet flushed)
- **Host governance (2026-09-02, 005-agentic-loop T043-T045, FR-033)** — config-driven host directory ingestion wired into ZenLoopWorker:
  - T043: Stage 2 runs `sweep_host_sources` BEFORE `ingest_sweep` so promoted files enter the same `prev_inbox` stale-detection flow. Per `[[agentic.loop.host_sources]]` entry (`host_path`, `para_target`, `m_tier`, `worker_type`, `raw_policy`, `sensitivity`, `allow_cloud`): `HostSourceConfig::resolve()` validates fail-closed (invalid `worker_type|raw_policy|sensitivity|para_target` → warn + skip source), expands `~`/`$HOME`, hashes to 8-hex `host_dir_hash` (sha256 of expanded path). New top-level `md`/`txt` files stage into `vault/inbox/_incoming/{host_hash}/` (staging tree = seen-set: pending slot OR `promoted/{name}` ledger → exactly-once ingestion); `SourceIngester::promote_incoming` moves them to `inbox/{host_hash}_{filename}` (hash prefix disambiguates cross-host collisions; defer when target still pending distill)
  - T044: `zen-vault/graph_router.rs` dual-track extractors — `CodeExtractor::extract` (deterministic slug/package notions, no LLM; `write_provenance_page` → `vault/{para_target}/host-{hash}-{slug}.md` with `host_path`+checksum, index-only, never copies) and `DocExtractor::extract` (heuristic `NotionExtractor`, host-governed products limited to M2 Fact/M3 Concept — `NotionKind::{Concept, Other}`; Belief remains FR-025-only) + `ensure_raw_copy` (doc+copy policy preserves originals read-only under `vault/raw/{host_hash}/` stamped with `worker_type`/`workspace_id`/`source_path`/`sensitivity`). `route_and_join` signature now `(client, source, content, host: Option<&HostSourceContext>, provenance_root: Option<&Path>, cycle_id)` — host routing honors `raw_graph_routing` master switch; code routes from host dir in place, doc+copy routes from raw copies
  - T045: sensitivity routing — `resolve_model_tier` routes through `DefaultRouter::route` → existing `enforce_sensitivity`: `allow_cloud=false` (default) requests `Sensitivity::Private` (local-only); `allow_cloud=true` is the explicit per-source opt-in (cloud tier). Audit event `loop.host.ingested{ts, cycle_id, source, sensitivity, model_tier: local|cloud|local-unavailable, worker_type, raw_policy, allow_cloud, staged, promoted}` appended to `logs/audit.jsonl` per source with activity. Config `sensitivity: "Internal"` maps onto taxonomy `Sensitivity::Private` (both local-only)
- **Personal-agentic increment (2026-09-03, 005-agentic-loop Phases 11-15)** — TUI input, Pi entities, RSI skills, heartbeat, bounded memory:
  - TUI `Shift+Enter` (FR-019, `tui/handler.rs` + `inline_handler.rs`): `Enter` always submits, `Shift+Enter` inserts newline, `Ctrl+Enter` fallback submit; hint constant `INPUT_HINT` (`app.rs`) reads `Input (Enter=send, Shift+Enter=newline, Ctrl+D=exit)`; kitty `DISAMBIGUATE_ESCAPE_CODES` already on
  - Pi 5-point entities (FR-021): `NotionKind::{Person, Preference, Temporal, Goal}` (additive, `notion_aliases` F1 dedup, zero new tables); `SessionJournaler` derives preference triples → `wiki/wisdom/preferences/*.md` M4 + `preference_triggers` for the router; `temporal_entity` tag + recency weight in memvid nightly; `Goal→Commitment→Progress` with `mention_to_achievement_ratio > 5 → GapRecord{AntiTalkSuspect}`; `InformationQualityGate` enforced at M2 write AND M2→M3 (`grade_session_signal`, contract `memory-grade.json`)
  - RSI skill precipitation (FR-037, `skill_hit_router.rs` + `skill_precipitation.rs`): trigram-Jaccard score reused from `zen_vault::distill` (no new deps), threshold 0.72 / max_hits 1 (contract `skill-hit.json`), hooked before `AgentOrchestrator::route()` into M1 top-5; `[skills.auto_route] enabled` (default true, env `ZEN_SKILLS_AUTO_ROUTE`, per-skill `auto_route: false` opt-out); Dream distills ≥2 similar successes into staged drafts, `zen skill precipitate/confirm` CLI (Hybrid C first-occurrence gate); `zen skill list --json` for cross-agent consumers (FR-039 neutral plane: vault + skills, `Private` stays local)
  - Heartbeat + bounded (FR-038/040): `morning-brief` worker (`0 0 9 * * *`, 15th scheduler worker) stages 3-line brief to `logs/outbox/morning-brief-<date>.json` for qqbot active-send drain (agents↛gateway dep preserved); `MEMORY.md` 200-line cap → merge+supersede with rollup (never silent truncate, `enforce_memory_cap`); `RejectedHypothesis` → `wiki/wisdom/rejected/`; wake-up brief `logs/wake-up-<date>.md`; memory nudge every 10 user turns (logs + `memory-nudges.jsonl`, never into the token stream)
- **Hardening closeout (2026-09-04, 005-agentic-loop T091-T105)** — verify-then-fix per D10-D12 + dropout P0:
  - T105 silent-dropout fix: `parse_tool_invocations_verbose` returns `(invocations, errors)`; serde/normalizer failures become `❌`-surfaced diagnostics (never swallowed), both loops `continue` on empty (never `break`), block echoed as answer carries self-healing feedback naming failed tools
  - T103 approval affinity P0 (was live): broker routed by first-free scan — later-registered turn firing first claimed the earlier turn's route (cross-surface misroute). Fix: `ApprovalCallback` takes `Option<String>` turn; `AskApprovalHook` reads task-local `APPROVAL_TURN` in-task and forwards across `spawn_blocking`; exact-match `decide_for`/`route_for` (unknown/claimed → Deny); legacy `None` path for single-context surfaces; gateway `turn()` scopes execution. E2E `turn_bound_routing_survives_inverted_firing_order` 2/2
  - T092 LLM review stage: `BlastRadius::{Low,High}` (`Confidential` metadata or entropy > 0.8, shared `HIGH_BLAST_ENTROPY`); async `SemanticReviewer` hook runs after Hermes READY for HIGH only when `[agentic.review] llm_review_high_blast` (default true); veto → `delivery_ready=false`. New `[agentic.review]` keys: `max_momus_retries` (2⸱0-5), `max_hermes_revisions` (1⸱0-5), `llm_review_high_blast` (true) + `ZEN_REVIEW_*` env, 5-layer merged
  - T101 outbox drainer: gateway `channel/qqbot/outbox_drainer.rs` (`drain_once` + tick spawn in `run()`, gated on `audit_path`); recipients = live `qq_bindings` via additive `QqBindingRepo::list_chat_ids()`; group-first/C2C-fallback; Public-only fail-closed; never deletes undelivered
  - T093/T096-T099: identity-file 256KiB cap + sanitizer on 5 prompt reads; streaming query strip parity; per-round token accounting + mid-loop bailout; recursive tool-output screening both loops; `decide_tool_action` pure (trio skip now blocking); T095 mock provenance `(String,u32,bool)` + `model_used` fallback flag; T102 dispatch EOF contract test; T091 `output_schema.rs` module done (27/27), pipeline wiring (replace `output_schema: None` + ≤2 retry) deferred follow-up
  - T104 missing targets created (5 files, 12 tests): `tool_loop_contract`, `web_search_multiloop`, `session_multiturn`, `vault_write_e2e`, `fs_write_contract`. Resolved findings: workspace config layer dispute closed by T18 (config global-only, 4-layer merge); `user_root()` test-seam via `test-support` cfg feature (T118)
  - Gate: 2307 passed / 19 skipped / 0 failed; `fmt --check` + `clippy --workspace --all-targets -D warnings` clean
  - Config-cache note: `load_config()` caches per process in non-test builds (`#[cfg(test)]` does not propagate to deps) — integration tests mutating env must call `zen_core::config::invalidate_config_cache()`; cluster quorum counts observations, not unique texts (`detect_cluster`)

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
