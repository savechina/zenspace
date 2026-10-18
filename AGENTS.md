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
cargo test               # Run integration tests
bin/lint                 # fmt --check + clippy -D warnings
orb run bash -c 'unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY; cd "$PWD" && CARGO_TARGET_DIR="$HOME/zen-linux-target" bin/lint'  # Linux lint gate in OrbStack VM (CI parity; arm64; isolates target dir from macOS). Proxy unset is MANDATORY: OrbStack injects the macOS host proxy into the VM; a dead host proxy leaves http_proxy=0.0.0.0:1087 and every cargo registry fetch fails with "curl failed … via 0.0.0.0" — the VM's NAT networking needs no proxy (verified: direct curl to index.crates.io returns 200 with proxy vars unset)
orb run bash -c 'unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY; cd "$PWD" && CARGO_TARGET_DIR="$HOME/zen-linux-target" cargo test -p <crate> --lib <filter>'  # Run tests under real Linux git/glibc (same proxy-unset requirement)
cargo fmt --all          # Format
bin/ship -m "type(scope): 功能点" [-b "功能点正文"]  # Gated commit: lint + Conventional Commits + AI-signature/detail blockers (--suggest for a draft, --allow-detail one-pass)
bin/load-harness         # SC-004 load test: 100-note batch, isolated ZEN_HOME (ignored by default)
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
| `delegate.task` (006/T373/T374) | inherits session | zen-agents/delegate_task.rs | Model-driven sub-agent delegation: args `{agent, prompt, description}` single-task or `{tasks:[...]}` fan-out (≤8/batch, batch width ≤4 passes the consumer gate); sub-turn runs a real LLM loop (≤4 rounds) on a sub-agent whose grants keep `delegate.*` only below the depth cap (`max_depth` default 1, clamp 1..=3, tier matrix: O→O 拒, P→P/O 拒, Specialist/Worker leaf); `spawn_allowed` + task-local depth chain; delegation gates are `evaluate_gates` (advisory: independent/consumer-decision/bounded≤32k chars/worth-it, logged as `loop.delegate.gates`) + `validate_task` (hard: known-agent/tier-matrix/depth-cap — the recorded `should_delegate`/`tool_loop_over_budget` names never existed as functions, D14 drift correction); optional `output_format`/`task_boundaries` contract args + `reasoning`/`verification` report markers (D9, W1.2) and `artifact` output-path convention (D12, W2.2 — sandbox-validated, fs.write-grant required); fan-out results are slot-ordered (results[i] ≡ tasks[i], mixed reject+run batches included); concurrency `[agentic.delegate].max_concurrent` (default 4, clamp 1..=8, env `ZEN_DELEGATE_MAX_CONCURRENT`); lazily registered at first orchestrator turn, kill-switch `[agentic.delegate].enabled`; structured Ok-error outputs, never panics the round; fan-out hard cap `FANOUT_MAX_TASKS=8` enforced in `parse_requests` with structured rejection (T216 — the recorded ≤8 value, previously advisory-only); `[agentic.delegate].context_inheritance` (default false, T215): on ⇒ sub-session inherits the parent's retrieved knowledge block via `compose_sub_session` (budget halved per depth, gates-audit additive `{inherited_notes, inherited_chars, skill_hits}`) |
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

- **O16 落地 + O17/O18 生产实测双 P1 修复 + 重新部署（2026-10-17 当日续；报告 §9.5/§9.6）** — owner 裁决「Fix O16 now」→「Fix both now」。门禁：`bin/lint` exit 0；`bin/test` **3271 → 3279 passed / 0 failed / 25 skipped**（O16 +7：gateway `knowledge_degraded.rs` ×4 + vault 内联 ×3；O17 三个 vacuous 测试 3 换 3 = 净 0；O18 +1 `resource_limits_test.rs`）。部署：`bootout` → `cargo install --path crates/zen --force --locked` → `bootstrap`；daemon **PID 54622**、`Binary=Daemon 0.0.9`、无 STALE、`Store="ok"`；复验（15:51:44）`contributing_tiers` **3 → 4**、零 EAGAIN、零列名错误、响应无 `degraded` 键；`zen doctor` 10 探针 9 绿（唯一 FAIL = 既有已裁决的 outbox）。
  - **O16（已修 — 查询侧 degraded 可见性）**：`SearchDiagnostics { query_embedding_degraded, query_embedding_unavailable }`（正交两标志：前者 = hash 伪向量被当正常向量，后者 = 根本没有向量可比）；原 `search_fused`/`search` 保持签名、成为**字节一致的薄 wrapper**（`*_with_diagnostics` 承接新逻辑，既有调用点零改动）；tier-3 arm 改 tagged 嵌入；gateway 累积 → 去重 push 进**既有** G4-01 `degraded` 数组（`query_embedding_degraded_entry`：tier 固定 `Some(3)`、unavailable 优先、`existing` 含 `"vec"` ⇒ `None` 不重复）⇒ 复用既有 TUI 渲染器，**不重开任何 owner 决策**；无降级时 `degraded` 键完全不出现（健康路径字节等同，生产实测确认）。测试期望由**一次实时探针 embed 推导**（宿主无关，不假设本机有无真 provider）。**未执行（诚实留档）**：以死端口强制 degraded 的**端到端外观**尚未亲眼见到 —— 今日降级分支证据 = 探针推导单测，生产证据 = 健康路径无 `degraded` 键。
  - **O17（已修 — tier-3 vec0 语义检索在生产从未成功过）**：症状 `tier3 (vec0) failed during fusion error=sqlite error: no column found for name: file` **5/5**，tier-3 列表被 `warn!` 丢出 RRF fusion（`/search` 实际只剩 graph/PPR，返回 `@ai`/`@anytype` 类条目），agent 工具 `tier3_search` 亦返回同一错误 —— 正是用户报的「agent tools 不好用」。根因**两层**：① `EmbeddingsRepo::search` 投影 `nm.file_path`，而返回类型 `VecSearchResult { file, line, content }` 是 `#[derive(FromRow)]` ⇒ sqlx 按**字段名**映射 ⇒ 必须存在列 `file` ⇒ prepare 阶段失败；② 即使改对别名，`nf.content` 读的是 **contentless** `notes_fts`（migration 001，`content=''`）⇒ 内容恒空（T222 只修了 `NotesRepo::search` **一侧**）。修法：`nm.file_path AS file` + `0 AS line` + `'' AS content` + **去 FTS join**（顺带停止静默丢弃「有向量无 FTS 行」的笔记）+ Rust 侧逐条 `crate::notes_repo::read_canonical_snippet(&r.file)` 回填；该 helper 由私有改 `pub(crate)`、**共享 `SNIPPET_FILE_MAX_BYTES = 256KiB`**（防两份上限漂移）。**测试空缺是根因之一**：3 个 vec0 测试**全部 vacuous**（接受任意错误 / 走空 `Ok(())` 分支无断言）⇒ 查询与写入彻底失效也全绿，已替换为真断言（canonical 内容 + round-trip + 替换语义）。**连带新缺陷（已修）**：`insert_entity_embedding` 用 `INSERT OR REPLACE`（vec0 不支持该语义；note 侧注释早已写明此陷阱）⇒ 同一 `notion_id` 二次写入**必失败** = entity 向量**写一次即报废**（重嵌入永远失败）；新测试首跑 FAIL，改 delete-then-insert 后 PASS，今日生产 `notion_embeddings` 0 行故零影响。复验：`no column found for name: file` 末次 **15:18:52（部署前）** → 部署后零次；返回真实 canonical 内容 940–1271 字符/条。agent 工具侧走**同一函数**（`Tier3SearchTool::invoke` → `Tier3Search::search` → `EmbeddingsRepo::search`，已 grep 核实唯一路径）⇒ 传递覆盖；**未**跑真实 agent 轮次端到端复现 `tier3_search`（需 LLM 决定调用，随机且昂贵）——记录为剩余未验证环节。
  - **O18（已修 — `RLIMIT_NPROC=50` 使 daemon 无法 spawn 任何子进程）**：症状 `failed to spawn rg — falling back to in-process scan error=Resource temporarily unavailable (os error 35) rg=/opt/homebrew/bin/rg` **5/5** ⇒ 2026-10-16 落地的 tier-1 **rg 快路径运行时失效**（`resolve_rg_binary` 能正确定位，spawn 仍 EAGAIN；in-process walker 静默兜底，**掩盖了那次 E2E 的「绿」**）；`shell.exec`（tokio::process → `posix_spawn`）同样不可用。根因：`apply_resource_limits()`（`sandbox.rs:815`，**唯一调用点** `ZenWiring::new` → `wiring.rs:327`）把**自身**软 `RLIMIT_NPROC` 设为 50（FR-038），而该限额按 **real UID 全登录会话**计数（macOS/Linux 同）⇒ 本会话持有 **550** 个进程（daemon 自身仅 33 线程）⇒ 每次 spawn EAGAIN；旧注释已见 EAGAIN 但归因「子进程超限」（**归因错了**）。**为何测试没抓到（结构性）**：遏制只存在于**测试 harness** —— `wiring.rs` 有 `#[cfg(not(test))]` + `ZEN_SKIP_RLIMIT`/`NEXTTEST`/`NEXTTEST_PROFILE` 跳过，且 3 个 gateway 集成测试还主动提升/恢复软限制 ⇒ 集成测试全绿，而 **launchd 启动的生产 daemon 没有任何豁免**。修法：**移除 NPROC 上限**，保留 `NOFILE=256` + `CORE=0`（真·per-process 遏制）；rlimit 无法表达 per-subtree 上限（需 cgroups/job objects）⇒ 不发明机制（宪法 XI）。测试：新增 `crates/zen-core/tests/resource_limits_test.rs`（断言 NPROC soft 不变 + 施加限制后仍能 spawn 子进程；带 `NofileGuard` Drop 还原以免污染同进程测试），3 个 gateway 测试同步清理 NPROC 条目与已失真注释（NOFILE 恢复改直接内联调用，消除 clippy `single_element_loop`）。复验：末次 EAGAIN **15:18:52（部署前）** → 部署后零次，且该窗口**无 in-process fallback 警告**（spawn 失败分支**必** warn ⇒ 无 warn 即 spawn 成功）。**诚实标注**：`zen sandbox test` 通过**不构成** O18 证据 —— 该 CLI 路径不经过 `ZenWiring` ⇒ 根本不施加限制（已 grep 核实唯一调用点）；确定性证据是单元测试，生产侧证据是「失败分支必 warn」+ 部署边界前后 warn 的消失。
  - **共因教训（两条同源，本轮最大收获）**：O17 与 O18 都是「**测试通过但生产不可用**」—— O17 是断言缺失（vacuous），O18 是刻意豁免（harness skip）；共同点：**绿色从未代表生产状态**。两条均已写入报告 §10「被证伪的假设」表（新增 5 行：vec0 测试全绿≠路径正常 / `zen sandbox test` 通过≠NPROC 正确 / 集成测试全绿≠生产 rlimit 已覆盖 / 2026-10-16 rg-hybrid E2E 绿≠rg 可用 / `INSERT OR REPLACE` 可用于 vec0）。

- **state.db 索引累积根治 + 生产清理（2026-10-17，承上一条事故）** — owner 裁决由「仅出报告」改为「修代码 + 清理数据」。生产 `~/.zen/data/state.db` **948 MiB → 16.7 MiB**（993,456,128 → 17,489,920 B），`notes_meta` 2,537 → **127**（快照行归零），`notes_fts` 599,777 → **127**，孤儿 FTS 行 597,240 → **0**，`note_embeddings_rowids` 282 → **127**，`freelist` 82,394 → **0**，`integrity_check = ok`；连续第二轮 reindex 三表计数与字节数**完全不变**（增长停止）。`~/.zentest` 同办 62.7MB → 17.4MB。同日新发现的 **O14（向量静默陈旧）按 owner 裁决（选项 A）一并修复**：migration 012 `notes_meta.embedded_hash` + `Reindexer` 差异重嵌入；**同日再发现的 O15（向量语义无效）按 owner 裁决修复**：生产/测试均切 `local_provider="ollama"`（`qwen3-embedding`，原生 4096 维）+ `degraded` 计入 reindex 报告/日志/CLI（详见下方 bullet 与报告 §9.2/§9.3）。门禁：`bin/lint` exit 0；`bin/test` **3254 passed**（O7–O9 批）→ **3261 passed / 0 failed / 25 skipped**（+7：migration_012 ×2、embedded_hash ×3、判别性向量刷新 ×1、reset 不变量 6 ×1）→ **3263 / 0 / 25**（O15 +2）→ **3271 passed / 0 failed / 25 skipped**（O14-C +8：worker 测试 ×7、degraded 保持脏判别测 ×1）。**随后同日的选项 C 落地 + metis 复核**：30min `embedding-refresher` worker（探针先行、首个 degraded 即中止）+ degraded 向量**不再标记为新鲜**（行保持脏）；metis 新发现 **O16（查询侧 hash 回退无可见告警）仍未修**（**→ 同日已修并部署，见本区首条 bullet 与报告 §9.5**），详见 §9.4 与下方 bullet。记录：`docs/specs/005-agentic-loop/review-2026-10-17-daemon-hang-incident.md` §9/§9.1/§9.2/§9.3/§9.4。
  - **O7 快照污染（已修）**：`reindex.rs` 新增 `EXCLUDED_DIR_COMPONENTS = ["iterations"]`（**按路径组件判定**，非子串）+ 一次性删除既有 2,410 行快照（E5 版本快照占索引 95.0%）。
  - **O8 rowid churn + FTS 孤儿 99.6%（已修）**：`index_note` 改**确定性 rowid**（`sha256(id)` 前 8 字节取 i64），`notes_fts` 写同一 rowid ⇒ `INSERT OR REPLACE` 命中同一行；**migration 011** 置 `contentless_delete=1`（否则旧词项经 rowid `JOIN` 命中**当前**笔记 ⇒ 假阳性）+ 删除触发器改按 `old.rowid` + 一次性清 `content_hash` 触发全量回填。附带修正 `--rebuild-fts`：原「重置后提前 return」在 contentless 表上静默空转（FTS5 `rebuild` 不支持），改为「重置（`notes_fts` 用 `delete-all`）+ 同轮强制全量 reindex」。
  - **O9 vec0 碎片 282 活向量占 432 MB（已修）**：新增 `zen wiki reindex --reset-embeddings` —— **进程内** DROP + 重建 vec0（plain `sqlite3` 报 `no such module: vec0`），DROP/CREATE 同批，并**同批清空 `content_hash`** ⇒ 中断后自愈（否则留下「有哈希、无向量」的静默缺口，永久跳过重嵌入）；`force_full` 覆盖该 flag，单轮完成重嵌入。**否决「migration 012 清表」**：daemon 侧 reindex 走 `Reindexer::without_embeddings()`（`zen_loop.rs:1548`），迁移清表会让其他安装的向量永久为空。
  - **新发现 O13（未修，非紧急）**：id 回退 `auto-{文件 stem}` 跨目录同名即冲突 —— 生产 154 个非 iteration 文件中 34 个 basename 重复（27 对内容字节不同）⇒ 双方争同一 `notes_meta` 行，每轮恒报 `Updated 27 files, 127 unchanged`，其中一方按自身路径不可搜且白做解析/FTS 替换。**经实测不造成 DB 增长**（同 PK 的 `INSERT OR REPLACE` **复用** vec0 chunk 槽位：副本连续 3 轮 `chunk_bytes` 恒为 16,846,848）⇒ 历史 95× 放大的真因是旧的不稳定 id（每轮新 PK），已根治。修复方向：id 回退带路径判别，或跳过 `merged_into` 快照页。
  - **新发现 O14（已修 — 选项 A）**：vec0 向量原先**无新鲜度标记**（`note_embeddings(note_id, embedding)`）+ 生产唯一的自动 reindex（zen-loop 5min）`without_embeddings()` **仍推进** `content_hash` ⇒ 被 daemon 改写的页面向量永久陈旧且不可检测（"vec0 归 wiki-compiler 30min / memvid-indexer nightly" 的注释经 grep 证伪：两者都不碰 `note_embeddings`；`insert_embedding` 仅 CLI reindex 可达）。清理后的 127 条向量只是**新鲜度语义上**的首次一致（`embedded_hash` 与 `content_hash` 对齐）——**其语义有效性当时尚未被检验，而 O15 随后证明它们全是 hash 伪向量**；此后开始静默漂移；tier-3 语义检索与 T214 注入据此排序。**修复**：migration 012 加 `notes_meta.embedded_hash`（`TEXT NOT NULL DEFAULT ''`，加法式无回填）+ `index_note` 显式置脏 + `NotesRepo::{mark_embedded, all_embedded_hashes}` + `Reindexer` 向量新鲜度门控（`needs_index == false && embed` ⇒ **向量-only 刷新**，不触 `content_hash`/`notes_fts`，计入 additive `files_reembedded`；`embedded_hash == content_hash` 视为新鲜；预载失败 fail-open）；`without_embeddings` 由此变成「只标脏、不落向量」⇒ 自愈且零周期成本；`--reset-embeddings` 同批清 `embedded_hash`（空 vec0 绝不与新鲜 hash 共存）；`zen_loop.rs` 被证伪的注释已订正。**边界**：A 使陈旧**可检测、可修复**，但**不自带自动刷新**——该缺口随后由 O14-C（选项 C，30min `embedding-refresher` worker）闭合，见下方 bullet 与报告 §9.4；报告 §9.2，只读核查 `select count(*) from notes_meta where embedded_hash != content_hash`。
   - **运维陷阱（新增）**：launchd `KeepAlive` 会在 `zen serve stop` 后数秒内重启 daemon（实测新 PID）⇒ 清理/替换二进制前必须 `launchctl bootout gui/$UID/dev.zen.serve`（完成后 `launchctl bootstrap` 恢复）；`pgrep -f "zen serve start"` 必须为空才能动 `state.db`。
   - **新发现 O15（已修 — 语义向量切换 + `degraded` 可观测）**：生产向量**语义无效** —— 链是 `local_provider="fastembed"`（本机无 `libonnxruntime.dylib` ⇒ `ort` 在 `lib.rs:234` panic，被既有 panic 容器标 dead），**ollama 从不在链里**，故每条向量都是 `hash_embedding()`（字节 rolling-hash + 符号 + unit-normalize + 零填充 4096）——**捕捉字节重合、不捕捉语义**；而 `degraded` 标志只在 agent 工具 JSON 暴露，reindex 报告/日志只报 `embedded=149`（假健康）⇒ §9.2 刚标记「真正新鲜」的 127 条其实一直不语义。**修复**：① 生产与 zentest 各自新增 `[embeddings] provider="local" / local_provider="ollama" / model="qwen3-embedding"`（**配置按 root 解析，两条必须同步**——分叉被新计数器当场抓到：zentest 补配置前 `degraded=123/123`，耗时 1:00.37 = fastembed 60s init 超时签名）；② 可观测性：`compute_embeddings_for_text_tagged()` + `aggregate_chunks_tagged`（`degraded` 沿 chunk 取并集，聚合数学逐字节不变）+ `ReindexReport.embeddings_degraded`（唯一计数缝 `store_embedding`，仅 insert+mark 双成功后计数）+ 日志 `degraded=` + CLI `degraded>0` 追加告警（=0 时输出逐字节不变）。**验收（实测）**：生产 `scanned=153 updated=153 embedded=153 embed_skipped=0 errors=0 degraded=0`、1:59（hash 回退约 2s）、日志 `computed provider="ollama" dim=4096`、DB `notes_meta=127 / stale=0 / empty_embedded_hash=0 / rowids=127 / fts=127 / integrity=ok / 17,600,512 B`；zentest 2:11 无告警、`rowids=77=notes_meta`；本轮 degraded 告警 0（历史 33051 条全在切换前，末条 12:20）。**边界（实测）**：daemon 13:22:33 重启（`provider returns 4096-dim` 预热）→ 13:24:41 一轮 zen-loop reindex `updated=31 embedded=0` ⇒ `stale` **0 → 30**（约一个 5 分钟周期）——自动刷新已由**选项 C**闭合（30min `embedding-refresher` worker，见下方 bullet）；临时手段 = 定期手动 `zen wiki reindex`。**运维规则**：CLI 重嵌入必须 **proxy-clean**（本机 shell `http_proxy=0.0.0.0:1087` 死 Privoxy 会劫持 loopback；daemon 由 launchd 启动、环境本无代理故不受影响）；`--reset-embeddings` 会重建 vec0 ⇒ **必须先停 daemon**。**fastembed/ORT 集成按 owner 裁决延后留档**：唯一入口 `ORT_DYLIB_PATH`（`ort-2.0.0-rc.13/src/lib.rs:224/234`，fastembed 自身从不 init ORT）；brew `onnxruntime 1.30.0`（vs client rc.13 ≈ ORT 1.20/1.21，ABI 需实测）；BGE-M3 缓存不完整（2.1G，snapshot 仅 `onnx/`、缺 tokenizer/config、3 个 0 字节 `*.lock`）；三条候选（A 纯 env / B 加 `ort` 依赖 + `ort::init_from` / C build 期下载）。
   - **测试自伤点修正（O15 附带）**：初版两测断言「本机没有可用 provider」这一**宿主属性**（装上 onnxruntime 即失败）⇒ 改为由一次探针 embed 推导期望（宿主无关），并加确定性断言（空输入零向量且非 degraded + 多/单 chunk 标志一致 + 与未标记路径逐字节相同）。
   - **O14 选项 C 落地 + degraded 不标新鲜 + metis 新发现 O16（2026-10-17，当日续；报告 §9.4）**：O15 边界实测暴露缺口——daemon 重启后 `stale` 于一个 5 分钟周期内 **0 → 30**、13:39 达 **49/127**（生产唯一的自动 reindex 走 `without_embeddings()`，只标脏不落向量），owner 据此裁决 ①「实施选项 C」② degraded 向量「仍写入，但不标记新鲜」。
     - **degraded 不再固化（`reindex.rs::store_embedding` 单缝）**：degraded ⇒ 仍 `insert_embedding`（保留检索兜底）+ `files_embedded += 1` + `embeddings_degraded += 1`、但**跳过 `mark_embedded`** ⇒ `embedded_hash` 保持 `''` ⇒ 行保持脏、下轮重试、`stale` 如实；non-degraded ⇒ `mark_embedded` 成功后才 `files_embedded += 1`（原语义不变）。`mark_embedded` 失败**推入 `report.errors`**（CLI `Errors:` 块可见）——唯一被提升为错误的 skip 分支，另两个（无 db client / 空正文）保持 counter-only 以免误报。
     - **选项 C worker（新建 `crates/zen-agents/src/scheduler/workers/embedding_refresher.rs`）**：cron `0 */30 * * * *`、**双 profile**（Full+InApp，`worker_ids()` +1/+1）；用**启用嵌入**的 `Reindexer`（增量靠 `embedded_hash`），不触 vec0 DROP / `content_hash` / `notes_fts`；**探针先行**——run 开始先做一次 tagged embed，若 degraded **立即中止**（防止在无真 provider 的机器上把整批向量重写成 hash，即 O15 的固化路径），`AtomicBool` 只告警一次；审计 `loop.embedding.refresh` **仅**在 `refreshed>0 | degraded>0 | aborted` 时发（干净空转不写 = flood 纪律；abort/degraded 必发 = 对静默降级的正面回应）。**已知限度**：探针只覆盖 run 开始时；provider 中途失效则该轮带 degraded 跑完（行保持脏、下轮中止），代价为每文件一次有界超时（240s）。
     - **验收（实测）**：`bin/lint` exit 0；`bin/test` **3271 / 0 / 25**（+8）；部署后运行中 daemon 日志 14:10:03 `scheduler: worker registered worker=embedding-refresher schedule=0 */30 * * * *`；新 daemon PID 14863、Binary=Daemon 0.0.9；baseline `stale=49/127`（14:07 只读）；**首次触发 14:30:12 实测 `refreshed=74 degraded=0 aborted=false elapsed_ms=37991`**（审计 `loop.embedding.refresh` 同数），`stale` **49 → 18**（14:32）→ 37（14:36，loop 每周期 +36 的锯齿；18 个 basename 互不相同 ⇒ 是 loop 在 refresher 快照窗口内改写的**竞态残留**，非 O13 冲突集）；`integrity_check=ok`、字节数与清理后**完全一致**（17,604,608 B ⇒ 刷新不增长 DB）。**特性**：锯齿而非单调收敛（峰值 ≈ 30min × 36/5min，故语义检索最多落后 ~30min、平均 ~15min）——sweep 无单轮预算，每轮必收敛；未加与 loop 的互斥（无损坏路径，收益仅瞬时一致性）。
     - **metis 复核新发现 O16（未修，待裁决）**：**查询侧** hash 回退**无可见告警** —— ollama 在查询时刻不可用 ⇒ `compute_embeddings_for_text`（`embeddings.rs:234-236/87-88`）落 hash 查询向量，而 `search_fused` 仍以**权重 1.0** 融合（`service.rs:115`）⇒ 语义检索质量静默劣化，仅日志可见。metis 排名 #1（约 1–2h、单 PR）：把查询侧 flagged 结果透传进**既有** G4-01 `degraded` 数组（`knowledge.rs:109-141`）→ 既有 TUI 渲染器（`commands.rs:532`），不重开任何 owner 决策。附带 under-specification：默认 **TUI-chat 注入**（`tui/chat.rs:102-104`，默认 Fast ⇒ `tiers=["fts"]`）与默认 **agent-turn 注入**（`orchestrator.rs:1008-1018`，默认 `"fts"`）均为 **FTS-only** ⇒ vec0 只在 `/search`、`zen chat`、TUI-Full、`tier3_search` 生效。metis 排名 #3 即选项 C（本轮已实施，排名被取代）；其 #2（搜索响应加 staleness 提示）仍开放。

