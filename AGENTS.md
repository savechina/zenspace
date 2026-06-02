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
- `rig-memvid` for context management (vector store, prompt hooks, compaction)
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
- **Agent Orchestration**: rig-compose 0.3
- **LLM Abstraction**: rig-core 0.37
- **Vector Store**: sqlite-vec + rig-sqlite 0.2
- **Template Engine**: tera + include_dir
- **Configuration**: dotenvy + 5-layer inheritance

See `.specify/memory/constitution.md` for full principles, rationale, and governance process.

## AGENTIC ARCHITECTURE

Zen routes operations through a layered agentic pipeline: notes -- consolidation -- knowledge graph -- search. Each crate handles one concern.

### Crates

| Crate | Role | Path |
|-------|------|------|
| zen | Binary entry (13-line wrapper) | `crates/zen/` |
| zen-cli | CLI library (24 commands, TUI, dispatch) | `crates/zen-cli/` |
| zen-core | Config layers, error taxonomy, path scoping, constants (13 modules) | `crates/zen-core/` |
| zen-service | Starter/wps/cleanup business logic | `crates/zen-service/` |
| zen-data | Dual API: sqlx repository + rusqlite schema (FTS5, vec0) | `crates/zen-data/` |
| zen-knowledge | 10+ services: note, wiki, 5-tier search, consolidation, lint | `crates/zen-knowledge/` |
| zen-memory | Identity context (SOUL.md, MEMORY.md, store, stats) | `crates/zen-memory/` |
| zen-agents | 13 agents, 4-tier registry, blackboard, QualityPipeline | `crates/zen-agents/` |
| zen-provider | 13 providers, 3 protocol types, DefaultRouter factory, auth resolution | `crates/zen-provider/` |
| zen-auth | Keychain + SecretRef resolution | `crates/zen-auth/` |
| zen-plugin | WASM sandbox (wasmtime) + MCP server | `crates/zen-plugin/` |
| zen-gateway | HTTP daemon (axum, placeholder) | `crates/zen-gateway/` |

### Dependency Graph