- **生产 daemon 无限挂死双根因根治 + 部署路径修复（2026-10-16/17）** — 触发：daemon 挂死只能 `kill -9`（"需要kill 进程才可以"）；embedding 预热已实施却仍不可用。两个**独立**根因，同一失效形态 = "无界阻塞 + 跨调用持锁"：
  - **A. ollama embedding 路径缺 loopback no-proxy（已修）** — LLM 路径有 `.no_proxy()`（`providers/ollama.rs:32`），`embedding.rs` 的 `OllamaEmbeddingProvider::embed` 却是裸 `ollama::Client::builder()`；env proxy `http_proxy=https_proxy=http://0.0.0.0:1087`（失效 Privoxy）静默 stall loopback ⇒ embed 无限阻塞（实证：`curl localhost:11434/api/embed` → `500 Internal Privoxy Error`，清代理秒回）。修复：loopback 走 `reqwest::Client::builder().no_proxy().timeout(EMBED_REQUEST_TIMEOUT_SECS=240)` + `.http_client(http)`（镜像 LLM 路径；`.build()` 必须在各分支内 —— rig `ClientBuilder` 是 marker 类型）；`is_loopback_url` → `pub(crate)`。
  - **B. fastembed 模型下载经 hf_hub/ureq 走同一死代理且无界（已修）** — 线程采样实锤：`embed_with_providers → fastembed::TextEmbedding::try_new → hf_hub::ApiRepo::download → ureq::BodyReader::read` 阻塞。**`catch_unwind` 捕获不了 hang**，而 240s 超时只覆盖 ollama。修复：init 改**独立线程 + `recv_timeout(FASTEMBED_INIT_TIMEOUT_SECS=60)`**，超时即 Err → 标记 dead → 降级 ollama（保留 panic 容器与 `AssertUnwindSafe` soundness 依据）。**代理行为退化是关键**：生产 10-15 抓到的是 `fastembed init error: Failed to retrieve onnx/model.onnx_data`（快速报错），今日变为**静默 stall** —— 同一缺陷从"快速失败"退化成"永久挂死"。
  - **C. 跨 provider 调用持锁（已修）** — `embed_with_providers` 原在调用 provider 期间持 `dead: Mutex<Vec<bool>>` ⇒ 任一 provider 挂死 = daemon 全量 embed 永久阻塞（scheduler 仍在跑，故假健康）。改为先 `dead.lock().unwrap().clone()` 释放锁再调用，标记 dead 时才重新取锁。回归测试 `concurrent_embeds_overlap_while_a_provider_is_running`（GatedProvider，双线程 peak==2）。
  - **新发现 O19（已修 — Fix A 同日落地，部署后健康巡检发现）**：`Frontmatter parse failed for … — indexing as raw` 告警洪水，全量 **896,591** 条（首见 2026-09-14，多为已清理垃圾页），**清理后仍持续 ~168 条/小时**（14 条/5 分钟周期，永不停止）。**两个独立成因**：① 13 个假设页 —— writer 以 YAML **块序列**写 `Vec` 字段（`evidence_refs:` + `  - "…"`），而 reader `note.rs:208 parse_frontmatter` 是手写逐行解析、对非 `key: value` 行硬错（`note.rs:219`）⇒ **writer/reader 对同一格式理解不一致**；② `wiki/log.md` **本无 frontmatter**（OKF §6 豁免生成物）却被 `extract_front_matter`（`note.rs:125`）报 `missing leading --- delimiter` ⇒ **「没有 frontmatter」被当错误**，尽管纯 markdown 笔记合法。**真实损失仅两项（刻意不过度声称）**：这 13 页的 YAML 块混进其 FTS/语义内容；告警洪水喂给无轮转的 923MB `zen.log`（O2）。**磁盘数据无损**（`slug` 未被 reader 映射 ⇒ 即使解析成功这些页仍是 `auto-*` id，故不是身份丢失）⇒ 定级 **P3**，与 O17/O18 的 P1 不可比。**顺带查实的 O13 具体实例**：`missing_entity-why-these-letters-matter.md` 同时存在于 `wiki/wisdom/hypotheses/`（1114 B）与 `archive/2026-10/`（1374 B）⇒ 同 stem ⇒ 同 `auto-` id ⇒ 同一 `notes_meta` 行（现指向 archive 副本）⇒ 假设页在**自己路径下不可搜**，正是 §9.4 的 `Updated ~27 files` 地板/锯齿机制（**不由 O19 修复项解决**）。**修复选项**：(A) 修 reader（推荐，~10 行+测试）容忍前导 `- ` 列表项 + 无前导 `---` 降级 debug；(B) 修 writer 为行内式（只治当前形状，未来任何 `Vec` 字段再撞同墙 ⇒ 较弱，宪法 XI）；(C) 仅记录。报告 §9.7。**→ 同日已修（owner 裁决 Fix A，17:41 部署；daemon PID 83444）**：`parse_frontmatter` 显式跳过块序列行（`parse_kv_line` 的 `find(':')` 对含冒号列表项产出畸形 key 而非 None，必须显式判断）+ 新增 `pub(crate) has_frontmatter`（`extract_front_matter` 复用）+ `reindex.rs` Err 分支按其分流 `warn!`/`debug!`（**回退元组字节不变**——id/content/tags 行为零变化）；边界 caller 核实零回退（`pipeline.rs:1747` 只扫 inbox；`service.rs:398` `.ok()`+`unwrap_or(false)` 只会更包容）。复验（实测）：末条 WARN **17:39:45**（部署前）→ 部署后**零条**（1 个 daemon 周期 + 2 次 CLI 全量 reindex；新二进制 17:45:01 周期 `errors=0 degraded=0` 即洪水源路径）；首次 reindex `Updated 33`（13 页内容去 YAML + churn）后 `notes_fts MATCH 'evidence_refs'` = **0**（判别断言：修复前必命中）；id 语义不变（仍 `auto-*`），二次 `Updated 24` = 既有 O13 地板。门禁：`bin/lint` 0；`bin/test` **3279 → 3282 / 0 / 25**（+3：note ×2 + reindex_gate 判别 ×1）。两项未验证环节（真实 agent 轮次复现 `tier3_search`、zentest 死端口 degraded 外观）owner 裁决 **"Neither, leave recorded as unverified"**。
  - **验证（proxy 在位）**：`~/.zentest` → `fastembed init timed out after 60s` → `marking dead` → `embedding warmup complete dim=4096 ms=60003`（修复前 = 永久挂死）；生产 `--locked` 部署 + `zen serve restart` → `fastembed init panicked (missing onnxruntime dylib)` → `warmup complete dim=4096 ms=1`。两种失败模式（stall / panic）现已**均有界**。门禁：`bin/lint` exit 0；`bin/test` **3247 passed / 0 failed / 25 skipped**。
  - **部署路径缺陷（已修 + 已记录）** — `cargo install --path crates/zen` **不带 `--locked` 必失败**：`memvid-core 2.0.140` 要求 `zip ^7.1`，而 7.1.0–7.4.0 **已被 crates.io 全部 yank** → 离线解析失败；`--locked` 复用 `Cargo.lock`（pin zip 0.6.6/7.2.0/8.6.0）即通过。`bin/install` 同步补 `--locked`（原脚本未带 ⇒ 无法编译）。
  - **新发现（未修，已报告）** — ① `~/.zen/logs/zen.log` **766 MB 且无轮转**（retention 覆盖 `audit.jsonl` 但不含 `zen.log`），与先前"磁盘耗尽致 cargo 失败"环境问题同源，建议纳入 retention；② `serve.err.log` 被 `Warn: Failed to load .env file: path not found`（launchd cwd=/）刷屏 385KB；③ 生产 host 缺 `libonnxruntime.dylib` ⇒ fastembed 在生产**结构性不可用**，每次启动白走一次 init（`[embeddings] local_provider="fastembed"` 与运行现实不符）。
  - **次级 bug（已修）** — **B13** `zen chat` 短回复恒显 `✓ 0 tokens`（`len()/4` 字节整除）：新增 `crates/zen-cli/src/session.rs::estimate_tokens`（`chars().count().div_ceil(4)`）并替换 4 处（`cmd/chat_command.rs:72,78`、`tui/chat.rs:372,451`），复验 `✓ 1 tokens`。**B7** `INPUT_HINT` 宣称 `Ctrl+R=search` 但全屏 TUI 吞键：抽取 inline 的 reverse-i-search 状态机为共享 `tui/history_search.rs::handle_key(key, app) -> bool`（`inline_handler.rs` 净 −141 行，零重复），全屏 `handler.rs` 在**保留 session-picker rename 优先级与 slash-popup 守卫**的前提下委派之，新增 5 条全屏测试。
  - **文档校正** — `zen doctor` 探针数：README "8" 与 AGENTS.md 内文 "9" 均为陈旧值，权威实测 **10**（config / state.db / memories / daemon / daemon-version / loop / provider / vault / outbox / schema-compat，`zen doctor` 实跑确认）；AGENTS.md 探针清单补第 10 项 schema-compat。注：生产 `doctor` 当前 exit 1 系 `outbox` 按设计 FAIL（7 个 staged brief + qqbot 未配置，用户已裁决保留待 30 天 retention 清理）。

- **生产事故三连修复 + tier1 rg 混合 + 路径锚定 + 并行 dispatch（2026-10-16，续 /search 语义化）** — 触发：用户报告 `/search` 经 gateway 仍不可用（`~/.zentest` 日志实证 `tier=1` 直路由 + `failed to spawn ripgrep`），并明确要求 **rg 保持为快路径**（修 spawn 环境 bug，而非替换引擎）。本轮六项：
  - **P1 CJK char-boundary panic（已修）** — `crates/zen-vault/src/distill/pipeline.rs` 的 `auto_link_wikilinks` 按字节遍历 + 字节索引切片，中文笔记触发 `byte index N is not a char boundary` panic。改为全 char 化遍历 + `matches_entity_at` 辅助（段式 `to_lowercase` 匹配，规避 Unicode `to_lowercase` 长度失配）。测试 `crates/zen-vault/tests/auto_link_wikilinks_test.rs` 4 条（中文实体 / ASCII 实体 / 跳过 code+wikilink / 词边界）。**此 panic 此前每周期杀死 zen-loop worker，掩盖了下一项事故**。
  - **P1 hypothesis 爆炸事故（已修 + 生产清理）** — vault 累积 **26,566** 个垃圾 md（`missing_entity-missing_entity-…` 链式命名）。根因：`zen_loop.rs` verify 输入 = 全量 wiki 清单（含 `wisdom/**`）⇒ 每周期把 wisdom 页报为 `WikiPageWithoutEntities` ⇒ hypothesis 页以 `missing_entity-<前代 slug>` 链式重入 + 截断哈希保证 slug 唯一 ⇒ **永不收敛**（CJK panic 掩盖下无限增长）。修复三层：`graph_verify.rs` 新增 `concept_page_inventory`（排除 `wisdom`/`communities` 目录 + `index.md`/`log.md`）；zen_loop verify 改用之；`distill/hypothesis.rs` `MAX_HYPOTHESES_PER_CYCLE=20` 硬上限。生产清理 **26,582 → 289** md（删 `hypotheses/missing_entity-*` 2878、`orphan-buffett-*`、`archive/*missing_entity*` 21243、`iterations/wisdom`），保留 35 个合法 `orphan-*`。DB 派生索引重建（contentless FTS5 用 `delete-all` 而非 `--rebuild-fts`）：FTS `buffet`=7、`notes_meta`=282、embeddings=282。**收敛实证**：清理后每周期 hypotheses 稳定 22（13 个 `missing_entity-<概念页 stem>` 为 verifier 正确报告的 notion 页缺 entity gap——无链、有界、同 slug 收敛）+ 5 分钟观察零增长。
  - **tier1 rg 混合方案（修 spawn 环境 bug、保 rg 快路径）** — 诊断：daemon 由 launchd 启动（cwd=/、极简 PATH）⇒ 裸 `Command::new("rg")` spawn 失败。`search/mod.rs` 修复：`resolve_rg_binary`（`ZEN_RG_PATH` env → `which::which` → `RG_WELL_KNOWN`=`/opt/homebrew/bin/rg`、`/usr/local/bin/rg` + `$HOME/.cargo/bin/rg`，`OnceLock` 缓存；`resolve_rg_binary_uncached` 供测试）→ 以**绝对路径** spawn（免疫 daemon PATH）；spawn 失败/不可解析 → `search_in_process`（walkdir）兜底（跳隐藏/二进制、总量上限、10s 预算）。测试：PATH 清空仍成功（绝对路径钉）、rg/walker 等价（**排序多重集**——rg 并行 walker 输出顺序非确定）、walker 语义直测、`parse_rg_line`×2、`ZEN_RG_PATH` 覆盖钉。
  - **agent 相对路径锚定（已修）** — daemon cwd=/ ⇒ `fs.read vault/...` 解析为 `/vault` ENOENT。`crates/zen-core/src/sandbox.rs` 新增 `resolve_agent_path`：绝对透传 → cwd 存在命中 → `user_root` 锚定存在命中 → `GLOBAL_ROOT_SUBTREES`（`vault`/`memories`/`logs`/`skills`，故意排除 `data`）新文件锚定 → 否则保持 cwd 相对；接线 9 个 fs 工具。测试 `crates/zen-plugin/tests/fs_relative_anchor_test.rs` 7 条。**沙箱读权限本已允许 `~/.zen`**（`is_zen_path`→Ok，读无 workspace-root 检查），真正缺口是相对路径解析（数据面，非权限面）。
  - **并行多工具 dispatch（已实施）** — `crates/zen-agents/src/orchestrator.rs` `dispatch_timed` 串行 for → `DISPATCH_MAX_CONCURRENT=4` + `futures::stream::iter(...).buffered(4).try_collect()`（保序、hook `Terminate`/budget 经 `try_collect` 传播、非终止错误逐调用隔离合成 `{"error"}`）；HRTB 陷阱用显式 for 循环收集 futures Vec 解决。
  - **fastembed/ort panic 守卫（已实施）** — `crates/zen-provider/src/embedding.rs` 加 `FASTEMBED_BROKEN: OnceLock<String>` + `catch_unwind` 包 `try_new` + `panic_reason`：首次 ort dylib 加载 panic 后短路 Err 让路由器降级（ollama→hash），对治 ort 2.0.0-rc.13 `src/lib.rs:234` `.expect("Failed to load ONNX Runtime dylib")` 直接 panic、无可映射 Err。测试 8/8。
  - **环境纪律（用户明令）**：`~/.zentest` = 开发/测试 home（`bin/zentest`）；`~/.zen` = 本机生产配置——开发调试不得影响生产数据。
  - 门禁：四 crate nextest **1759/0**；`search::` 48/48（3 连跑稳定）；`bin/lint` exit 0。部署：`cargo install` + 生产 daemon restart（Binary 0.0.9 = Daemon 0.0.9，无 STALE）+ `bin/zentest serve start`（用户测试面）。**部署面修正 + 生产宕机根因（本轮修复）**：launchd plist 应指向稳定安装路径 `~/.cargo/bin/zen`（`serve install` 用 `current_exe`）；先前指向 `target/release/zen`，该路径被磁盘清理删除 ⇒ launchd `last exit 78 (EX_CONFIG)` crash loop + 生产 daemon 宕机（`zen serve status` 报 not running）——已 `zen serve install` 重写 plist 修复（launchd `state=running`，pid 稳定）。教训：launchd 目标必须是 target/ 清理不影响的位置。
  - **E2E 实证（2026-10-16）**：隔离 fixture（`~/.zentest/vault/inbox/` 写入**未索引**探针笔记——FTS=0、仅 rg 可扫到）经 TUI `/search` → gateway `knowledge/search` RPC → daemon `SearchService` → tier1 rg **返回命中**，证明 daemon（cwd=/、极简 PATH）环境下 rg spawn 修复端到端生效。**正交观察（未修，已记录）**：`qwen3-embedding:latest`（4.6GB）冷加载会使首次搜索超过 30s RPC 超时（`gateway rpc error -32603`），模型热后即恢复——属 embedding 延迟，与 rg 缺陷无关。

- **知识搜索语义化 + 索引重建（2026-10-15，续 slash 修复）** — 用户要求 `/search` 必须搜索 vault 笔记内容并以语义查询知识库。诊断实证：FTS 仅 50 行（wiki 53 页只索引 1 页）、`note_embeddings`/`notion_embeddings` 全为 0（语义层空转）、单词语查询路由到裸 tier-1 ripgrep（跳过语义+图信号）。修复三层：
  - **auto 路由单词语也走 fusion**：`SearchService::search(tier=None)` 原 `preferred == 2` 才 RRF 融合，改为 `matches!(preferred, 1 | 2)`——单词语查询同样获得 FTS5+vec0+graph+PPR 信号；显式 intent 前缀（`similar:`/`graph:`/`summarize:`）保持单层路由。判别测试 `single_word_auto_query_routes_through_fusion`（陈旧索引行夹具：FTS 有词而磁盘文件无词 ⇒ rg 盲，旧路由返回空、新路由命中）。
  - **fusion 纳入 tier-1 ripgrep 内容扫描**：`search_fused` 第五路 RRF 列表（`TIER1_WEIGHT=1.0`，与 FTS5 同信任级——直接扫文件内容、不受索引新鲜度影响；spawn_blocking 包裹，RG_TIMEOUT 10s 沿用）——**未索引/陈旧的 vault 内容现在也可被融合搜索找到**。测试 `fused_search_scans_unindexed_files_via_ripgrep`（未索引文件经 ripgrep 列表浮出）。
  - **/search 空结果提示**：`[gateway] No results for 'x'` 后附一行 `Tip: zen wiki reindex ...`（索引陈旧时的可操作指引）。
  - **数据面重建（实跑）**：`zen wiki reindex` 全 vault 扫描——FTS 50→108 行（inbox 55 + wiki 53，wiki 从 1→53 全覆盖）、`note_embeddings` 0→62（embeddings 默认开启，provider→本地→hash 三级回退）。daemon 的 `scan_dirs=[inbox, wiki]` 原本就正确。
  - **诚实记录**：`/search buffet` 仍将返回空——vault 中确实不存在 buffet 内容（2026-09-03 写入被拒后从未落盘）；语义搜索无法凭空找到不存在的内容。补救：TUI `/note` 或让 agent 写 `vault/` 相对路径。运行中的 daemon 为旧二进制，重启后（`zen serve restart` + 新构建）单词语语义路由生效。
  - 门禁：zen-vault search:: 44/44；三 crate nextest **1196/0/19**；`bin/lint` exit 0。

- **Slash 命令与工具审计路径修复（2026-10-15）** — 用户报告三症状：`/search` 显示 `[gateway] No results for ' letter'`（前导空格）、非意图 slash 执行、工具审计写入工作区 `logs/` 而非全局。三根因全部修复：
  - **工具审计路径全局化（Path Spec v2 违规）**：`ZenWiring::audit_log_path` 旧解析 = 首个 workspace root 下 `logs/audit.jsonl`，无 workspace 时回落 **cwd 相对路径**（launchd daemon cwd=/ 会写 `/logs/`）⇒ tool-dispatch 审计从未进入全局轨迹（实证：全局文件 8 行全是 loop 类，零 tool 记录；工作区残留 5 条 2026-09-03/04 buffett 写拒绝记录）。改为 `zen_core::paths::user_root().join(LOGS_DIR).join("audit.jsonl")` 全局唯一；`ToolAuditHook::default` 与 `PluginApi::new` 的 cwd 相对默认同步全局化（三处同源）。钉测试 `audit_log_path_is_global_not_workspace_or_cwd`；两个 FR-048 hook 隔离测试改用显式临时路径（它们测隔离语义，`with_isolation` 本就是显式可覆盖 API）。工作区残留 5 条已并入全局（8→13 行）后删除。
  - **slash 参数余量 trim**：`handle_slash_command` 的 `splitn(2,' ')` 余量未 trim ⇒ `/search  letter`（双空格笔误）把 `' letter'` 前导空格原样送进用户可见消息与 FTS 查询。抽出 `split_slash_input`（token + trim 后余量；空余量归 None——顺带修复 `/model ` 旧显示 "Unknown provider: " 现显示 picker）+ 4 个纯函数测试。
  - **inline Enter-with-popup 对齐全屏语义**：inline Enter 原把 buffer 重写为裸命令并**立即提交**（T084 codex 语义）——用户只想补全再输参数时命令被意外执行（"not intent to execute slash"）。改为补全 `/cmd `（带尾空格）不提交，与 handler.rs 全屏 Enter 一致；钉测试更名 `slash_popup_enter_completes_without_submitting`。
  - **根因链说明（无代码变更）**：`/search buffet` 无结果的直接原因是 2026-09-03 `fs.write notes/finance/buffett_letters_archive.md` 被 sandbox 拒绝（"not under any workspace root"——错误文本已含 `vault/...` 补救指引），笔记从未落盘，搜索正确返回空。补救：让 agent 写 `vault/` 相对路径或 TUI `/note`。
  - 门禁：zen-plugin/zen-agents 定向测试绿；zen-cli `tui::` 321 passed；`bin/lint` 与三 crate 全量见提交时记录。

- **006 前沿采纳波 W1-W4 + 运行时兼容守卫全量实施（2026-10-14，`/speckit-implement`，spec D8-D19 → tasks.md 全波次）** — `bin/lint` exit 0；`bin/test` **3214 passed / 0 failed / 25 skipped**（基线 3115，**+99**）。触发：工具链审计（`review-2026-10-14-tools-audit.md`：4×P1/6×P2，用户报告的"tier1/tier3 工具失败+多工具链断裂"实证为代码缺陷而非旧版残留）+ 283× migrate 错误事故。
  - **W4 运行时兼容（D16-D18，owner 决策 B+ 容忍+可观测）**： `run_migrations` 改容忍迁移器（codex `runtime_migrator` 同款 `ignore_missing:true` 六字段结构体重建——`Migrator` 非 Clone；checksum/Dirty 守卫保留）+ `embedded_migration_max()`/`detect_schema_skew()` pub API（zen-repo 保持零 tracing）；doctor 探针 #10 `schema-compat`（只读 `mode=ro`，skew ⇒ 信息性 PASS"upgrade when convenient"，绝不迁移/取锁）+ daemon 启动一次性 `warn_schema_skew_once` 行；4 个 worker open 失败点 fail-loud-once（`log_once` AtomicBool 门，fail-open 语义不变）。该策略下 2026-09-19 事故类别整体消失（旧二进制继续正常工作）。
  - **W4.5 dispatch 链 P1 修复**： `dispatch_timed` 每调用错误隔离（synthesize 结构化结果，已执行兄弟结果保留；仅 hook Terminate/Budget 传播并准确标注"blocked (approval/seatbelt/budget)"）——修复"单工具失败丢弃整批+谎报 sandbox"；`partition_granted_invocations` 预过滤（rig `Skip` 全有或全无语义实证）+ 有界 `resolve_invalid_tool_call` 循环——修复"≥2 无效工具名 ⇒ protocol violation 整轮死亡"与"单个未授权名压制全批"（**tier1 不存在为 agent 工具** + tier3 仅 6/13 授权 = 用户症状的精确机制）；`BatchPlan` 按 call-id 映射结果 + 内联应答被拒调用（索引错位修复）；zen-plugin 46 处 fs.* Err→Ok 结构化（**11 处 sandbox 拒绝刻意保留 Err**——外部 `fs_write_contract` 钉 + 安全门语义）+ `zen_error_to_dispatch` 适配器（ExecutionFailed/NotFound ⇒ Ok 结构化）。
  - **W4.6 P2/P3**： execute() 路径 fenced-only 契约钉死 + 死代码 `execute_model_call_stream` 删除（G1-01）；parse_errors 不再丢弃有效子集（G1-05）；**多轮上下文累积**（`accumulated_results` 有界历史注入每轮 CallModel——顺带修复潜在 bug：旧 `## Tool Results` 文本提取恒为空，rig 以 `UserContent::ToolResult` 而非 Text 携带结果，G1-07）；会话历史持久化实际 native calls（G1-08）；delegate 子循环错误标签区分（G1-09）；tier 注册失败 fail-loud ERROR + `tier_tools_registered()` getter（G3-01）；授权表文档化 + 一致性钉（**审计文档"tier2=全 13 agents"有误，实证 12/13——Prometheus 未持有**，测试钉住真相，G3-05）；`/search` degraded-tier 可见性（RPC additive `degraded` 数组 + TUI ⚠ 行，G4-01）+ offline 谎言修正（G4-02）+ 内容截断（G4-03）。
  - **W1 编排即时改进（D8-D10）**： AB/BA 位置互换均值合并 + SplitMix64 Fisher-Yates 种子化 rubric 顺序（momus task-id 种子，单一候选路径字节等同，W1.1）；委派契约 `output_format`/`task_boundaries` + `REASONING:`/`VERIFICATION:` 报告标记提升 + effort-scaling 描述钉（W1.2）；`aggregate_weights.rs` Laplace 平滑每 agent 成功率（N=100 常量，ordering-only，冷启动字节等同，W1.3）；**W1.4 漂移修复**：in-loop 预算门 + stream 路径 pre-loop 预留（:2369-2380 变 reconcile-only，G5-02 关闭）、`intent.rs` 模块文档重写为已编码阶梯、`tokens_spent` 死写字段删除、AGENTS.md 命名同步（本条目）。
  - **W2 结构升级（D11/D12）**： plan.execute `on_failure: retry|skip|halt`（默认 skip 字节等同钉；retry 单任务单次重排队 + 已验证兄弟 checkpoint 复用即最小子图修复；halt 停层链；**零迁移零子行**，原子节点粒度按计划推迟）+ 回滚记忆（plan 失败边界写 `vault/wiki/wisdom/rewinds/<slug>.md` 复用 reflection helpers，256KiB 帽 + InputSanitizer 读点，`recent_rewinds` 下周期注入；**诚实记录**：另两个 D12 边界（TransactionScope/CAS drift）helpers 就绪但未接线——文件所有权边界，seam 已文档化）+ artifact 输出路径约定（sandbox 校验 + fs.write 授权硬门，escape/env/无授权结构化拒绝）。
  - **W3 影子观察者（D13，V13-A.3 纪律——零阈值零激活）**： QE cascade / Bayesian self-escalation（competence 后验充分统计量纯函数）/ Minority Sentinel（休眠 record-only）三观察者 `rung:"shadow"` `loop.decision` 共位发射 + shadow-never-mutates 钉（T168 模式）。
  - **主会话集成修复**： 跨 crate Send 回归（W4.5a agent 在 tracing 宏字段表达式内 `.await`——`warn!(consumed = self.token_budget.tokens_consumed().await, ...)` 两处——宏生成 `Arguments`/`&dyn Value` 跨 await 存活破坏 `TurnExecutor` `Box<dyn Future + Send>`；该 agent 只门禁了 zen-agents 故盲区）提升 await 修复 + daemon.rs `collapsible_if` + commands.rs fmt 漂移。
  - **实施方法记录**： 11 个并行 general-agent 批次（文件集互斥），背景任务系统当日高频 0s 瞬毙（约 40% 发射率，重试即愈）+ 一次任务记录丢失（W3 落盘经 git diff 验证恢复）。

- **docs/designs 收敛为统一架构概要文档（2026-10-13）** — 六份按主题的设计文档（doc-conversion / e1-semantic-lint-writers / gateway-upgrade-management / memory-strength-knowledge-wave / openkb-compile-hygiene / search-mode-defaults，共 1067 行）已并入新增的 **`docs/designs/architecture.md`**（834 行，git 跟踪）并删除。角色分工：architecture.md = **活文档**（始终反映系统当前架构，架构变更时更新它）；docs/specs/&lt;feature&gt;/ = 各特性已完成工作的详细设计（历史，原样保留，仅链接引用）。
  - 结构：§1 系统总览（12-crate 图 + 依赖图）· §2 架构框架（7 层类型分类 / Triple-Enum / Path Spec v2 / 5 层配置继承 + T192 真相规则）· §3.1-3.10 十个子系统（知识管线 / 记忆 / 搜索 / 编排 / 网关 / 路由 / 调度 / 安全 / 数据层 / CLI+TUI，全部带 crate::path 锚点 + 数据流图）· §4 跨切原则 · §5 设计决策注册表（六条 **[consolidated]** 条目 + 006 C1-C6 / 004 design / 003 extension-seams / 002 ADR / 001 memory-arch）· §6 演进时间线 · §7 源索引（被并入文件已注记）。
  - 引用清扫：13 处活引用改指 architecture.md（`semantic_lint.rs:4`、`lint.rs:34`、`zen-core/config.rs` ×4、`strength.rs:2`、`Cargo.toml` 注释、`config/config.toml:29`、`TODOS.md`、`005 tasks.md` ×3+bare、Phase 31 review 工件）；**AGENTS.md 历史条目按仓规不改写**——本条目即合并事实记录。
  - 准确性纪律：specs 为规划文档存在漂移，统一文档以 AGENTS.md 落地事实为准（spec-only 未落地项显式标注），关键事实经代码抽验。门禁：`bin/lint` 0（纯注释/文档改动）。六个删除文件将出现在 git status，由用户手动提交。

- **006 spec 收敛 + D1-class async 路由 hazard 修复（2026-10-13）** — 006-agentic-orchestration 最后一个开放实现项关闭 + spec 账本从 Draft 收敛为 Implemented。`bin/lint` 0；`bin/test` **3113 passed / 0 failed / 25 skipped**（基线 3109，+4）。
  - **D1-class 修复（TODOS P2，叶子级根因）**: `OllamaProvider::health_check`——`enforce_sensitivity → is_local_llm_available` 链中唯一在调用线程上的 `reqwest::blocking` 站点——改 dedicated-thread 探测（`complete()` 同款模式；join panic ⇒ `false`+warn fail-safe，Confidential 走 typed local-unavailable 错误绝不 panic turn）。单点改动端到端覆盖全部 7 个 async 可达链: delegate `run_single`（首恶，006 起无守卫；`plan.execute` 同缝继承）、orchestrator `execute`/redraft、zen_loop `resolve_model_tier`、zen-vault `llm_distill::call_llm`、zen-cli `provider_command`（debug 构建 `zen provider route/test/list` panic 一并治愈）——zen-agents/zen-gateway 生产代码零改动、路由决策零变化。修复前实证 panic，3 个判别性测试翻绿（health_check ×2 runtime flavor 直接钉在 runtime 线程上 + 真实 `run_single` Confidential current_thread pin，port-1 loopback no_proxy 按 T217 教训）。
  - **审计新发现（P3，owner decision，已入 TODOS）**: 自由函数 `is_local_llm_available(&dyn LlmRouter)` 命中 trait-default `false`——`DefaultRouter` 的探测 inherent 方法不在 trait impl——⇒ orchestrator:1090（T170 review local-hop）与 decision L1 双 rung **从不探测、结构性永远 abstain/escalate-frontier**（本地优先意图在这两条路径上休眠）。接线 override ≈1 方法但会激活休眠 rung = 行为变化 ⇒ owner 决策；runtime-safety pin 已就位防激活时回归。同审计顺带记录: 4 个死云 provider `health_check`（anthropic/mistral/cohere/gemini）同用 reqwest::blocking 零调用者——若经 `ProviderInstance` 暴露需同款守卫。
  - **P3 接线激活（owner 批准，同日闭合）**: `impl LlmRouter for DefaultRouter`（router.rs:1541-1548）新增 `is_local_llm_available` override——纯委托 inherent 探测（:1243，仅可达 ollama + dedicated-thread health_check；4 个死云 health_check 仍休眠不可达）。三个 dyn 消费者（T170 local-hop + L1 双 rung）真实探测。**今天字节等同**：三站点全部在 calibration τ 门后（默认 None）——T169/T170/T171 no-τ 钉测试断言原样全绿；阈值落地（`logs/decision-audit/thresholds.json`）那一刻本地优先 L1 层即生效，零再接线。钉：`zen-provider/tests/local_llm_probe_dyn.rs`（inherent + dyn 双路径对不可达 loopback 均 probe-false）。门禁：bin/lint 0；bin/test **3115/0/25**（基线 3113，+2）。
  - **006 spec.md 收敛**（`docs/specs/006-agentic-orchestration/spec.md`，本地账本）: Status Draft→Implemented；D3 结构性 depth-1 标注被 T373 可配置深度取代；Config Surface 补全（`max_depth`/`max_concurrent`/`context_inheritance`/`FANOUT_MAX_TASKS=8` + `[agentic.orchestrator] surface`）；Post-Implementation landmine 陈述更正（2026-09-30 证伪记录 + health_check 真例外指向 C4）；Non-Goals 前三项标注 REALIZED（PD-01 A/B = T114/T115、T373/T374）；新增 **Convergence & Maintenance Record C1-C5**（001 愿景收敛 T372-T379、PD-01 迁移、Phase 31 委托面加固 T215-T218、D1 修复闭合、D4/D5 质量门演进——gate 范围收窄/blast-radius 单一驱动/去锚定评审/intent 路由契约）。
  - **AGENTS.md 卫生**: 两处 ADR 路径笔误修复（`006-multi-agent-orchestration` → `006-agentic-orchestration`）；false-alarm 记录 bullet 补 health_check 真例外注记。