```
zen (binary)            -- 13-line main.rs -- loads .env, config, calls zen_cli::shell()
 └── zen-cli (library)  -- clap Parser, TUI (ratatui), command dispatcher
      ├── zen-service   → zen-core
      ├── zen-gateway   → zen-core
      ├── zen-knowledge → zen-data   → zen-core
      │                 └── zen-provider → zen-auth → zen-core
      ├── zen-agents    → zen-memory → zen-core
      │                 └── zen-provider
      │                 └── zen-knowledge
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
- **Library**: `crates/zen-cli/` -- exports `shell()` via `lib.rs`, contains clap Parser, TUI runner, 24 subcommand dispatchers
- **TUI**: Runs on main thread (ratatui/crossterm) when `cli.command.is_none()`

### Data Flow

```
zen note create → zen-knowledge (note service) → zen-data (sqlx repository)
zen search run  → zen-knowledge (SearchService) → tier routing → ripgrep/FTS5/vec0/graph/LLM
zen ingest      → zen-knowledge (raw directory) → IngestResult → consolidation
zen consolidate → zen-knowledge (ConsolidationPipeline) → zen-provider (entity extraction)
zen session start → zen-agents (ZenCoordinator) → blackboard → executor
zen similar find → zen-knowledge (tier3, vec0 embeddings) → zen-provider (embeddings)
zen graph query → zen-knowledge (tier4, graph.db) → entity graph
zen reindex run → zen-knowledge (Reindexer, checksums, embeddings)
zen lint run    → zen-knowledge (Linter, orphan pages, broken wikilinks)
```

## STRUCTURE

```
zenspace/
├── crates/                     # 12 workspace crates (binary/library split)
│   ├── zen/                    # Binary entry (13-line wrapper)
│   ├── zen-cli/                # CLI library: 24 commands, TUI, clap derive
│   │   ├── src/
│   │   │   ├── lib.rs          # pub use cli::shell
│   │   │   ├── cli.rs          # clap Parser/Subcommand (24 variants), shell() dispatcher
│   │   │   ├── tui/            # ratatui TUI interface
│   │   │   ├── session.rs      # Session helpers
│   │   │   ├── sandbox.rs      # Sandbox helpers
│   │   │   └── cmd/            # 24 *_command.rs dispatchers + mod.rs
│   │   └── tests/              # Integration tests (ZenTest harness)
│   ├── zen-core/               # Core infrastructure (13 public modules)
│   │   └── src/
│   │       ├── config.rs       # 5-layer config (Default/embedded/global/workspace/env)
│   │       ├── errors.rs       # ZenError (8 variants), AgenticError (20+ variants)
│   │       ├── paths.rs        # ZenPaths (global_root + workspace_root dual-scope)
│   │       ├── constants.rs    # Directory constants + 13 provider URLs/models
│   │       ├── secrets.rs      # SecretRef (keychain/env resolution)
│   │       ├── types.rs        # Shared types (Sensitivity, AgentTier)
│   │       ├── validate.rs     # Input validation
│   │       ├── sanitize.rs     # Output sanitization
│   │       └── definition.rs   # AgentDefinition
│   ├── zen-service/            # Business logic (starter, wps, cleanup)
│   ├── zen-data/               # Dual API data layer
│   │   └── src/
│   │       ├── pool.rs         # sqlx SqlitePool + create_pool()
│   │       ├── schema.rs       # sqlx migrations (notes, agent_profiles, audit_logs)
│   │       ├── repositories.rs # Trait interfaces (NoteRepository, etc.)
│   │       ├── repo_impl.rs    # sqlx implementations (SqliteNoteRepository)
│   │       ├── sqlite_repo.rs  # rusqlite wrapper + FTS5/vec0/graph schema
│   │       └── models.rs       # Note, AgentProfile, AuditLog entities
│   ├── zen-knowledge/          # 10+ knowledge services
│   │   └── src/
│   │       ├── note.rs         # Note, NoteService, frontmatter parsing
│   │       ├── wiki.rs         # WikiPage, WikiIndex, AtomicWikiWriter
│   │       ├── search/         # 5-tier search (ripgrep → FTS5 → vec0 → graph → LLM)
│   │       ├── consolidate/    # 4-stage pipeline (extract → compile → deduplicate → index)
│   │       ├── entity.rs       # Entity, RelationType, EntityService
│   │       ├── maintenance/    # Linter, Reindexer, LearningLoop
│   │       ├── ingest/         # FeedEntry, RssFetcher, ingest_local_file
│   │       └── intent.rs       # Intent detection
│   ├── zen-agents/             # Agent system (13 agents, 4 tiers)
│   │   └── src/
│   │       ├── registry.rs     # AgentRegistry, DefaultAgentRegistry
│   │       ├── agent_profile.rs # Profile, Role, SensitivityLevel, LlmPreference
│   │       ├── zen_agent.rs    # ZenAgent, IdentityContext, load_identity_files
│   │       ├── orchestrator.rs # AgentOrchestrator
│   │       ├── coordinator.rs  # ZenCoordinator
│   │       ├── blackboard.rs   # Blackboard, Deliverable, Feedback, SystemEvent
│   │       ├── executor.rs     # AgentExecutor, RetryPolicy, ErrorCategory
│   │       ├── execution.rs    # AgentExecution, ToolCall
│   │       ├── review.rs       # QualityPipeline (Metis→Momus→Hermes→Zeus)
│   │       ├── sandbox.rs      # WasmSandbox (wasmtime), ResourceLimits
│   │       └── wiring.rs       # ZenWiring (DI wiring)
│   ├── zen-provider/           # Multi-provider LLM routing
│   │   └── src/
│   │       ├── providers/      # 7 protocol-specific providers (13 named configs)
│   │       ├── router.rs       # DefaultRouter, LlmRouter trait, auth resolution
│   │       ├── chat.rs         # ChatMessage, ChatSession, MessageRole
│   │       ├── model_meta.rs   # ModelMetadata, ModelRouter, routing metrics
│   │       └── stream.rs       # StreamResponse
│   ├── zen-memory/             # Identity context (SOUL.md, MEMORY.md)
│   ├── zen-auth/               # Keychain + SecretRef resolution
│   ├── zen-plugin/             # WASM sandbox + MCP server
│   └── zen-gateway/            # HTTP daemon (axum, placeholder)
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
| Add sqlx model | `crates/zen-data/src/models.rs` + `schema.rs` | Then implement repository trait |
| Add repository trait | `crates/zen-data/src/repositories.rs` | Define async trait interface |
| Implement repository | `crates/zen-data/src/repo_impl.rs` | SqliteXxxRepository impl |
| Modify FTS5 schema | `crates/zen-data/src/sqlite_repo.rs` | init_kb_schema() |
| Modify vec0 schema | `crates/zen-data/src/sqlite_repo.rs` | init_vec_schema() |
| Modify graph schema | `crates/zen-data/src/sqlite_repo.rs` | init_graph_schema() |

### Knowledge Services