- **Phase 32 审查修复（T219-T224 全部落地，2026-10-11，`/speckit-implement`）** — Phase 31 `/sdd-review` 的 6 项 oracle 裁决修复全部实施：3 个并行实施 agent（文件集互斥）+ 主会话 diff 抽验 + sole-cargo-owner 独立复跑全量门禁。`bin/lint` 0；`bin/test` **3109 passed / 0 failed / 25 skipped**（基线 3088，+21）。账本: tasks.md Phase 32 closed 注记；设计文档 "Phase 32 implementation record"；TODOS.md 三条目 RESOLVED。
  - **T219 (R1 P1 URGENT) 夜间语料复制已止血**: memvid-indexer worker 切 `index_incremental`——机制自 T208 完整却零生产调用者（FND-9 反转），mtime-ms checksum sidecar `memories/.index-checksums.json`，缺失/不可读 ⇒ 回退全量；按裁决**未用 `dedup(true)`**（会阻断 provenance 回填）；报告数字变诚实的 per-run 新增语义（安静夜=0）。判别性测试: 未变语料两次 worker 运行零新增帧（旧代码 run-2 帧翻倍）+ 3 条 zen-memory 幂等 inline 测试。
  - **T220 (R2 P1) pre_supplied 判据收紧**: 单一事实源判据 `has_substantive_knowledge`（≥1 条 trim 后非空 content）双捕获点共用（execute/execute_stream）——TUI Fast 的 5 条空 content 笔记不再假阳性压制唯一默认 ON 门（D-F knowledge_inject）；skip 逻辑与审计串不动。**记录一个界外观察（未改）**: On-mode `extend`+`truncate(M1_TOP_K=5)` 在表面预供满 5 空窗口且注入仍有实质命中时会挤出注入笔记——T220+T222 后实际不可达（全空现在要求文件不可读，而 orchestrator 自身检索同样丢弃它们），按账本"skip logic unchanged"保持原样。
  - **T221 (R3 P1) enriched 强化饿死已修**: additive sibling `retrieve_with_entity_context_and_cards`（位置对齐；原方法委托、签名不变）+ enriched 路径在同一 fail-closed `memory_strength.enabled` 门下跑同一 `increment_memvid_injection_rewards`——零归因逻辑复制（同 flock/dedup-by-source_path/uri-scoped 词法解析）；测试: 门开写 sidecar 含 dedup 钉（2 卡同源 ⇒ access_count 1）、门关零写入。**`enabled` 翻转前置条件已满足**。
  - **T222 (R2 P2 治本) tier-2 canonical content**: `NotesRepo::search` 查询后按 hit 从 `nm.file_path` canonical 文件填充 snippet（SQL 死表达式 `SUBSTR(nf.content,…)` → `'' as snippet`；MATCH/bm25/行形不变）；`SNIPPET_FILE_MAX_BYTES=256KiB`——先例真实匹配（T093 IDENTITY_FILE_MAX_BYTES / T214 WAKE_UP_BRIEF_MAX_BYTES，orchestrator 对同一批文件用同一 cap ⇒ 数据层/消费层 cap 漂移消除），const 非 config 键（T192）；超帽不读（T214 镜像）；fail-open 缺失/非 UTF8/超帽 ⇒ 空、零日志（zen-repo 无 tracing 依赖）。一处修复四个消费者受益: gateway knowledge/search + TUI Fast + tier2_search + fusion。
  - **T223 (R5 P2) rebuild provenance**: `memory_rebuild` 解析与 worker 同一 `[agentic.memory_strength]` 门（T210 fail-open: config 错误 ⇒ warn+off，rebuild 绝不中止）并翻转 `.with_provenance_metadata`；rebuild 保持 `index_all`——**按设计重追加帧（手动、有界；夜间增量已由 T219 去重）**。E2E 测试驱动真实 `memory/rebuild` RPC（sole_owner harness）: gate-on 帧带 source_path+tier / gate-off 两键皆无（容忍 extractous_metadata，T208 钉形）。
  - **T224 (R4 P2) strength_floor**: `[agentic.memory_strength] strength_floor`（默认 **0.0 字节等同**、clamp 0.0..=1.0、env `ZEN_MEMORY_STRENGTH_FLOOR` 沿用该节缩写命名惯例、非有限 ⇒ warn+default）；双 rank 站点 `score × max(R, floor)`（gateway memory.rs + zen_agent `clamped_card_strength` resolver）；发射的 `strength` 字段保持 **raw R**（floor 是排序政策非衰减声明——诚实报告）；`memory_strength_half_life` → `memory_strength_rank_params() -> Option<(half_life, floor)>`（单次 config 加载）；加法混合记录为已考虑被否（fail-closed 遗忘即意图；CAMeR 2026 同形式；V13-A.3 底值=operator 选择，mem0 0.3 不硬编码）；floor=0 字节等同双站点钉（bit-for-bit to_bits + 直接公式参照）；:421-443 既有钉测试原样绿。**`search_rerank` 翻转前置条件已满足**。
  - **顺带账本卫生**: T199 有 closed 注记但复选框漏翻 [X]——已补（与 TODOS.md 自相矛盾同类失效模式）。

- **Phase 31 `/sdd-review` 六阶段深审（2026-10-11，三轨盲推 × GAN 对抗 × oracle 裁决 × Munger 审计）** — 对象: Phase 31 三个 wave（`def929b`/`e24c70e`/`019861f`, T206-T218）。方法: 全局 SPEC 定位 → 第一性原理问题定义 → 三路独立基线（自推方案 / 行业 mem0·Zep·Letta·ChatGPT-memory / 论文 MemoryBank·CAMeR·GenerativeAgents·FSRS·FadeMem）→ 12 项 Findings 攻防 → oracle 逐项 file:line 亲验裁决 → 覆盖矩阵/兼容审计/用例走查/UX 十原则/Munger 八模型。完整工件 `docs/specs/005-agentic-loop/review-2026-10-11-phase31.md`；裁决入 spec.md "Phase 31 Deep Review Record"；修复账本 tasks.md **Phase 32 (T219-T224)**；TODOS.md 三条目已更新。
  - **R1 (P1 URGENT, 审查新发现——此前无任何记录)**: 夜间 memvid-indexer **每晚全语料重复追加**——worker 调 `index_all`（无 checksum skip；`index_incremental` 机制完整却零生产调用者）且 `PutOptions{dedup:false}`（memvid-core BLAKE3 去重存在但 zen 从未启用）⇒ 自 T195 (`4529f1c`) 修好路径起每晚 re-append 全部 M2/M3/M4 帧 + 每 chunk 新 MemoryCard，逐夜复合（01:00 daemon Full profile）。修复=worker 切 `index_incremental`（T219）；**禁用 dedup(true) 修复**（会阻断 provenance 回填）。竞态场景已审计安全: worker/daemon 同进程单例 `Arc<Mutex<Memvid>>`（memvid.rs:129-141），可交错不可损坏。
  - **R2 (P1) contentless-FTS × pre_supplied 合取**: 此前记录里空 tier-2 content 与 pre_supplied skip 是两条独立事实，**合取从未被记录**——TUI Fast 传 5 条空 content 笔记 ⇒ 新判据 `!is_empty` 假阳性 ⇒ 唯一默认 ON 门（D-F knowledge_inject）在主交互面结构性跳过 + M1 渲染空块。修复: 判据收紧"≥1 条非空 content"（T220, P1）+ 治本 tier-2 读 canonical file（T222, P2）；**FTS external-content 迁移被否决**（最重且无增益）。
  - **R3 (P1) enriched 路径饿死强化——重分类**: wave-3 记录的"已记录限制"经裁决升级为 **premise-break under enable**——session 首个持久化 turn 后 enriched 恒胜 ⇒ T211 增量点 ≈每 session 一生一次 ⇒ 成功判据"排序反映年龄×使用"的 ×使用 半边在启用后结构性死亡。T221=翻转 `memory_strength.enabled` 的**前置条件**（cards 已在 memvid.rs:514-517 手中，additive 返回即可）。
  - **R4 (P2) score×R 无下界**: memory.rs:421-443 测试把"2000 天旧高分输给新鲜垃圾"钉成期望行为；双轨反证（mem0 clamp [0.3,1.5] / GA 加法）。修复= `strength_floor` 配置键默认 **0.0 字节等同**（V13-A.3: 不发明常数，值为 operator 选择）+ 双排序点 clamp（T224, search_rerank 翻转前置）；**加法混合记录为已考虑被否**（fail-closed 遗忘即意图；CAMeR 2026 同形式支持乘法）。
  - **R5 (P2) rebuild 丢 provenance**: `memory_rebuild`（daemon.rs:1025）不翻 `with_provenance_metadata` ⇒ 启用态 rebuild 后 strength 退化 anchor-only + T211 停摆至次夜。T223 同门翻转 ~2 行 + 记录 rebuild 按设计复制帧（手动、有界）。
  - **R6-R8 记录类**: ① **框架修正**——wave-2 "strength consumers" 的诚实表述: gateway `memory/search` rerank 是 **served-but-unconsumed in-repo**（contract-004 公开 RPC 面供外部 UDS 客户端；公式与活路径 structured resolver 真实共享），仓内活排序消费者仅 structured-fallback 重排 ② `loop.knowledge.injected` 无机器聚合器=**record-with-trigger**（trigger=首次延迟/校准调查；今天建聚合器本身会成为无消费者原语）③ 双重强化（anchor 重置+log2 拉伸）**防守完胜**=GA/MemoryBank 正典语义（log2 饱和 n=10⁶ 时 ~21×HL，永不真永生），记录为决策非缺陷。
  - **背书入 Decision Record**: 纯函数强度零存储零迁移（T153 类结构性不可能）· report-only 驱逐**行业领先**（mem0/Zep 无候选清单产出）· gate-closed+byte-identity 纪律（mem0 对等）· 单缝 compose_sub_session（接线审计全覆盖证实）· 衰减=relevance prior 永不代理 correctness（Graphiti #1728 的 41% 连带失效数据为反例警示）· 配置面零孤儿。**文献偏差带 trigger 记录**: 30d HL 长于全部发表默认(5-11d) · depth 减半继承无 2024-2026 先例（最新颖选择，测量计划=inheritance on/off × depth 质量对照）· 规则式注入门=本地优先刻意偏差（Self-RAG/Adaptive-RAG/FLARE 学习式趋势）· 2026 衰减公式论文全部为未复现 preprint（唯一 peer-reviewed 衰减公式=MemoryBank AAAI'24；peer-reviewed 重量在 structure-over-forgetting 一侧——HippoRAG×2/A-MEM 全无衰减）。
  - **逆向思维（Munger）主信号**: "这一波怎样会静默失败"精确命中——注入在主面被跳过(R2) + 强化在优先路径饿死(R3) + 重排无仓内消费者(R6) ⇒ 默认安装中波次的用户可见效果 ≈ 仅 hosted 面注入；测试 3088 全绿与用户可见效果的落差=最大审查信号。机会成本次序教训: **先修数据层缺陷（contentless-FTS），再在其上建特性**。

- **Phase 31 记忆强度 × 知识波（wave 1+2+3 全部落地，2026-10-11）** — `docs/designs/memory-strength-knowledge-wave.md`；owner 决策：D-E zen-native skill 根优先、**D-F 修订 knowledge_inject 默认 ON**（"作为个人 agent 最重要的是知识管理"；唯一非 closed 门，guards=空 knowledge 前置+Conversation 跳过+4000 字符预算+2000ms 超时 fail-open+逐 turn 审计）、D-G 单一 `search_rerank` 门管两个消费者、D-H fan-out 硬上限 8。
  - **Wave 1（T206-T208，`def929b`）** — `[agentic.memory_strength] {enabled=false, half_life_days=30 复用 RECENCY_HALF_LIFE_DAYS clamp 1..=365, search_rerank=false, eviction_report_size=20}`；`zen-memory/src/strength.rs` **纯函数派生强度**（Ebbinghaus `R=0.5^(t/(HL×(1+log2(1+n))))`，读时派生零存储零迁移——T153 复合衰减 bug 类结构性不可能，5-step 推进≡单次求值幂等测试钉住）；indexer additive `source_path`/`tier` provenance 写 extra_metadata + `select_cards_with_strength`。**消费者契约（实测）**：memvid-core 会注入自身 `extractous_metadata` 键——消费者必须以 source_path/tier 存在性判定，绝不以 map 空判。门禁 3041/0/25。
  - **Wave 2（T209/T210/T212/T213，本批）** — 强度消费者 + `.agents/skills` 发现面，全部门 closed + byte-identity 钉：**T209** gateway `memory/search` 按 `score × R` DESC 稳定排序 + additive `strength` 字段（gate-off 字节等同；ranking-only 永不丢弃命中；锚点链 journal-uri 日期 > source_path mtime（tier→根：m2=memory_root，m3/m4=wiki_root）> frame created_at，不可解析 ⇒ R=1.0 中性——未知年龄命中永不被沉底）+ `retrieve_memories_structured` 走 `select_cards_with_strength`（cards 无 source_path ⇒ anchor-only，events=0）。**sidecar-join 诚实结论**：今日生产 writer 以扁平化绝对路径键控 sidecar，而 memvid 帧携带根相对 source_path ⇒ join 现在必然 miss（events=0）直到 T211 以两消费者共用的 `card_id_from_path(source_path)` 派生写入 memvid 侧增量（前向兼容，测试钉住+两处文档化）。**T210** memvid-indexer worker 翻转 `with_provenance_metadata(enabled)`（唯一生产翻转点；config fail-open 保索引存活）+ 启用时写文件式（sidecar+journal 锚点，无 store 扫描）bottom-N `logs/memory-strength-report.json`（write_atomic）+ `loop.memory.strength` 审计线（count>0 才发，flood 纪律）+ `zen discover report` additive `memory_strength` 节（missing/corrupt ⇒ `{"recorded": false}`）。REPORT-ONLY 永不删除（retention 检疫先例）。**T212/T213** SkillLoader 三根多根发现（zen-native > 项目 `.agents/skills` cwd→repo root closest-wins > `~/.agents/skills`；同名 first-wins+warn 双路径；`new_from_dir`/trigram router 不变）+ `export-skill --target zen|agents`（共享派生 `zen_core::paths::{agents_skills_under, user_agents_skills_dir}` 防 loader/export 漂移）。门禁：bin/lint 0；bin/test **3074/0/25**（+33）。
  - **Wave 3（T214-T218+T211，`019861f`，热文件串行 T211 收尾）** — 知识注入 + sub-agent 上下文继承 + 链路加固 + sidecar-join 关闭，仅触 4 个 zen-agents 文件：**T214** `inject_knowledge_context` 在 `execute`/`execute_stream` 双入口 intent ladder 之后、build_agent 之前——guards：pre-supplied-knowledge skip（**turn 入口捕获**，先于 zen 自身 M1 injectors，TUI 面永不二次检索）、Conversation-category skip、`knowledge_tiers` 经 gateway `resolve_tier` 同名契约映射（未知名 ⇒ audited skip 绝不静默改道）、字符边界预算截断、tokio 超时 fail-open、插入时 InputSanitizer+256KiB 帽（wake-up-brief 先例）、M1_TOP_K 截断 ⇒ FR-034 reward 路径自动覆盖；懒加载 OnceLock SearchService+SqliteClient（失败不缓存）。**Contentless-FTS 发现**：`notes_fts` 是 `content=''`（migration 001）⇒ tier-2 snippet 结构性为空——注入改读 canonical markdown 文件；gateway knowledge/search + TUI Fast 模式今天仍供给空 tier-2 content（pre-existing 缺陷，已报告待独立任务）。**T215** `SubAgentContext{knowledge, skill_hits, budget_chars}` + `compose_sub_session` 单缝（run_single ⇒ plan.execute+resume 自动覆盖，replayed checkpoints 保持 verbatim）；`context_inheritance` off ⇒ legacy 空 SessionContext 字节等同（钉住）；on ⇒ `SharedKnowledge` cell（SharedSensitivity 精确模式，`propagate_knowledge` 同点刷新）继承父检索块、无 per-subtask 新检索、预算 `base>>depth` clamp ≥512、skill-hit 走提取共享 `inject_skill_hits_into`（零重复）；reservation 估算 += knowledge_chars/4。**T216** `parse_requests` 拒绝 `tasks.len()>FANOUT_MAX_TASKS=8`（AGENTS.md 记录值复用），结构化 model-readable 错误；plan.execute MAX_PLAN_TASKS=12 不变（层批直达 run_single 不经 parse_requests）。**T217** refund-on-early-error 纯回归钉（consumed 保持 0 + available 完整恢复；loopback-401 非可重试路径级钉）。**T218** resume 审计 additive `replayed_checkpoints[{task, checkpoint_age_secs}]`（读既有 workflow_tasks.updated_at 零迁移；staleness 风险代码注释记录，F3-D3 report-only，TTL 被拒=发明阈值）。**T211** `retrieve_memories_structured`（门=`memory_strength.enabled` 即 provenance 唯一翻转开关 ⇒ 门关可证 no-op）以 `card_id_from_path(source_path)`——wave-2 两消费者钉住的同一派生——写 reward 增量（同 flock 纪律 increment_access）；归属模型：frame-id 等值经公开 API 不可行（实测 put 与 SearchHit id 空间分歧、cards 无 source_offset）⇒ uri-scoped top-1 词法解析 + source_path 存在性判定（m2 精确双射、m3/m4 共享 knowledge-base uri 取字面最佳匹配、miss ⇒ 不增=保守欠计）；按 source_path 去重、绝不 warn-flood。**已记录限制**：检索链优先 enriched，structured（唯一 increment 点，与 T209 读侧对称）是 fallback #2。门禁：bin/lint 0；bin/test **3088/0/25**（+14，主会话独立复跑核实）。
  - **审计线文档**：`loop.memory.strength`（T210，gated `[agentic.memory_strength] enabled`）：`{kind, ts, count, lowest, half_life_days}` — 每晚一行仅当 count>0；`count`=报告条目数（bottom-N by retention strength，N=eviction_report_size 默认 20），`lowest`=items[0].strength（升序）。REPORT-ONLY：无消费者据此删除/驱逐。`loop.knowledge.injected`（T214，gated `[agentic.orchestrator] knowledge_inject` 默认 on）：`{kind, ts, mode(on|shadow), ms, notes, chars, skipped_reason?}` — 每个 gated turn 一行 BY DESIGN（唯一默认 ON 门的实时 latency/token 可观测性），skip 也发；`skipped_reason` ∈ knowledge_pre_supplied|conversation_intent|unknown_tier|timeout|retrieval_failed；shadow=检索+审计绝不注入；off ⇒ 零审计行（字节等同钉住）。`loop.delegate.gates` per-task 条目 additive `{inherited_notes, inherited_chars, skill_hits}`（T215；无条件形状——gate 关/被拒槽位为 0，旧字段不变）。`loop.plan.completed` additive `replayed_checkpoints[{task, checkpoint_age_secs}]`（T218；仅 resumed 且有 replay 时出现）。
  - **偏差记录**：① `~/.agents/skills` 派生放 zen-core paths.rs（T212/T213 单一事实源）；② T210 报告 shape/reader 住 zen-agents（writer 所在 crate，避免跨 crate 复制 JSON shape）；③ 碰撞 warn 文本未做断言（树内无 tracing-capture 依赖，新增违反 no-new-deps）——first-wins 行为已钉；④（wave 3）`loop.knowledge.injected` 增 `mode` 字段超出任务枚举——否则 shadow 与 on 行不可区分，shadow 测量目的落空（additive）；⑤ pre_supplied 在 turn 入口捕获而非注入点判空——zen 自身 injectors（wake-up brief/skill hits）会掩盖 TUI 前置条件；⑥ 笔记上限=常量 3（设计 F2-D7 记录值）非 config 键——T206 记录的推迟 + T192 无休眠键；⑦ T211 归属经 uri-scoped 词法解析（frame-id 等值经公开 API 实证不可行），m3/m4 miss ⇒ 保守欠计，函数 docstring 记录；⑧ T217 用 loopback-401 + NO_PROXY 构造确定性非可重试失败（本机代理会劫持 loopback，实测 Privoxy 500 使错误可重试）。

- **Workspace-path 兼容修复批 + gateway 升级管理设计 + 特性波设计（2026-10-10，/speckit-analyze 四维审计的落地波）** — 四维审计（spec 实现状态 × RSI × 知识加工链路 × workspace path 兼容性）+ pi/codex 对标研究。spec 状态结论：186/186 tasks、FR 40/40（含 archive）、SC 14/15、0 CRITICAL；剩余全部为外部依赖（标签供给/生产流量/vendor access）。
  - **T195 (W1, HIGH) MemvidIndexer 重扎根 Path Spec v2（已修）** — indexer 仍按 v2 之前的布局扫描 `{workspace_root}/memories/journal` + `{workspace_root}/wiki/...`，而真实位置是 `~/.zen/memories/journal` + `~/.zen/vault/wiki/...`：**不存在任何 workspace_root 取值能让两组 join 同时命中**（`find_workspace_root` 结构上不返回 global_root）⇒ 夜间 memvid-indexer 生产上是静默 no-op（None-skip 或空目录），M2/M3/M4 从未进入 memvid；`memory/rebuild` RPC 半边空转；doctor 保持绿色；测试全绿只因自建旧布局。修复：`MemvidIndexer::new(memory_root, wiki_root)` 显式双根；worker None-skip **删除**（fail-loud）；RPC 去掉 workspace_root 前置；**M3 递归遍历**（walkdir 既有依赖——生产 notion 页在 `notions/technology/` 下，`wiki_compile.rs:314`，平铺 read_dir 即使根正确也会漏掉 M3；checksum 集与遍历集镜像）；判别性回归测试 `index_all_reads_path_spec_v2_layout`（真实 v2 布局 × 每 tier 恰一文件 ⇒ 3/3 总数即逐 tier 判别，旧代码只得 1）。
  - **T196/T197/T199（已修）** — `build_env_info` 标签撒谎（`Git branch:` 渲染 session_id、`Model:` 渲染 agent_name → 改 `Session:`/`Agent:`）；`loop_wisdom_test.rs:416` 过期注释（commit_cycle_to_git 早已 vault-based）改正；`workspace_id` 溯源标签随 daemon cwd 漂移 → 消费者审计证实**全 workspace 零读者**（graph_router 三个 stamp 点纯写入）⇒ 不透明标签 ⇒ None 时确定性回落 `"global"`，测试钉住。
  - **T198/T200（记录）** — `ZEN_WORKSPACE` env override（接受任意存在目录）补进本文件 Path scoping；daemon workspace 契约记录：launchd daemon（cwd=/）workspace_root=None 是**正常 hosted 形态**——全局树全功能，hosted turn 不支持项目 workspace 写入（sandbox 报错已含补救指引）；identity AGENTS.md 的 workspace 回退在 launchd 下永不可见 = 已接受的表面不对称。
  - **Phase 30 gateway 升级管理（已实施，T201-T205）** — 一手研究 `codex-rs/app-server-daemon`（11 项机制全表）+ pi 无 daemon 教训；zen 已有握手版本协商（`negotiate_version` MAJOR 相等 + client minor ≤ server minor，`-32001{recovery}`）但**线上版本信息零消费者**。落地：G1 pid 记录 additive `version`/`protocol`（legacy 形状容错，never parse-error；顺带修复 pre-existing bug——bare-pid 回退分支因 JSON number 误入 object 分支而不可达）+ `zen serve status [--json]` Binary vs Daemon 双版本 + STALE/STALE-unknown 标记；G2 `zen serve restart`（launchd 在装 ⇒ `launchctl kickstart -k gui/{uid}/dev.zen.serve` + readiness poll（复用 READY_BUDGET）；否则优雅 stop（既有 SIGTERM drain）+ StartupLock 下 start；Start/Stop 体**提取复用非复制**；stopped daemon ⇒ restart=plain start）；G3 `[gateway] upgrade_policy=warn|auto-restart`（owner 决策 D-A：默认 **warn**，gate-closed；非法值 ⇒ warn+tracing::warn 绝不静默）三入口诊断（status STALE + policy action 仅 human 模式、doctor 第 9 探针 daemon-version 仅诊断、TUI prewarm health/status 对比 + -32001 ⇒ banner 提示一次 + auto-restart ⇒ 一次 restart+reconnect 绝无循环；隐式 daemon 结构上不触发）；G4 迁移竞态审计 **PASS**（zen-repo 零 `SELECT *`；唯一位置映射 notions_repo.rs:1050-1052 读固定显式投影——additive 列不可移位；结构性注意：容忍度依赖 Principle XIII 的 NOT NULL DEFAULT 纪律）；G5 契约 additive 补遗（contracts/00 §Addendum：pid 形状 + 版本消费约定，无 RPC/错误目录变更）+ spec Configuration Surface `[gateway]` 行 + 升级 runbook（设计记录 §5）。G6（codex 式独立 updater + 包解耦）**owner 决策推迟**（brew/cargo 单渠道；revisit trigger=分发渠道变化或 remote-control 需求）。测试 +23（config 5 / pid_record 4 / serve 4 / doctor 5 / prewarm 5）。设计记录 `docs/designs/gateway-upgrade-management.md`。
  - **Phase 31 特性波（设计完成，5 个 owner 决策待答）** — `docs/designs/memory-strength-knowledge-wave.md`：① 记忆遗忘曲线 = **纯函数派生强度**（不存储、零迁移——Ebbinghaus `R=0.5^(t/(HL×(1+log2(1+n))))`，HL 复用既有 `RECENCY_HALF_LIFE_DAYS=30`，n=access_count+citations 来自既有 MemoryReward sidecar，anchor=max(内容日期,last_reward_at)，belief.rs 锚点纪律；T153 复合衰减 bug 类被结构性排除——无累计状态）；indexer additive 写 `source_path` extra_metadata（memvid-core PutOptions 支持，SearchHitMetadata 回读）；消费者=ranking-only（gateway memory/search rerank + select_cards，config 门默认关）+ 夜间 report-only 低强度清单（retention 检疫先例，永不删除）。② `.agents/skills` 标准发现面（codex+pi 双双收敛的中立面）：SkillLoader 多根（zen-native > 项目 > 用户，first-wins+warn）、`export-skill --target agents`。③ orchestrator 知识注入：`[agentic.orchestrator] knowledge_inject` 门（默认关，T138 纪律），仅对 knowledge 为空的非 TUI 面（gateway/qqbot 今天拿不到任何 KB 上下文），Conversation 类跳过，FTS tier + 4000 字符预算 + 超时 fail-open。④ sub-agent 上下文继承：`compose_sub_session` 单缝（run_single ⇒ plan.execute/resume 自动覆盖），`[agentic.delegate] context_inheritance` 门，深度减半预算，loop.delegate.gates additive 遥测。⑤ 链路加固：fan-out **硬上限 8**（记录值复用，今天代码无上限=spec 漂移）、reservation refund-on-early-error 回归钉、resume 审计 `checkpoint_age_secs`。
  - 门禁：`bin/lint` exit 0；`bin/test` **2986 passed / 0 failed / 25 skipped**（基线 2984，+2 = v2 布局回归 + workspace_id 回落）。
  - **基础设施备注（诚实记录）**：本日 explore/librarian/plan 三类 subagent 全部因模型下线失败（mimo-v2.5-free deprecated / muse-spark-1.3-contributor-free 缺失）；`general` 通道正常——修复批（28min）与特性设计（15min）均由 general 代理完成并经主会话 diff 抽查 + 独立测试复跑核验。

- **eng review 第二轮：深读此前仅模式扫描的文件（2026-10-02）** — 承接上一条记录中"约 40 个文件未逐行深读"的诚实缺口，对风险最高的 5 个 seam 做逐行深读。**1 个新缺陷（记账）+1 处假陈述被改正；4 处经审计确认无误（明确记录以免重复怀疑）**。
  - **P1 distill 预算补账少报真实 token（已修）** — `llm_distill.rs` 预算预留 `estimate` 后，若 provider 实际用量更大则以 `delta = usage - estimate` 补账，但旧代码把 `note_tokens = usage_tokens` 放在 `if consume_tokens(delta)` **成功分支内**。而 `LoopBudget::consume_tokens`（`types.rs:432`）在超限时 `return false` **且不修改累加器**——于是该 section 真实消耗的 `delta` 从 `consumed` 中蒸发，并经 `pipeline.rs:1073` 的 `b.consume_tokens(tokens_used)` 把少报的值折回周期预算。**与本批 P1 #1 同源的记账不对称**：同一函数里 `charge_cost` 是**无条件**累加（记账=问责，非闸门），token 补账却有条件。修复：`let _ = consume_tokens(delta)` 后**无条件**上报真实用量；预算闸门仍由下一 section 的 `consume_tokens(estimate)` 把关。补 2 条测试：`budget_refusal_discards_rather_than_clamps` 钉住"拒绝即丢弃、绝不 clamp"这一**跨模块前提**（若 `consume_tokens` 改为 clamp，被丢弃的调用将反过来改动预算）、`cost_is_charged_even_when_the_budget_refuses` 钉住两个 sink 的"无条件"对称。
  - **假陈述：`replay_stale_transactions` 的并发安全依据（已改正）** — `pipeline.rs:597` 原注释称"1h age guard keeps a concurrently-running cycle's tracking file safe"。**该保证不成立**：distill 周期**没有整体超时**（`zen_loop.rs` 唯一一处 `tokio::time::timeout` 是 per-source host staging），`max_steps: 10` × 树索引多 section × 每次 LLM 600s 首token预算 ⇒ 合法周期可超过 3600s。真正防止活周期被回滚的是**调度器 lease**（每 ZEN_HOME 单周期）+ 本进程内"先 replay 后 `txn.begin()`"的次序。注释改为如实陈述：age guard 是**崩溃进程的后备**，不是并发闸门。**与 cascade 模块文档同类**：以"看起来有保护"掩盖实际缺失的保护。
  - **经审计确认无误（不再重复怀疑）** — ① **树索引预算耗尽用 `return` 而非 `break`**（`llm_distill.rs:166`）：中途放弃整阶段，调用方不会误以为该笔记已被完整蒸馏；原审查列为待查项，现证实正确。② **扩展名谓词结构上不可能漂移**：`is_convertible_extension` = `pdf || is_office || is_sidecar`，两个子谓词**同时**喂给谓词与 dispatcher（`convert.rs:213`/`220`），新增扩展名自动传播至两处；`source_ingest.rs:155` 的 staging 过滤确实复用该谓词，`md`/`txt` 是**平铺层**（不需转换的互补类别）而非平行清单——上一批 P1 的结构性修复成立。③ **`normalize_aliases_pass`**：干净表真零写入（`repairs.is_empty()` 提前返回）、幂等、`INSERT OR IGNORE` 无 PK panic；跨实体碰撞会使别名静默改指另一 nottion，但这是**已记录的设计意图**（PK 决定无法并存两者），且未包裹事务只带来可自愈的部分应用（幂等）。④ **`apply_tui_env`（`config.rs:2792`）**：`to_lowercase()` 大小写不敏感；`_ =>` 分支只 `warn!` **不赋值**，故拼写错误保留 TOML 值而非静默回落；env 正确覆盖 TOML。
  - 门禁：`bin/lint` exit 0；`bin/test` **2984 passed / 0 failed / 25 skipped**（上一轮 2982，+2 = 本轮新增 2 条记账测试）。
  - **环境说明（诚实记录）**：本轮多次遇磁盘耗尽导致 `cargo` 报 `failed to open object file` / `No space left on device`——该失败**与代码无关**，系外部进程清理 `target/` 所致（项目内无任何 `cargo clean`：已核查 `bin/test` 无 clean、无 `CARGO_TARGET_DIR`、无 `.cargo/config.toml`）。最终以单次 `cargo nextest run` 完成构建+执行以缩短窗口并取得全绿。

- **分支全量 eng review 修复（2026-10-02，`/plan-eng-review` on `005-agentic-loop`）** — 对 36 commits / 70 files（+7581/−239）做整分支审计，2 个 P1 + 7 个 P2/P3 全部修复。**根因优先：两个 P1 都是"类型/接口无法表达真实语义"而非笔误**。
  - **P1 cached-path 计费双减不相交桶（已修）** — `llm_distill.rs` 旧式 `input + creation − read` 与 Anthropic 规范冲突：*"Total input tokens … are calculated by **summing** `input_tokens`, `cache_creation_input_tokens`, and `cache_read_input_tokens`"* —— 三者**互不相交**，`input_tokens` 只是**未命中缓存的余量**，故 `read` 从未被包含在内，减它属重复计数。测试夹具（in=1000/create=200/read=800）旧代码算 **400**、真值 **2000**；`saturating_sub` 更使高缓存命中笔记归零。**且该错误测试把它钉成"正确"**（`assert_eq!(…, 450)`）。修复走**让模型能表达真实计价**而非削弱：`model_meta.rs` 新增 `CachedTokenUsage`（四桶显式类型）+ `cached_usage_to_cost_usd`（uncached 1x / write **1.25x** / read **0.1x** / output）。`ModelMetadata` 原先只有单一 `input_cost_per_million`，**结构上无法**表达 Anthropic 的 cache 倍率——这才是根因。**tokens 与 cost 语义分离**：tokens 是吞吐口径（四桶全计，read 虽折价仍占 context 与算力），cost 是金额口径（按桶加权）。污染面为**双 sink**：`consume_tokens`（token ceiling）与 `charge_cost`（`[cron] llm_cost_cap_usd`），故 `093e68a`"让花费触达成本上限"的目标在缓存路径上此前**从未真正生效**。doctest 反过来抓出我自己在注释里写错的期望值（0.0065 → 0.00632）。
  - **P1 orphan 删除不可回滚 + 存活判据过弱（已修）** — `cascade.rs` 的 `fs::remove_file` **未纳入 txn**：`TransactionScope::rollback` 的语义是**删除** tracked path（无备份），故"track 一个已删文件"根本不可能恢复它——原模块文档"All mutations go through the cycle's TransactionScope so CAS rollback and crash replay cover them"对删除而言是**假陈述**（已改正）。更危险的是 liveness 判据为 `Path::new(source).exists()`，而 `sources` 是**绝对 vault 路径**：vault 瞬时不可达（未挂载 / `ZEN_HOME` 迁移 / 从备份恢复）会让**每个** source 读作 dead，`alive.is_empty()` 对所有机器页成立 → **批量删除整个已编译 wiki**。修复（两道独立护栏）：①先把原始字节 capture 进 `PageIterations`，使删除可经 `zen wiki rollback` 恢复；②**两轮扫描宽限**——首次只 capture、下一轮才删（capture 本身即"已见 dead"记录，零新增状态文件）。推论：vault 中途恢复时 capture 惰性失效，永不删除；`orphans_pending` 计数区分"待删"与"已删"。`None`（无 store）显式回落为旧行为而非永不删除——该反直觉的 `.unwrap_or(true)` 有注释锁定，因为反向"修正"会静默禁用全部删除。
  - **P2 `complete_cached` 无超时（已修）** — `reqwest::Client::new()` **无默认超时**，是全仓唯一无预算的 LLM 路径（ollama 120s / zen-agents 600s 首token + 120s token 间）。新增 `CACHED_COMPLETE_TIMEOUT = 120s`。
  - **P2 DRY：6 provider × 2 方法（已修）** — 12 份近乎逐字的 usage-bearing 实现。抽出 `used_completion`（文本+usage 提取，防止各 provider 规则漂移）与 `blocking_usage_call`（专用线程 + 自有 runtime；记录 nested-runtime panic 隐患，否则读者会"简化"回内联 `block_on`）。provider 差异从 +639 行降至**净 −105 行**（53 增 / 158 删）。
  - **P3 `Unavailable` 重试告警无界（已修）** — 转换器缺失是"待人工修复"的环境条件，按周期（约 5min）重复告警 ≈ 288 条/天且永不停止。改为每文件每进程一次，并明写"silence here is not a drop"（限流后的静默在重试路径上会被误读为丢数据）。
  - **P3 `PromptCacheControl` 未被真正使用（已修）** — 唯一 consumer 硬编码 `provider == Provider::Anthropic`；改为查询 `supports_prompt_cache()`，第二个支持缓存的 provider 无需再改此处。
  - **P3 TODOS.md 正文与 `Depends on` 自相矛盾（已修）** — 3 条 `## Review` 项正文仍描述修复前状态（"outbox is currently write-only"、"Current `decide()` scans for first `!claimed`"）而 `Depends on` 已标 IMPLEMENTED。按本文件既有 `— RESOLVED <date>` 约定改正标题与正文，并给出证明该修复的代码位置。**与刚关闭的 AGENTS.md 腐化同一失效模式，且仍无任何门禁覆盖。**
  - **P3 单侧计价配置静默（已修）** — 告警条件由"两侧皆 0"放宽为"任一侧为 0"，并**指名缺失的 key**。
  - **P3 `CACHE_BREAKPOINTS_PER_PROMPT` 只被测试断言（已修）** — 该常量在 prod 从不校验；`CacheSegments` 无 `Doc` 块时动态标记数会变成 2。改为在 `build_cached_request_body` 内 `debug_assert_eq!`。
  - 门禁：`bin/lint` exit 0（fmt + clippy `--all-targets --all-features -D warnings`）；`bin/test` **2982 passed / 0 failed / 25 skipped**（基线 2975，+7 = cascade 3 + llm_distill 4）。均以 `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` 运行（磁盘余量不足）。
  - **未覆盖（诚实记录）**：36 commits 中约 40 个文件仅做模式扫描（panic/unwrap / 破坏性操作 / 超时 / 用量算术），未逐行深读——本次会话 5 个并行审计 agent 全部失败（2 个 0s 空返回、1 个 abort、2 个 31min 超时），并行度丧失使"全量逐行"不可达。