| Task | Location | Notes |
|------|----------|-------|
| Add search tier | `crates/zen-knowledge/src/search/tierN.rs` | + register in search/service.rs |
| Modify consolidation | `crates/zen-knowledge/src/consolidate/mod.rs` | 4-stage pipeline |
| Add lint rule | `crates/zen-knowledge/src/maintenance/mod.rs` | Linter trait |
| Note format change | `crates/zen-knowledge/src/note.rs` | frontmatter, Domain, write_note |

### Agent System

| Task | Location | Notes |
|------|----------|-------|
| Register agent | `crates/zen-agents/src/registry.rs` | DefaultAgentRegistry |
| Agent definition | `crates/zen-agents/src/zen_agent.rs` | ZenAgentBuilder |
| Blackboard change | `crates/zen-agents/src/blackboard.rs` | 4-channel shared memory |
| Quality gate | `crates/zen-agents/src/review.rs` | Metis→Momus→Hermes→Zeus pipeline |
| Sandbox extension | `crates/zen-agents/src/sandbox.rs` | WasmSandbox (wasmtime) |

### Provider Routing

| Task | Location | Notes |
|------|----------|-------|
| Add provider | `crates/zen-provider/src/providers/` | impl Provider trait |
| Routing logic | `crates/zen-provider/src/router.rs` | DefaultRouter, LlmRouter |
| Model metadata | `crates/zen-provider/src/model_meta.rs` | ModelRouter, metrics |
| Auth resolution | `crates/zen-provider/src/router.rs` | resolve_api_key(), 4-tier resolution |

## CONVENTIONS

- **Workspace deps**: All shared deps in `[workspace.dependencies]`, inherit via `workspace = true`
- **Error flow**: thiserror for library → anyhow for app → ZenError as top-level; AgenticError auto-categories via ErrorCategory
- **CLI pattern**: `main.rs` (bin) → `zen_cli::shell()` (lib) → clap Parser → `execute_command()` dispatch
- **Config**: 5-layer merge (Default → embedded → global `~/.zen/` → workspace `.zen/` → `ZEN_*` env vars)
- **Path scoping**: ZenPaths dual-scope: workspace root for knowledge (inbox/raw/wiki), global root for system (db/sessions/memory/logs)
- **Tests**: Integration only (no inline `#[cfg(test)]` in most crates). Custom ZenTest/ZenOutput harness.
- **Lint**: `bin/lint` → `-D warnings` + `--allow dead_code`
- **Command files**: Pattern `src/cmd/{name}_command.rs` with `pub fn execute_command(...)` dispatcher (note: correctly spelled now)

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
bin/release patch        # Bump version, tag, push
```

### Agentic Commands (24 commands)

| Command | Description | Dispatch File |
|---------|-------------|---------------|
| (no args) | TUI (ratatui interactive) | `tui/run()` |
| `zen hello <name>` | Greeting | `cli.rs` inline |
| `zen clean` | Cleanup (trash, cache, all) | `cleanup_command.rs` |
| `zen starter` | Dev tools/workspace init | `starter_command.rs` |
| `zen wps` | Work process utilities | `wps_command.rs` |
| `zen version` | Show version | `cli.rs` inline |
| `zen session` | Session lifecycle | `session_command.rs` |
| `zen serve` | Gateway daemon | `serve_command.rs` |
| `zen agent` | Agent registry | `agent_command.rs` |
| `zen workspace` | `.zen/` structure | `workspace_command.rs` |
| `zen config` | Config layers | `config_command.rs` |
| `zen provider` | LLM provider mgmt | `provider_command.rs` |
| `zen audit` | Audit log ops | `audit_command.rs` |
| `zen note` | Create notes | `note_command.rs` |
| `zen search` | KB search (5-tier) | `search_command.rs` |
| `zen similar` | Vector similarity | `similar_command.rs` |
| `zen graph` | Entity graph query | `graph_command.rs` |
| `zen reindex` | Rebuild index | `reindex_command.rs` |
| `zen research` | Research tasks | `research_command.rs` |
| `zen consolidate` | Consolidation pipeline | `consolidate_command.rs` |
| `zen lint` | Knowledge lint | `lint_command.rs` |
| `zen ingest` | Ingest files/feeds | `ingest_command.rs` |
| `zen routine` | Routine management | `routine_command.rs` |
| `zen task` | Task management | `task_command.rs` |
| `zen brief` | Brief generation | `brief_command.rs` |
| `zen plugin` | Plugin management | `plugin_command.rs` |
| `zen auth` | Auth/keychain ops | `auth_command.rs` |

## FRAMEWORK PATTERNS

### clap Derive API (CLI)

- `Parser` derive on `Cli` struct with `#[command(author, version, about)]`
- `Subcommand` derive on `Commands` enum (24 variants)
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
- Embeddings: `vec0` virtual table via sqlite-vec extension (384-dim via ort ONNX)
- Extension loading: `dlopen` for `libsqlite_vec0.dylib` at runtime
- Dual API: sqlx for repository CRUD, rusqlite for search/schema operations