- **审计遗留两项收尾 + 门禁盲区关闭（2026-10-02）** — 上一批（2026-10-01）判定"未修"的两项，在此关闭；更关键的是修掉**让它们得以腐化的结构原因**（AGENTS.md 不在任何 rot 门禁内，而 `c6067d9` 门禁只读 `docs/src/cli-commands.md`——盲区在同一失效模式上原样复发）：
  - **AGENTS.md 命令面（已修）** — 3 处"20 commands"→23（crate 表 / STRUCTURE 树 / COMMANDS 标题），命令表补 `zen sandbox` 行。
- **rot 门禁扩展到 AGENTS.md（`crates/zen-cli/src/cli.rs` `docs_consistency`）** — 两个新测试，覆盖面**限定在 `## COMMANDS (CLI)` section 内**（全文扫描会被反引号散文提及满足，删表格行看不见——本批实测）；计数断言**限定在 `## Recent Changes` 之前**（历史条目合法引用已退役计数，如 T193 的"29 commands"），且扫描前剥标点（三处真实声明有两处是括号式 `(23 commands`）。
  - **门禁鉴别力已实证** — 4 次扰动分别触发 3 个计数位点与表格行缺失，全部正确 FAILED。第一版实现**只钉住 3 处中的 1 处**：token `"(23"` 解析失败被静默跳过，测试"绿"但欠鉴别——这是"测试通过 ≠ 有效"的又一例，与同批 OKF 验收断言同一纪律。
  - **E1 writer 清单补第 20 个条目（`docs/designs/e1-semantic-lint-writers.md`）** — `zen wiki rollback` 的 `PageIterations::restore`（`wiki/iterations.rs:156` `write_atomic`，E5 `e8ef9e2` 晚于清单编制新增）。不改变 E1 只读结论（`semantic_lint.rs` 生产代码零写入；restore 是用户手动 CLI、先 capture 再还原、自身可逆，不构成并发 writer 冲突）。
  - **顺带更正一个从未成立的数字** — `openkb-compile-hygiene.md` 的"13 个 wiki writer"与清单实际条目数不符（一直是 20 个条目 = 19 内容写入者 + 1 仅删除的 retention）；上一批条目按当时的文档原样引用了"13"，更正落在设计文档与本条目，历史条目不改写。

- **设计验证审计 + 两个 P1 修复（2026-10-01）** — 对 4 份设计记录（`docs/designs/*`）+ TODOS.md 全部未决项做了"存在 ≠ 落地"的活跃路径验证（每项均验证调用链、配置门默认值、可观测验收），25 项确认落地，抓出 3 个缺陷，修复其中 2 个 P1：
  - **P1 pandoc sidecar 层生产不可达（已修）** — `stage_host_dir` 用 `is_convertible_extension` 暂存（含 epub/odt/rtf），而 `promote_incoming` 只接受 `md|txt|pdf|office`：sidecar 文件被暂存后**永不推进、永不隔离、永不重暂存**（暂存树兼 seen-set）→ 永久孤儿；唯一能转换它的 `ingest_local_file` 自 2026-08-28 CLI 收缩起零生产调用者（`ingest_command.rs` 未编译），故 T4 层（`172f2bb`）实现完整却**无任何活跃路径可达**。修复用**根因方式**：`promote_incoming` 改为复用 `is_convertible_extension` 这一唯一谓词（"dispatcher 有转换器吗"），两个列表各自维护正是漂移根因，现结构上不可能再漂移。`ConvertError` 语义逐字保留：`Unavailable`（pandoc 未装）**留暂存待重试、永不隔离**；`Failed` 终局隔离。测试钉在开关两侧（本机 pandoc 已装 → 走真实转换提升的强断言；未装 → 走 Unavailable 契约）。
  - **P1 OKF lint 前提半假、验收永不可达（已修）** — `OKF_REQUIRED_KEYS = ["type","description"]`，但 `render_page` 的 `description` 输出 **0 次**、`type` 仅当源笔记带 `type:` 才输出 → **每个机器编译页被永久标记**，设计记录的验收"缺失数=可解释为零"永不可达。修复方向选择**让编译器合规而非削弱 lint**（`OKF_REQUIRED_KEYS` 原样保留）：`Note`/`WikiPage` 增 `#[serde(default)] description`（加法式）；`parse_frontmatter` 解析 `description:`；`note_to_page` 补齐——`type` 回退到**笔记自身分类**（`NoteCategory`，即决定输出目录的同一词表，不自造），`description` 优先取笔记 frontmatter、否则由 `first_substantive_line` 从正文派生（跳过空行/标题/围栏/引用/表格/图片，折叠空白，复用实体渲染路径既有的 `truncate_for_description` 100 字符界限+引号转义）；`render_page` 非空时输出。已验证 merge 路径走 `read_to_string`+追加、**不经 `render_page`**，故 frontmatter 不被抹除。测试含一条直接打在 `Linter::run_with_okf` 真实扫描上的验收断言，并实证其鉴别力（旧代码下失败并精确复现 `missing: ["type","description"]`）。
  - **未修（已记录）** — ① AGENTS.md 命令面腐化：3 处声称"20 commands"、实际枚举 23 个变体、命令表缺 `zen sandbox` 行；`docs/src/cli-commands.md` 正确且被 `c6067d9` 门禁覆盖，但 **AGENTS.md 自身不在任何门禁内**（1A 门禁的盲区）。② E1 的"13 个 wiki writer"清单少 1 个：`zen wiki rollback` 的 restore（`iterations.rs:156`）是清单编制后新增的第 14 个 wiki 页写入者；不影响 E1 只读前提（`semantic_lint.rs` 生产代码零写入），且它是用户手动 CLI 恢复（先 capture 再还原、自身可逆），不构成并发 writer 冲突。
  - 门禁：`bin/lint` exit 0（fmt + clippy `-D warnings`）；`bin/test` **2973 passed / 0 failed / 25 skipped**（+5：sidecar promote 1 + OKF 4）；zen-vault 645/645。分支 `005-agentic-loop`。


- **Provider usage plumbing + T138 decision memo (2026-09-30, owner-picked follow-ups to the plan-eng-review batch)** — `3aa4eb0` + `cfe3e6a` + option A:
  - **Real usage on the sync completion surface (`3aa4eb0`)** — rig-core 0.42 already normalizes per-provider usage into `CompletionResponse.usage`; the 6 providers were discarding it inside `agent.prompt()`. Additive surface, estimate semantics preserved: `SyncUsage`/`UsedCompletion`/`reconcile_usage` (a Some echo with non-zero total wins; `None` — Mock — and all-zero echoes — rig's failed-to-report marker — fall back to bytes/4), per-provider `complete_async_with_usage` (rig `completion_request` builder path — same client construction, Anthropic's 4096 max-token default, Ollama's loopback-no-proxy + 120s thread timeout) + `complete_with_usage` (same dedicated-thread nesting-panic guard), `LlmRouter::call_with_usage` with a delegating default (`usage: None`) so all implementors compile, `DefaultRouter` override; `complete_metered` + llm_distill plain path reconcile (real echo bills directly, reservation-split only on silence; `complete_cached` untouched); `completion_model.rs` feeds the echo into rig `Usage` so AgentRun `run.usage()` / orchestrator `tokens_used` / T096 budgets are real on the non-stream path (stream keeps len/4, documented). deepseek/aliyun/groq/perplexity/moonshot ride the openai/anthropic impls. Deliberately still estimates: streaming surface, `executor.rs` inline, `PromptHookTelemetry::record`. Gate: clippy 0 (3 crates); nextest zen-provider 58 (+4) / zen-vault 640 / zen-agents 669 — worker-cost Mock tests pin estimate costs unchanged.
  - **T138 decision memo (`docs/designs/search-mode-defaults.md`, `cfe3e6a`)** — premise corrected by code audit: `KnowledgeSearchMode` gates exactly ONE call site (TUI chat context injection, default Fast → `tiers=["fts"]`); `zen chat` + TUI `/search` pass `tiers=None` unconditionally and DO reach RRF+PPR fusion for multi-word queries; agent tools bypass `SearchService` entirely (never fuse); HTTP/qqbot carriers do no KB search. Options: A) add the missing `ZEN_TUI_KNOWLEDGE_SEARCH` env var, B) flip TUI default to Full (per-turn latency trade), C) fused `search_knowledge` agent tool (the real agent-side gap), D) status quo.
  - **T138 option A adopted (2026-09-30, owner pick)** — `apply_tui_env` + `ZEN_TUI_KNOWLEDGE_SEARCH` (fast|full|off, case-insensitive, wired into `apply_env_overrides`): the last 5-layer config key without an env surface now has one; unparsable values `tracing::warn!` and keep the TOML-derived mode (cron-timezone discipline — a typo must never silently shift behavior); `KnowledgeSearchMode` doc + `config/config.toml` `[tui]` section annotated (commented key, defaults preserved — T192). Default stays Fast. Tests: env override applies (case-insensitive) + invalid value falls back. Open follow-ups recorded in the memo: C (fused agent tool) and B (behind a measured latency budget).