### Agent Quality Pipeline

```
Metis (correctness) → Momus (quality) → Hermes (safety) → Zeus (escalation)
```

## AGENT SYSTEM DETAILS

### 13 Agents

Agents are registered in `DefaultAgentRegistry` across 4 tiers:

| Tier | Role | Example |
|------|------|---------|
| Orchestrator | Session coordination, routing | `ZenCoordinator` |
| Planner | Task planning, decomposition | `AgentOrchestrator` |
| Specialist | Domain expertise (search, consolidate) | Agent-specific |
| Worker | Execution, tool calling | `AgentExecutor` |

### 3-Layer Permission Model

1. **Sensitivity filtering** (`SensitivityLevel`): Private / Internal / Public
2. **Role-based tool gating** (`Role`): controls which tools an agent can invoke
3. **Static assignment**: tool permissions assigned at agent registration

### Session Lifecycle

5 states: `Active → Paused → Archived → Error → Complete`

### Blackboard (4 Channels)

Shared memory between agents: `Deliverable` / `Feedback` / `SystemEvent` / `Task`

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

- Project uses Rust edition 2024 (stable toolchain, MSRV 1.80+)
- Gateway/Data crates are active: zen-data has full sqlx + rusqlite dual API; zen-gateway is still a placeholder
- `docs/specs/001-agentic-foundation/` has extensive architecture docs (~400KB)
- Karpathy guidelines skill installed at `.opencode/skills/karpathy-guidelines/`
- zen-llm exists in directory but is not a workspace member (staged for integration)

## Active Technologies

- Rust edition 2024 (stable toolchain, MSRV 1.80+) + clap 4.5 (CLI derive), tokio 1.47 (async runtime), rusqlite 0.32 (SQLite FTS5 + sqlite-vec), rig-core 0.37 (LLM abstraction), rig-compose 0.4 (agent kernel), rig-sqlite 0.2 (vector store), rig-tap 0.1 (observability), rig-mcp 0.2 (MCP bridge), jento-core/jento-context (DI + plugin lifecycle), rmcp 0.1 (MCP server), wasmtime 24 (WASM sandbox), security-framework 3 (macOS Keychain), serde/serde_json 1.0, tera (template engine), include_dir (embedded templates), ratatui 0.30 + crossterm 0.28 (TUI), axum 0.8 (gateway), sqlx 0.8 (async SQLite), ort 2.0 (ONNX runtime for embeddings)
- SQLite for derived indexes (FTS5, vector embeddings via sqlite-vec, entity graph, habits, finance), Markdown files as canonical source of truth, TOML for config (config.toml), habits (habits.toml), goals (goals.toml), budgets (budgets.toml), routines (routines.toml)
- Binary/library split: `zen` binary (13 lines) → `zen-cli` library (exporting `shell()`)

## Recent Changes

- Binary/library separation: zen (bin) + zen-cli (lib) architecture documented
- 24 CLI commands documented with dispatch file paths
- 5-layer config inheritance model (Default → embedded → global → workspace → env)
- Dual API data layer: sqlx (repository CRUD) + rusulite (FTS5 + vec0 schema)
- 5-tier search pipeline: ripgrep → FTS5 → vec0 embeddings → entity graph → LLM
- Provider routing: 13 named providers across 3 protocol types (rig-native, openai-compatible, anthropic-compatible)
- Agent system: 13 agents in 4 tiers, 3-layer permissions, 4-channel blackboard, QualityPipeline
- Corrected `excute_command` → `execute_command` across all 24 command files
- Documented framework patterns: clap derive, rig-core Client/CompletionModel, FTS5 schema
- **Architecture remediation (2026-06-01)**:
  - Unified LlmPreference: zen-core defines with Serialize+Hash+Display, zen-agents re-exports (A3 resolved)
  - Renamed zen-agents SensitivityLevel → AgentClearance (distinguishes agent permissions)
  - Renamed zen-core/validate SensitivityLevel → SafetyLevel (distinguishes validation results)
  - Deleted zen-llm directory (legacy subset, no consumers)
  - Deleted rig_ollama.rs/rig_openai.rs stubs (T229/T230 legacy)
  - Constitution principle XI added: Design-First & Reuse Priority