- **Plan-eng-review remediation batch (2026-09-30, TODOS.md open-item sweep, 3 decisions adversarially verified via Codex + 3 explores)** — `c6067d9` + `b344472` + `093e68a`:
  - **Docs-rot gate (1A, `c6067d9`)** — `cli.rs` `#[cfg(test)] docs_consistency`: every clap subcommand must appear in `docs/src/cli-commands.md` as the code-form prefix `` `zen <name> `` (opening-backtick anchored: matches nested rows like `zen sandbox test` for `sandbox`, prose without code ticks cannot false-positive) + the header "N subcommands" count claim must equal the live variant count; `zen-core/tests/config_documented_keys_test.rs`: sentinel round-trip for the highest-traffic documented keys (no `deny_unknown_fields` means a phantom key parses silently — the sentinel asserts the value LANDED; covers default_provider, providers.<name> incl. renamed `type`/`env_key` + pricing pair, agents.<task> routing, cron.llm_cost_cap_usd, channels.qqbot drain interval, agentic.loop reverify window). Runs under rust.yml's existing `bin/test` — docs.yml only triggers on `docs/**` so it can never see the crate changes that cause drift. (Closes TODOS.md "User-guide consistency check in CI".)
  - **NFC alias backfill (2A, `b344472`, T143 closure)** — `NotionsRepo::normalize_aliases_pass`: idempotent one-shot pass healing pre-2026-09-19 decomposed (NFD) `notion_aliases` rows that `resolve_alias` could never match (query-side normalization + stored-column equality); collision with an existing canonical row deletes-and-keeps-canonical (`INSERT OR IGNORE`, never a PK panic); read-only no-op on a clean table. Mounted at the top of `zen wiki reindex` (both branches; prints only when >0). Adversarial correction: "aliases self-correct on re-extraction" was FALSE — re-extraction inserts a fresh NFC entity while the NFD row orphans. Tests: `crates/zen-repo/tests/normalize_aliases_test.rs` (no-op/idempotency, NFD→NFC rewrite asserting the healed row answers a lookup, collision dedup).
  - **Distill cost visibility (3A, `093e68a`)** — zen_loop hardcoded `llm_cost_usd: 0.0` while the T046a distill stage + merge assist make real LLM calls (invisible to `[cron] llm_cost_cap_usd`, worst when `distill_allow_cloud=true`). Same fold-back as tokens: `LoopBudget.consumed_cost_usd` + `charge_cost` (recorded unconditionally — cost is accountability, not a throughput gate); `call_llm` returns `(reply, tokens, cost_usd)` billing via `usage_to_cost_usd` on the routed provider's `model_metadata` (mirrors `complete_metered`); cached path bills cache_creation as input and not cache reads, plain path splits the reservation input/reply-budget, unparseable echo falls back on the token estimate; `merge_section` returns `Some((body, tokens, cost))` (merge TOKENS stay unbudgeted as before — only USD is new); `LoopCycleReport.llm_cost_usd` (serde-default additive) feeds `worker_report`, and partial spend before a cycle failure still bills. Provider-usage precise plumbing (bytes/4 → real echoes on `complete_metered`) recorded in TODOS.md as deferred P3 with trigger (real metered cloud volume + cap misjudgment).
  - **TODOS.md hygiene (same batch)** — single-ledger TODO CLOSED as moot (`docs/specs/` deleted from disk; trigger dissolved); ⑥ PDF scope note RESOLVED (landed 2026-09-29); docs-sync pointers updated to code locations.
  - Gate: clippy 0 (all 5 crates), fmt clean; nextest zen-vault 640 + zen-agents 669 + zen-repo/core/cli 839 (+3 CLI docs tests, +1 config sentinel test, +3 NFC tests, +1 charge_cost test).

- **All-format doc → markdown ingestion, Tiers T2/T3/T4 (2026-09-29, PR-1 `1e8d73c` + PR-2 `c5d8123` + PR-3 `172f2bb`)** — PDF and Office files now enter the distill pipeline as markdown; design record at `docs/designs/doc-conversion.md` (three-tier progressive integration, Constitution XI audited):
  - **PR-1 PDF** — `zen-vault/src/ingest/convert.rs::pdf_to_markdown`: in-process `pdf-extract` pinned to the SAME 0.10 minor memvid-core already pulls (cargo-tree-verified: zero new version islands); `%PDF` header gate + `catch_unwind` containment (pdf-extract's known panic-on-malformed-input history, upstream #133/#154/#160) so a hostile document is a normal Err, never a worker crash.
  - **PR-2 Office** — `office_to_markdown` (docx/xlsx/pptx via `office_oxide`; 2026-09-29 mini-audit GO on 719k downloads/600k recent/15 reverse deps/fuzz+institutional CI; undoc 20k + anytomd 9k NO-GO on adoption). Legacy binary .doc/.xls/.ppt explicitly rejected with UnsupportedFormat.
  - **Promote-time conversion** (the `.txt` precedent): `promote_incoming` accepts txt/pdf/office → frontmatter-wrapped `.md` (ledger under the ORIGINAL name); terminal failures (garbled, panic-contained, zero extractable text = scanned) QUARANTINE — seen-set marker + staged removal — so a broken file is never re-read/re-warned every cycle; host originals untouched. `stage_host_dir` sweep filter `md|txt` → `md|txt|pdf|office` (raw bytes staged; conversion at promote).
  - **Plain-text path byte-identical**: non-conversion formats keep the original non-UTF-8 error semantics (`String::from_utf8`, not lossy).
  - Tests: generated minimal-PDF fixture (correct xref offsets assembled in-test, no binary blob) + OOXML round-trip via office_oxide's own `create_from_markdown_to_writer`; the docx promote test caught a real pre-commit bug (outer branch forgot `is_office`, raw bytes were renamed under a `.md` name).
  - **PR-3 pandoc sidecar (T4)** — `pandoc_to_markdown` (epub/odt/rtf, `--from={fmt} --to=gfm --wrap=none`, rga adapter shape; structured argv, 30s deadline poll with thread-drained pipes so a pathological document can neither stall a cycle nor deadlock) + unified `convert_to_markdown`/`is_convertible_extension` dispatcher. **`ConvertError` taxonomy drives promote semantics**: `Unavailable` (pandoc not installed — environmental) leaves the file STAGED for retry and never quarantines; `Failed` (garbled/timeout/non-zero exit) is terminal and quarantines. pandoc's presence on `$PATH` IS the feature switch (no config key, T192); its roundtrip test auto-skips to the Unavailable contract on boxes without it.
  - Gate: **`bin/test` 2951 passed / 0 failed / 25 skipped** · clippy `--all-targets` 0 warnings · fmt clean.

- **T046 LLM distill landed (2026-09-29, compile-hygiene ③-reader + ⑥ consumer)** — the FR-003 `LlmDistillStage` stub became a real bounded enrichment call, closing the two recorded deferrals in one workstream (two PRs):
  - **PR-T046a (1e5d717)** — `zen-vault/src/distill/stages/llm_distill.rs`: enrichment-only producer (the pipeline keeps its deterministic heuristic notions; LLM output merges dedup-by-name on top — any failure is byte-identical to pre-T046). Per-note bounded extraction routed via `route_with_preferences` at `LlmPreference::Provider(merge_llm_model)`; token accounting = estimate reserved before each call, cached-path real usage reconciles upward, over-budget short-circuits remaining notes. **`[agentic.cache] breakpoints` consumed (③ closure)**: routing landing on Anthropic + switch on (default true) goes through `AnthropicProvider::complete_cached` with `CacheSegments::build(system, task)` and bills the real usage echo (output + cache_creation + input − cache_read; all-None → estimate). **`[agentic.loop] distill_allow_cloud`** (env `ZEN_LOOP_DISTILL_ALLOW_CLOUD`, default false): the T045 encoding — naming a cloud provider in `merge_llm_model` alone never moves note content; the explicit flag switches the route request Private→Public. **Merge assist**: `execute_merge_plans` (now async) composes the merge section via `merge_section` when `merge_llm_model` is set, fail-open to the deterministic raw append. Stage budget borrows the scoped cycle ceilings (was a default `LoopBudget`, unaccounted) and folds consumption back. 10 tests incl. mock end-to-end fail-open.
  - **PR-T046b (a9855aa)** — **⑥ landed with its recorded first production consumer**: `zen-vault/src/distill/tree_index.rs` (consumer-local; zen-vault does not depend on zen-memory) — pure `segment_document` (ATX `#`..`######` split, pre-first-heading text = level-0 preamble, empty bodies dropped, no headings → None = fulltext fallback) + `render_toc` (outline riding each per-section prompt). **`[agentic.loop] tree_index_pages`** (default 20, clamp 5..=200, env `ZEN_LOOP_TREE_INDEX_PAGES`): a note exceeding `pages × 3000` chars is distilled one bounded call per heading section — removing the single-call 12k truncation blindness; below threshold = byte-identical single-call path. Budget exhaustion inside the section loop must `return` (a `break` would let the next note start fresh).
  - Gate: **`bin/test` 2934 passed / 0 failed / 25 skipped** · `bin/lint` exit 0. Config sample: `config/config.toml` `[agentic.loop]` (keys commented out — defaults preserved).

- **Compile-hygiene Phase 4 — deferred backlog (2026-09-28, E5+E7 from the 2026-09-27 CEO review)**:
  - **E5 Page versioning + rollback (e8ef9e2)** — `zen-vault/src/wiki/iterations.rs`: `PageIterations` captures the prior bytes of every wiki page the pipeline is about to overwrite into `vault/iterations/<wiki-rel-dir>/<stem>/<unix-millis>.md` (+ `diffy` unified-diff sidecar) — **outside the wiki dir on purpose**: `wiki_page_inventory`, the compile whitelist, the lint walk and the CAS `VersionSnapshot` all enumerate the wiki dir, so iteration files inside it would poison every one of those consumers. Capture hooks: compile overwrites (`compile_with_policy` gained an `Option<&PageIterations>` param; `CompileReport{ghostlinks, iterations}` replaces the bare `GhostlinkReport`), merge-target rewrites and `[[wikilink]]` rewrites in `execute_merge_plans` (now returns `(merged, captured)`). Captured files are `txn.track_path()`-ed, so CAS rollback and ④ crash replay cover them with zero new mechanics. `restore` captures the clobbered current version first — a rollback is itself reversible. Manual surface: `zen wiki rollback [--list] [--to <millis>]` (name resolved with the whitelist's NFC normalization); crash recovery stays the ④ delete-based cycle replay — the two never mix. Retention: `vault/iterations` age-delete @ 90d (journal-aligned). `ScopedRunOutcome`/`LoopCycleReport.iterations_captured` + `loop.page.iterations` audit line (ghostlinks flood discipline: only when ≥1 captured). Drive-by fix: the compile page write claimed "atomic write via temp rename" in its comment but was a bare `std::fs::write` — now actually `zen_core::atomic_file::write_atomic`. Identical overwrites and creates are never captured; unreadable (non-UTF8) priors fail open.
  - **E7 SKILL.md export (697daee)** — `zen-vault/src/wiki/skill_export.rs` + `zen wiki export-skill [--output <path>] [--json]`: renders the wiki inventory into an [Agent Skills](https://agentskills.io/specification) file so Claude Code / Codex CLI / Gemini CLI read the KB with zero runtime machinery. Spec discipline: frontmatter emits ONLY the portable fields (name/description; tool-specific extensions like Claude Code's `when_to_use` fail claude.ai/Skills-API packaging with a hard error); fixed `zen-wiki` identity whose directory name equals the frontmatter `name` (spec requirement). Body: conventions + per-directory page index (OKF §6-style `- [Title](rel) - description` links, `index.md`/`log.md` exempt like the lint), capped at 240 entries with an explicit truncation marker to stay under the 500-line skill-body budget. Default target `~/.zen/skills/zen-wiki/SKILL.md`; `--target agents` (T213) writes the vendor-neutral `~/.agents/skills/zen-wiki/SKILL.md` that codex-style and pi-style CLIs discover — explicit `--output` still wins over both (pure resolver `resolve_skill_export_path`, pinned). zen's own `SkillLoader` is multi-root since T212 (D-E zen-native first: `~/.zen/skills` > project `.agents/skills` walked cwd→repo root closest-wins > `~/.agents/skills`; same-name collisions first-wins + `tracing::warn` naming both paths; `new_from_dir` and the trigram router stay single-root/unchanged), so it discovers exports from either target (FR-039 neutral plane; discovery pinned by `zen-agents/tests/skill_export_discovery.rs`). Pure renderer (`render_wiki_skill_md`) pinned for byte-0 `---`, name conformance, ≤1024-char description.

- **Compile-hygiene Phase 3 (2026-09-28, docs/designs/openkb-compile-hygiene.md ⑤+E1, ⑥ deferred-with-finding)**:
  - **⑤ Skill trigger eval (6cb448e)** — `zen-agents/src/skill_trigger_eval.rs`: report-only dual-metric grader for confirmed skills — `trigger_accuracy` (per-trigger best match against the skill's rendered `## Evidence` observations, precision direction) + `body_alignment` (per-observation best trigger coverage, recall direction); fully deterministic trigram arithmetic reusing the router's own `score_trigger` (now `pub(crate)`) so a graded score means what a live hit would score; zero labels (T173 lesson). Every confirmed skill ends up with both scores or an explicit `NotEvaluated` reason (no triggers / no evidence and empty body / unparsable SKILL.md) — acceptance is 100% accounted-for skills. Surfaced as the `skill_eval` section of `zen discover report` (pure read, gates nothing). `SkillLoader::new_from_dir` constructor added. Hand-written skills without an Evidence section degrade body_alignment to None with the trigger score kept.
  - **E1 Semantic lint (ecf42aa)** — prerequisite deliverable FIRST: `docs/designs/e1-semantic-lint-writers.md` records the grep-verified writer inventory (13 wiki writers, file:line each) and the READ-ONLY decision (findings flow through the existing lint reporting path; no new wiki file surface → the second-writer prohibition is satisfied vacuously and E2's code-managed frontmatter invariant is preserved). New `[agentic.lint] semantic` (zen-core `LintConfig`, default **false** — the gate ships closed, zero LLM calls unless the operator opens it, T168 shadow-mode precedent; env `ZEN_LINT_SEMANTIC`). `zen-vault/src/tindy/semantic_lint.rs`: opt-in LLM audit of bounded page digests (40 pages × 2000 chars) for contradictions/gaps/stale/redundancy; provider call via the established spawn_blocking guard (route/call sync + OllamaProvider nested-Runtime hazard), Sensitivity::Private local-first; fail-open everywhere (unconfigured/unreachable/garbled → zero findings + warn); hallucinated page names, unknown kinds and empty notes dropped during validation. `LintResult.semantic_findings` + lint report section + `zen wiki lint` counts/detail. `wiki_compile::strip_frontmatter` now `pub(crate)`.
  - **⑥ Tree index — DEFERRED with a grep-verified finding**: the only mountable branch (`MemvidIndexer::index_single_file`/`index_incremental`) has **zero production callers** (the memvid-indexer worker drives `index_all` = M2+M3+M4; M3 notions and M4 wisdom dirs are machine-generated small pages, M2 journals are bounded at 100KB — no long-doc fulltext path exists in production), and the distill pipeline is deterministic/LLM-free until T046 (llm_distill) — where the long-doc cost actually appears. Per the plan's own guardrail #1 (no primitive without a consumer) and the ③ `[agentic.cache]` precedent, the segmentation module + `tree_index_pages` config key land WITH T046 (its first production consumer, alongside the cache key). The implementation (pure `segment_document`/`render_toc`, multi-level heading split, below-threshold→None byte-identical fallback, split-failure→fulltext fallback, threshold/integration tests — 391/391 zen-memory green) was built, verified, then reverted uncommitted; the design is recorded in the plan doc for T046 to restore. Acceptance 6 (index build time < fulltext p50) measured when the consumer lands.

- **Compile-hygiene Phase 2 (2026-09-28, docs/designs/openkb-compile-hygiene.md ④+E2+E3)** — crash-safe, provenance-tracked compile lifecycle; one PR per workstream:
  - **④ Crash replay (b158003)** — `RecoveryManager::replay_stale_transactions`: at `run_scoped` entry (before the new cycle's `txn.begin()`) stale `.txn-*.jsonl` tracking files from dead processes are rolled back via the SHARED `TransactionScope` rollback core (`rollback_tracking_file`, now `pub(crate)`, counted) — closing the gap where an interrupted cycle's partial writes survived forever (txn names are per-cycle uuids, so the old `begin()` truncation never touched them). `STALE_TXN_MIN_AGE = 1h` (cycle bounded by the 900s watchdog — a hour-old tracking file is dead with certainty); live-cycle files never touched; fail-open. Kill-point tests ×2 (mid-write / after-write-before-commit — full rollback pinned as correct semantics, replacing `test_recover_is_stub`) + live guard + clean no-op. `TransactionScope::new_for_logs` constructor seam. Retention backstop: txn files age-delete@7d — **deviation from plan's rotate@5MiB recorded at the policy site** (rotation would hide a tracking file from the replay scan pattern).
  - **E2 Page-level sources provenance (3d55a36)** — `WikiPage.sources: Vec<String>` (serde default, additive); `note_to_page` fills from the note's `file_path`; `render_page` emits `sources: [...]` frontmatter. **Code-managed** — the LLM never writes it (plan guardrail). Lint gains an advisory Source Provenance section: machine-rendered pages (`created_at:`+`updated_at:` signature) lacking non-empty `sources:` are reported; hand-written pages (neither key) never reported — acceptance "new compiled pages 100% sourced, lint=0" achievable without permanent legacy noise. Report section + CLI/TUI counts.
  - **E3 Delete cascade (7bf3dc4)** — `distill/cascade.rs`, two cycle-internal entry points (no new CLI): `redirect_page_source` (archive hook, driven by the cycle's `migrated_files` inside `run_scoped` — pages citing the inbox path re-pointed at the durable archive dest, txn-tracked so CAS rollback restores old sources with the rolled-back dest) and `sweep_dead_sources` (cycle start, inside the txn after `begin()`: strip dead entries, delete fully-orphaned machine pages, rewrite inbound `[[links]]` to plain text via NFC `normalize_alias` match). Safety contract: pages WITHOUT a `sources:` key are hand-written and never touched; `sources: []` pages kept (never deleted). Acceptance "dead source reference count reaches zero" pinned incl. idempotent resweep. `slugify` now `pub(crate)`.
  - Gate: **`bin/test` 2896 passed / 0 failed / 25 skipped** (Phase-1 baseline 2881; +15 Phase 2 tests) · clippy `-D warnings` clean · fmt clean.

- **Compile-hygiene Phase 1 (2026-09-28, docs/designs/openkb-compile-hygiene.md ①②③)** — OpenKB-derived compile hygiene, one PR per workstream (1A granularity), all config under the new `[agentic.compile]` section:
  - **① OKF conformance lint (0ac2a8f)** — `zen wiki lint`/`zen lint`/TUI lint gain an OKF section reporting pages missing a non-empty `type:`/`description:` frontmatter key; advisory only (legacy pages reported, never an error); generated `index.md`/`log.md` exempt (OKF §6); gated by `[agentic.compile] okf_lint` (default true, env `ZEN_COMPILE_OKF_LINT`); `LintResult.okf_missing: Vec<OkfFinding>` + report renderer + CLI counts.
  - **② Wikilink whitelist + ghost-strip (61b98a6)** — `WikiCompiler::compile` is two-pass: whitelist = pre-existing wiki page names (dashed slug + de-slugified forms) ∪ same-run titles/slugs, compared through `zen_repo::normalize_alias` NFC (NFD link spellings resolve; `[[Rust Guide]]` matches `rust-guide.md`); ghost `[[target]]` under `[agentic.compile] ghostlink_enforcement` (strip|warn, default strip, env `ZEN_COMPILE_GHOSTLINK_ENFORCEMENT`, invalid→warn+strip) is rewritten to plain text in body AND frontmatter `wikilinks:` list; per-run `GhostlinkReport{stripped,warned}` rides `ScopedRunOutcome`/`LoopCycleReport`; zen-loop emits one `loop.compile.ghostlinks` audit line per ghost-seeing run (zero-ghost cycles write nothing — counters still ride the report); `compile_with_policy` is the test/config entry, bare `compile()` = Strip. One pre-existing extraction test updated to seed its targets under the new contract.
  - **③ Prompt-cache breakpoints (2785d9d)** — `zen-provider/src/cache.rs`: `CacheSegments::build(system, docs, summary, whitelist)` (canonical OpenKB partition; the whitelist block is fed by ②'s whitelist) + `PromptCacheControl` trait defaulting no-op (Q-A); `AnthropicProvider::complete_cached` bypasses rig-core via raw `/v1/messages` (client doesn't expose `cache_control`) — segments become system blocks with ephemeral markers on last-doc/summary/whitelist (3 cumulative checkpoints), `cache_creation/cache_read_input_tokens` echoed for observability; `build_cached_request_body` pure/test-pinned. Deviation recorded: `[agentic.cache] breakpoints` key lands with T046 llm_distill wiring — no-reader key = phantom-key anti-pattern (T192).
  - **2A arena gate disposition (recorded in plan)** — `zen discover arena` = 4/5 (bare-singletons pre-existing loss; arena contestants exercise `correlate`/`generate_from_gaps` only, zero overlap with the compile path ② changes); compile-path regression coverage is the compile suite itself. Arena corpus not covering compile is a recorded limitation.
  - Gate: **`bin/test` 2881 passed / 0 failed / 25 skipped** (baseline 2856; +25 Phase 1 tests) · clippy `-D warnings` clean · fmt clean. One pre-existing test-isolation flake (`decision::tests::binary_gate_emits_the_calibration_record` under full-lib runs) reproduced on clean HEAD, not Phase-1-caused.

- **Agentic TUI polish pass (2026-09-22..24, 002-agentic-tui)** — inline stream UI refinements, all CHANGELOG-documented:
  - **Curated competitor UX polish (T084)** — slash popup gains registry-order group headers + dimmed description column; large bracketed pastes (>3 lines / >200 chars) collapse into an atomic `[Pasted N lines / M chars]` pill (expanded at submit/history); footer shows `● working Ns` while streaming; Esc-dismissed popup stays closed until the command token changes (codex-rs DismissedToken mechanics); Ctrl+J inserts a newline (tmux/SSH-safe LF fallback); selected row carries the `›` glyph. Popup slot stays ≤8 rows (shrink-to-fit per ADR-006); no fuzzy matcher, no Ctrl+C change.
  - **Stream-stall instrumentation (T082, NFR-008/009 measurement only)** — inter-token gaps >200 ms during a streaming turn append one `tui.stream.stall` audit line per episode, surfaced as additive `orchestration.stream_stalls` in `zen discover report`; `#[ignore]` PTY smoke `input_latency_smoke` measured keystroke→echo at 7 ms.
  - **Codex-parity slash popup** — selection grammar adopted from live codex v0.155.1 (vs its source `codex-rs command_popup.rs`): full-width reversed bar + bold name, selected description un-dims, matched filter characters bold, description column measured over all rows (stable while scrolling), exact matches hoist above prefix matches, sel/total index dropped; group headers + scroll arrows kept deliberately.
  - **Diff-fence red/green rendering (T031)** — `diff`/`patch` fences render explicit deltas at the markdown fence layer (not syntect): `+` green / `-` red, `@@` hunk headers dim cyan, `+++`/`---` file headers dark gray, context unstyled; NO_COLOR-safe. PageUp/PageDown raw-mode key capture pinned by test (T025). Task audit closed T030 (absorbed by T056/T057 tool cards) and T068 (tmux smoke passed on tmux 3.7b); T026 fuzzy + T038 dynamic viewport deferred with recorded rationale.
  - Gate: **`bin/test` 2856 passed / 0 failed / 25 skipped** at doc time (t031 ×2 + t025 ×1 included).

- **Four-dimension /review + adversarial pass (2026-09-21)** — user-directed audit of 数据目录隔离 / 表存储兼容 / 配置兼容 / vault 清理合并生命周期, GAN-verified by a fresh-context subagent (Codex CLI broken: stale model pin). Gate: **`bin/test` 2779 passed / 0 failed / 21 skipped · `bin/lint` exit 0** (baseline 2763; +16 retention tests). Tracked as Phase 28 (T190-T194).
  - **T190 retention subsystem (D2, full)** — `zen-vault/src/distill/retention.rs` policy engine + daily `RetentionWorker` (`0 30 3 * * *`, registered in BOTH scheduler profiles — the scheduler lease prevents double-fire; `create_default_scheduler` set-equality preserved; 17 Full / 15 InApp): audit.jsonl rotate@10MiB keep-3, loop-gaps/discovery-tree rotate@5MiB keep-2; age-deletes outbox>30d, journal>90d, research-suggestions>30d, wake-up>14d, virtue_logs(vault/memories/virtue_logs — the real writer path)/logs-archive/vault-output>365d, wisdom/suggestions>180d; demoted-beliefs keep-newest-200; **quarantine REPORT-ONLY (never deletes user data)**. One `loop.retention.applied` audit line per run (per-home removed/rotated/bytes_freed); per-home fail-open; crash-safe oldest-first rotation cascade. Config `[agentic.retention] enabled` (default true) / `dry_run` (default false), env `ZEN_RETENTION_ENABLED`/`ZEN_RETENTION_DRY_RUN`.
  - **T191 `zen clean` confirmation gate (D1, adversarial NEW-1 HIGH)** — `zen clean all`/`trash` (and bare `zen clean` → Trash default) destroy data OUTSIDE ~/.zen (system Trash via Finder AppleScript, other apps' logs, IDE caches); now gated behind a y/N prompt naming the destruction; `--yes`/`-y` bypasses for scripts; non-interactive input aborts; Cache stays ungated.
  - **T192 embedded config truth fix (D3)** — never-parsed keys stripped from `config/config.toml`: 13× `[agents.*] llm_preferences/max_sensitivity` (never parseable in any git revision — profiles are registry-hardcoded), `[session] drain_timeout_secs`, `[tools.fs_watcher] max_watchers`, `[sandbox.rlimits]`, `[sandbox.env_scrub]`, legacy `[qqbot]` comment block; `[sandbox.network_policy]` commented + marked NOT-YET-WIRED (T080). Spec gains a "Configuration Compatibility Record" (no-deny-unknown policy, required-field whole-parse hazard, per-section array merge semantics, legacy inert sections).
  - **T194** — `open_versioned`/`probe_latest_version` (zero production callers, verified twice) kept by owner decision as the designated binary-downgrade path; recorded in data-model.md with revisit trigger.
  - **T193 review fixes** — README + README_zh quickstart taught REMOVED commands (`zen note create`, `zen search run`, "29 commands") — rewritten to the real surface (TUI first-run, inbox file-drop, doctor, wiki list); `memory_wiring_regression.rs` fixed `/tmp` path → tempdir; `ZEN_CRON_TZ` doc-comment → `ZEN_CRON_TIMEZONE`; `e2e_consolidation_test` inert `provider_type` key removed with intent documented.
  - **Verified-sound dimensions** (do not re-litigate): test isolation clean (ZenPaths ZEN_HOME guard paths.rs:255-263, ZenTest harness env_clear+tempdir, T186 canary); migrations 001-010 all additive with paired tests (sole wart: 003 `recorded_at` non-constant `datetime('now')` default — display-only column, no consumer); Falsified by adversarial: subconscious.md IS bounded (dream compress_old_logs @100KB); "LlmConfig dead" corrected — live programmatic type (production uses `from_agentic`).

- **Phase 27 closeout — all open convergence tasks landed (2026-09-20, `/speckit-implement`)**. Gate: **`bin/test` 2763 passed / 0 failed / 21 skipped · `bin/lint` exit 0** (baseline 2731; +32 tests). Uncommitted (commit on request).
  - **T180 (P1) closed** — `placeholders.json` last-write-wins data-loss window eliminated: `zen-vault/src/distill/placeholders_store.rs::merge_save_placeholders` — cross-process flock on `<logs>/placeholders.lock` (mirrors the migration-lock guard), fail-open load, deterministic union-by-slug merge (merged > claimed > placeholder; ties ⇒ incoming), atomic persist via `write_atomic`; both writers (`pipeline.rs`, `zen_loop.rs`) route through it; 6 tests.
  - **LLM cost reporting made real** (outside-voice #2) — `[providers.<name>] input_cost_per_million/output_cost_per_million` (additive config), `usage_to_cost_usd` (local ⇒ structural 0.0), `DefaultRouter::complete_metered` + `model_metadata`, month-keyed `WorkerCostLedger` persisting to `<logs>/worker-costs.json`; the `[cron] llm_cost_cap_usd` cap now trips on real cost (test drives the actual `tick`); 6 workers report metered costs; zen_loop verified LLM-free.
  - **T187** — `zen wiki loop gaps` surfaces the Discovery Loop's pending `user_questions` (`{"gaps":[...],"user_questions":[...]}` JSON shape — contract tests in `crates/zen/tests/loop_*` updated to the new shape).
  - **T189** — `zen doctor` 8th probe `outbox`: staged morning-briefs + unconfigured `[channels.qqbot]` ⇒ FAIL naming the fix; 5 tests.
  - **T185** — SC-002 measured: `attempts` field on `loop.note.archived` audit lines + `zen discover report` → `success_criteria.first_attempt_success` (pct + n, withheld <30, legacy lines never assumed); SC-005 measured: `status_read_ms` in `zen wiki loop status --json`.
  - **T183** — phantom `WhatsAppChannelConfig`/`TelegramChannelConfig` deleted from config parse + `zen config` display (no `deny_unknown_fields`; stray sections inertly ignored, test-pinned).
  - **T186** — relative-path test leak fixed at the culprit (`search/tier2.rs:263` decoy-db regression test; rewritten tempdir + canary).
  - **T188** — FR-033 first-run host-ingest feasibility recorded as `research.md` **D30** (≤120 notes/hour deterministic ceiling at defaults; multi-day LLM-bound backfill; three ranked onboarding options: config-only catch-up recipe, phased per-source, heuristic-first).

- **Multi-instance concurrency hardening + convergence pass (2026-09-20, `/speckit-converge` + `/speckit-analyze`)** — closes the "解决下zen serve 启动多个如何处理并发写" ask and records it. Gate: **`bin/test` 2731 passed / 0 failed / 21 skipped · `bin/lint` exit 0** (baseline 2727).
  - **Startup lock** (`zen-gateway/src/daemon.rs`): `StartupLock`/`startup_lock_path` taken **before** `uds::bind_socket`, making the exists→probe→unlink-stale→bind hand-off exclusive. The old `uds.rs` comment claimed "exactly-one semantics" from the bind alone — it was false (two starters could both unlink-then-bind). Closes the TOCTOU window that 004 `tasks.md:167` had explicitly parked as deferred backlog. Lock is per data directory (`socket.with_file_name("gateway.lock")`) — one ZEN_HOME owns one daemon; kernel releases on SIGKILL. Losing starts fail fast naming the cause.
  - **Pid ownership moved to the daemon**: `GatewayDaemonConfig.pid_path`; the daemon writes its own pid after winning the lock. The CLI no longer pre-writes the child pid (removed both the pre-spawn and post-readiness writes), eliminating a clobber class where a losing start could overwrite the winner's record. CLI start is now idempotent: a `gateway_is_live()` pre-probe prints `✅ Gateway already running (pid: N)` and exits 0 **without touching the pid file**; readiness checks child liveness first, then the handshake, then requires the pid record to match the spawned child (live *different* pid → "this start attempt exited").
  - **Cross-process migration lock** (`zen-repo/src/client.rs`): `acquire_migration_lock`/`migration_guard` held across `run_migrations` in **both** `open` and `open_lazy`. The load-bearing fact: **sqlx-sqlite's own `Migrator::lock` is a no-op**, so nothing else serialized concurrent first-run migration. `SqliteError::{Io,MigrationLock}` added.
  - **Atomic-write helper** (`zen-core/src/atomic_file.rs`): `write_atomic` (tmp→fsync→rename) wired into nine production sites — `graph_verify::save`, `skill_precipitation::{write_pending, SKILL.md}`, `reflection.rs`, `pipeline.rs` (provenance ×3, merged page rewrite, target provenance), `vendor_eval::save_report`, `zen_loop` (attempts, refinement-queue, loop-last-report).
  - **Known remaining concurrency gap (recorded, not improvised)**: `pipeline.rs:~722` and `zen_loop.rs:~1529` both `registry.save()` the **whole** `placeholders.json`, so last-write-wins can drop the other writer's declarations — it needs a read-merge-write under a lock (T180).
  - **Convergence findings (Phase 27, T175-T185)**: the entire concurrency set above was **unrecorded** in the spec — `StartupLock`/`pid_path`/`acquire_migration_lock`/`write_atomic` appeared nowhere in `spec.md`/`tasks.md`/`plan.md`/`data-model.md`. Also found and recorded: `spec.md:229-230` still claims `select_parents`/`sample_stepping_stones` are "DEAD (test-only)" though both have **zero** non-test occurrences (deleted 2026-09-19 — the spec documented phantom primitives); `router.rs:1027` told users to set `[agentic.llm]`, a section `AgenticConfig` does not have (**fixed**: now names `default_provider` + `[providers.<name>]` / `zen provider`); `[channels.whatsapp]`/`[channels.telegram]` config structs are parsed and shown by `zen config` but no channel implements them; several `[agentic.loop]`/`[cron]` keys are read by code but undocumented; `skip_extensions` still implements FR-012, which was retired to Non-Goals.
  - **Analysis-method lesson (recorded because it produced a false number first)**: FR coverage reads **100% (40/40) only when `tasks-archive-2026-09-12.md` is included** — Phases 1-19 live there, so scoring `tasks.md` alone falsely reports 57% with 17 "uncovered" FRs. Likewise task checkboxes are `[X]` uppercase, so a `[ x]` pattern matches almost nothing. SC-002 ("95% first-attempt success") is a runtime outcome metric, not buildable; its *measurement* is the gap (T185).
  - Tests added: `crates/zen-gateway/tests/startup_lock.rs` (refusal + release-restores-availability + per-directory scoping), `crates/zen-core/tests/atomic_file_tests.rs` (replacement/no-residue + no-partial-read under concurrent writer).

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
    - **L1 decision substrate in shadow mode (T168)** — `crates/zen-agents/src/decision/mod.rs`: `DecisionRung` / `DecisionOutcome<C>` / the `DecisionFn<C>` rung trait, L0 `KeywordRung` and L2 `LlmRung` adapters over the existing code paths, an injectable `TextEmbedder` (production `VaultTextEmbedder` over the existing local embedding path; a deterministic fake in tests, so no ONNX is required), and `EmbeddingIntentRouter<E>` matching a small in-code seed intent-example table by cosine similarity. `run_intent_shadow` runs **after** the production intent decision and records the comparison in a `loop.decision` audit line (rung, choices, agreement, confidence, latency), gated by `[agentic.intent] shadow_embedding` (**default false**, env `ZEN_INTENT_SHADOW_EMBEDDING`), fail-open on embedder errors. It applies **no threshold** and cannot alter routing — asserted by `shadow_observation_does_not_mutate_production_intent`. Rationale: V13-A.3 forbids inventing τ, so the ladder is built as an observer that starts accruing the paired L1/L2 samples (disagreements are the highest-information cases) which calibration needs; **T169's migration stays blocked on labeled data**.
    - **L1 calibrated decision layer — T168/T169/T170/T171/T174 all IMPLEMENTED (2026-09-19 follow-up pass, commits `416e7b1`/`09c682c`/`f952ce7`/`073e0b4`)**. The design that makes this legitimate rather than a repeat of the uncalibrated-heuristic failure: **every threshold τ is sourced from a calibration artefact** (`logs/decision-audit/thresholds.json`, written by T173 once labels exist) **or an explicit config override, and `None` means the gate is closed** — so absent calibration data the pre-T168 behaviour is preserved *byte-for-byte*, and each case is pinned by a test asserting the old behaviour with no threshold present.
      - **T169 ladder**: `decision::classify_intent` — embedding router → `LocalClassifierRung` → `LlmRung` (0.7 verbal gate intact). No τ ⇒ it delegates to `intent::classify` unchanged (asserted with a 1.0-confidence L1 rung that must still lose). `LlmOutcome::L1Resolved` + `IntentSource::L1` make the L1-served rate measurable (<10% LLM-intent baseline); `loop.decision` logs only when L1 actually served, so the L2 path is not double-counted.
      - **The expressiveness inversion is fixed (not τ-gated)**: rung verdicts carry an optional signal, `resolve_agent(category, signal)` prefers a resolved signal over the category default, the LLM's signal is validated against `INTENT_SIGNALS` (a hallucinated signal cannot route), and the seed table carries a signal per example — **all 13 registered agents are reachable from every rung**.
      - **T170 de-anchored judge**: `review/deanchored.rs` — the local judge commits its own answer *before seeing the draft* (that exclusion IS the de-anchoring, pinned by test), then scores against its commitment (2607.05904). Escalation to the frontier only below τ; no τ ⇒ local phase skipped and the frontier call is byte-identical to pre-T170. Fail-open at every hop; an unreadable confidence is an error, never a default.
      - **T171 binary classifiers**: `classify_binary(threshold, heuristic, l1, input)` — no τ ⇒ the classifier is not even called and T161's substring rules stay authoritative; below τ/abstain/error also falls back, so a broken classifier cannot rewrite FR-034 reward bookkeeping. **T161's heuristics are deliberately not retired** until a threshold is calibrated against labeled corrections.
      - **T174 vendor gate**: `zen-vault/distill/vendor_eval.rs` — the four axes (cardinality vs 255 cap, latency p50/p95 under bounded concurrency, ECE reusing `decision_audit::ece`, local-first) with `NotEvaluated`/`InsufficientData` states; the aggregate passes only when every axis passes **and every workload had data**, so it reports `insufficient_data` today rather than fabricating a verdict.
      - **New config** (all decision-layer keys default to absent = gate closed): `[agentic.intent] l1_threshold` (env `ZEN_INTENT_L1_THRESHOLD`), `[agentic.review] escalate_threshold` (env `ZEN_REVIEW_ESCALATE_THRESHOLD`), `[agentic.classifiers] {correction_threshold, citation_threshold}` (env `ZEN_CLASSIFIER_*_THRESHOLD`).
      - **What remains is not code**: label supply (`logs/decision-audit/labels.jsonl` needs real usage + human adjudication before any τ can be calibrated), observing the baselines from production traffic, and vendor early access. Gate: `bin/test` 2693 passed / 0 failed / 21 skipped · `bin/lint` exit 0.
    - **Label supply + threshold derivation + baselines — the "not code" items, resolved (2026-09-19, commits `ba05b3b`/`03daf0d`/`cd5e3e1`)**. Re-examined, three of the four "not code" blockers were missing *plumbing*, not missing reality:
      - **Threshold derivation** (`zen-vault/distill/decision_audit.rs`): `select_threshold` picks the widest-accepting operating point meeting a stated precision target (0.95 on the **accepted** set — a mis-accept is what a fast path risks, not aggregate error) subject to a coverage floor (0.10) and a 100-label minimum; `write_thresholds` persists it (merge semantics — an unrecalibrated field keeps its previous value; refusals never persisted; atomic; 4-decimal rounding; risk metadata under a sibling `_meta` key, ignored by `DecisionThresholds::load`). **The operating point is now CERTIFIED, not in-sample (2026-09-19 `/review` pass)**: selection uses an exact **Clopper–Pearson** one-sided upper bound on the accepted set's error rate at the **Bonferroni** level γ = δ/K over a **pre-fixed K = 8 grid** (`THRESHOLD_GRID_HUNDREDTHS = {0.00,0.15,0.30,0.45,0.55,0.65,0.80,0.90}`, CIC arXiv:2607.04430 Thm 3.3; LTT arXiv:2110.01052) — under Bonferroni every grid point is simultaneously certified on one ≥1−δ event, so any selection rule over certified points (coverage floor, max-coverage preference) inherits the guarantee. Two candidate designs were rejected on the math and must not be re-proposed: the **Wilson** bound is anticonservative (a 52-sample zero-error tail certifies at UB≈0.0495 while its true error can exceed α with probability 6.7% > δ), and **stop-at-first-feasible is not fixed-sequence** (Bauer 1991 licenses stopping at the first *non*-rejection; stop-at-first-*rejection* was constructed to false-certify at ≈6.1% > δ). Honest consequence recorded: with the lift guard requiring ≥5% base errors and the exact bound requiring a ≈99-error-free accepted set at K=8, **certification realistically begins around 120 labels** (K=101 would need 149 and could never certify at 100). Two guards keep this from being a new invented heuristic: **`UninformativeConfidence`** (if confidence does not separate correct from incorrect decisions — precision lift < 0.05 — *no* threshold is emitted however good the numbers look; V13-A.3's "never gate on verbalized confidence alone", arXiv 2601.07767, enforced in code) and **`CannotMeetTarget`** (a threshold that is perfectly precise because it accepts 2.5% of cases is precision bought by refusing to decide). Calibration is per `(kind, rung)` — `intent_l1` reads (intent, L1) and never the LLM/keyword rung's confidence (`THRESHOLD_SOURCES` is the single table stating this). Certificate scope, recorded: the bound is conditional-on-acceptance (coverage is best-effort utility absorbed by escalation, never a safety claim), per-field δ is independent (a joint statement would need δ/4), and labels are assumed exchangeable with future decisions (drift ⇒ recalibrate; risk metadata rides with the value so a stale threshold cannot masquerade).
      - **Baselines** (`zen-vault/distill/baselines.rs`): intent p95, LLM-intent call rate, routing distribution + shadow agreement, and the frontier-call rate, each with its `n` and a first-class `InsufficientSample` below 30 observations. The frontier rate is derived from `loop.decision(decision_kind=review).escalated`, which `JudgeTrace`/`JudgedVerdict` + `record_decision` now emit — the trace records the **local** verdict even when a case escalated, deliberately, because calibrating the escalation threshold needs (local confidence, was it right) and escalated cases are the informative ones. The obsolete `FRONTIER_NOT_DERIVABLE_REASON` constant and its dead constructor were deleted rather than left as a stale excuse.
      - **Label supply** (commit `cd5e3e1`; hardened 2026-09-19 `/review`): the blocker was **content privacy, not tooling** — `loop.turn.review` retained no user input, so no decision could be adjudicated at all. Fixing it means writing user input to a log, so it is **opt-in and off by default**: `[agentic.audit] decision_excerpt_chars` (env `ZEN_AUDIT_DECISION_EXCERPT_CHARS`, 0 = off, clamped ≤500, char-boundary truncated, newlines flattened so one excerpt cannot forge a second JSONL line; rules live once in `AuditConfig::excerpt`). `zen discover label status|next|set` is the workflow, and `next` states plainly when no excerpt was recorded and how to enable one rather than letting the user believe they see full context. **`append_label` now validates before writing** (the earlier "rejects an unknown record id" claim was false): the id must exist in the dataset (`UnknownRecordId`) and the label must canonicalize case-insensitively onto the kind's vocabulary (`InvalidLabel`, mirrored from the emitters: intent `Query|Action|System|Conversation`, review `approved|vetoed`, correction/citation `yes|no`) — a typo would exact-compare false against every decision forever; entries also record `recorded_at` (additive, append-only last-wins). Verified end to end: `set` appends, `calibrate` still refuses at 1 of 100 required labels.
      - **Correction/citation emitters (F7, 2026-09-19 `/review`)**: `THRESHOLD_SOURCES` declared `correction_l1`/`citation_l1` calibrated from `(correction|citation, L1)` records, but **nothing emitted them** — 2 of the 4 gates could never accumulate samples (same invisibility class as the missing `decision_kind` key fixed in `ba05b3b`). `classify_binary` now appends one `loop.decision` line per invocation with the keys `record_from_line` extracts; classifier observations carry `rung:"L1"` + confidence (below-τ included — legitimate future-τ samples), abstain/error/heuristic carry `rung:"L0"` and no confidence (observability only). Pinned by `binary_gate_emits_the_calibration_record`.
      - Also fixed: the ladder's `loop.decision` line now emits `decision_kind` (the extractor reads that key, not `decision`) — without it L1 records landed under kind `"decision"` and were **permanently invisible to calibration**; and audit confidences are rounded via `audit_score` (a bare f32 widened to f64 serializes as `0.6000000238418579`).
      - **New surface**: `zen discover calibrate [--write]` (report-only unless `--write` — writing OPENS the gates, never implicit) and additive `thresholds`/`baselines` sections in `zen discover report`.
      - **Truly external now**: earning the labels from real usage (every mechanism exists; the privacy switch is the user's), observing the baselines (needs traffic), and vendor early access. Gate: `bin/test` 2722 passed / 0 failed / 21 skipped · `bin/lint` exit 0.
    - **Linux-CI portability fix (2026-09-19, commit `09c682c`)**: the launchd helpers in `zen-cli` (`LAUNCHD_LABEL`, `render_plist`, `plist_path`, `gui_domain`) were compiled on every platform but only used inside `cfg(target_os = "macos")` blocks, so a **non-macOS build failed under `-D warnings`** with dead-code plus `needless_return` — invisible to the macOS-local lint. They are now genuinely platform-gated (with macOS-gated tests) and `install_launchd`/`uninstall_launchd` are split into per-platform definitions instead of a cfg-block-inside-body.

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
  - Former "known landmine" (CLOSED 2026-09-30, false alarm): the record claimed all six zen-provider sync `complete()` impls panic under async contexts via nested `Runtime::new()` — code archaeology shows every sync site (6× `complete()` + 2× `embed()` in embedding.rs) has ALWAYS used the dedicated-thread pattern (`std::thread::spawn` → own runtime → `.join()` maps panics to typed errors) since `9223367`, which cannot hit the nested-runtime panic (the spawned thread has no ambient runtime; any panic is caught by `join()`). Pinned by `crates/zen-provider/tests/sync_complete_no_panic.rs` (Err-not-panic from pure sync + current-thread + multi-thread runtime contexts). The call-site `spawn_blocking` guards (intent.rs, review_pipeline, semantic_lint) remain good practice — they keep runtime worker threads from blocking on the sync surface. **health_check was the real exception (2026-10-13)**: `OllamaProvider::health_check` used `reqwest::blocking` on the CALLING thread, reached from async via `enforce_sensitivity → is_local_llm_available` on Private/Confidential routing (delegate `run_single` since 006, orchestrator execute/redraft, zen_loop `resolve_model_tier`, zen-vault `llm_distill::call_llm`, zen-cli `provider_command`) — debug-build panic verified empirically ("Cannot drop a runtime in a context where blocking is not allowed"); now dedicated-threaded like its siblings (leaf fix covers all chains end-to-end; pins in sync_complete_no_panic.rs health_check ×2 + delegate_task.rs Confidential current_thread test)
- **001 vision convergence (2026-09-08, 006 T372-T379 via /speckit-converge)** — the 001 multi-agent scheduling vision lands on the 006 kernel:
  - T372 intent: `zen-agents/src/intent.rs` — LLM-first classification into `Intent{category: Query|Action|System|Conversation, agent, signal, acl, confidence, source}`; confidence<0.7 → Conversation fallback; LLM failure/timeout (20s) degrades to the INTENT_SIGNALS keyword fast path; `router.route()` wrapped in `spawn_blocking` (ollama nested-Runtime panic safety); audit `loop.turn.review` gains intent_category/intent_source/intent_confidence/intent_acl
  - T373 depth: `[agentic.delegate].max_depth` (default 1, clamp 1..=3, env `ZEN_DELEGATE_MAX_DEPTH`); `spawn_allowed(parent_tier, child_tier)` matrix; task-local `DELEGATE_DEPTH`/`DELEGATE_PARENT` chain (root parent "Sisyphus"); `build_sub_agent(agent, allow_child_delegation)` keeps `delegate.*` grants strictly below the cap
  - T374 fan-out: delegate.task `tasks:[...]` collect mode — `should_delegate` gates (independent/consumer-decision/bounded 32k/worth-it advisory 50k tokens) audited as `loop.delegate.gates`; `max_concurrent` (default 4, clamp 1..=8) bounded batches via join_all; single-task keeps the legacy shape, multi-task returns `{results:[...]}`
  - T375 plan-DAG: `plan.execute` (see Agent Tool Inventory) — Kahn topological layers, per-layer parallel execution, failure propagation, Hermes verdict as completion signal, summary-only return (plan artifacts stay out of parent context)
  - T376 persistence: migration 006 `workflow_plans`/`workflow_tasks` + `WorkflowRepo`/`TaskCheckpoint`; per-task idempotent checkpoints ((plan_id, task_id) upsert); `resume_plan_id` replay; resumed plans close out to completed/failed
  - T377 blackboard superseded: `blackboard.rs` + tests deleted; ADR at `docs/specs/006-agentic-orchestration/adr-001-blackboard-supersession.md`; inter-agent transport = direct tool returns + batch aggregation
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
  - `SchedulerProfile::{Full, InApp}` (zen-agents `scheduler/mod.rs`): `create_configured_scheduler_with(config, profile)` registers per profile; legacy `create_configured_scheduler` = Full (unchanged signature). InApp = 15 learning-core workers (incl. `retention`; excludes `memvid-indexer` — gateway mv2 flock duty, and `morning-brief` — outbox staging consumed by the daemon drainer); `worker_ids()` is the test seam
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
