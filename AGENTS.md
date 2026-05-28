# PROJECT KNOWLEDGE BASE

**Generated:** 2026-05-23
**Commit:** workspace
**Branch:** main

## OVERVIEW

Zen is a Rust CLI productivity suite with agentic workspace architecture. Edition 2024, 10 crates, knowledge-base-first design with LLM routing, vector search, and session management.

## AGENTIC ARCHITECTURE

Zen routes operations through a layered agentic pipeline: notes → consolidation → knowledge graph → search. Each crate handles one concern.

### Crates

| Crate | Description | Path |
|-------|-------------|------|
| zen-cli | CLI entry point + 18 commands | `crates/zen-cli/` |
| zen-core | Config, errors, paths, constants | `crates/zen-core/` |
| zen-service | Starter/wps/cleanup business logic | `crates/zen-service/` |
| zen-data | SQLite entities + repositories | `crates/zen-data/` |
| zen-knowledge | Note, wiki, search, consolidation, lint | `crates/zen-knowledge/` |
| zen-memory | Identity context (SOUL.md, MEMORY.md) | `crates/zen-memory/` |
| zen-auth | Keychain + credential resolution | `crates/zen-auth/` |
| zen-agents | Agent registry + tool permissions | `crates/zen-agents/` |
| zen-provider | Multi-provider LLM routing | `crates/zen-provider/` |
| zen-gateway | HTTP daemon (stub) | `crates/zen-gateway/` |

### Dependency Graph

```
zen-cli
 ├── zen-service → zen-core
 ├── zen-gateway → zen-core
 ├── zen-knowledge → zen-data → zen-core
 ├── zen-core
 └── (direct deps: clap, colored, uuid, chrono, serde)

zen-agents → zen-memory → zen-core
zen-auth   → zen-core
zen-provider    → zen-core
```

### Data Flow

```
zen note create → zen-knowledge (note service) → zen-data (SQLite)
zen ingest      → zen-knowledge (raw directory) → zen-knowledge (ingester)
zen consolidate → zen-knowledge (pipeline) → zen-provider (entity extraction)
zen search      → zen-knowledge (search service)
zen similar     → zen-knowledge → zen-provider (embeddings, stub)
zen graph query → zen-knowledge → zen-data (graph, stub)
```

## STRUCTURE

```
zenspace/
├── crates/               # 10 workspace crates (agentic architecture)
│   ├── zen-cli/          # CLI entry + 18 commands
│   ├── zen-core/         # Config, errors, paths, constants
│   ├── zen-service/      # Starter/wps/cleanup business logic
│   ├── zen-data/         # SQLite entities + repositories
│   ├── zen-knowledge/    # Note, wiki, search, consolidation, lint
│   ├── zen-memory/       # Identity context (SOUL.md, MEMORY.md)
│   ├── zen-agents/       # Agent registry + tool permissions
│   ├── zen-auth/         # Keychain + credential resolution
│   ├── zen-provider/          # Multi-provider LLM routing
│   └── zen-gateway/      # HTTP daemon (stub)
├── bin/                  # build, test, lint, release scripts
├── config/               # Embedded config.toml
├── docs/specs/           # Agentic foundation specs
└── templates/            # Tera templates (empty)
```

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| Add CLI command | `crates/zen-cli/src/cmd/` | Create `_command.rs`, add to mod.rs, wire in cli.rs |
| Modify config | `crates/zen-core/src/config.rs` + `config/config.toml` | Embedded + user override |
| Add service logic | `crates/zen-service/src/` | Create `_service.rs`, export in lib.rs |
| Add error type | `crates/zen-core/src/errors.rs` | ZenError/ServiceError/AgenticError enums |
| Integration tests | `crates/zen-cli/tests/` | ZenTest harness + ZenOutput helpers |
| Release process | `bin/release` + VERSION file | Semantic versioning, auto-tag |

## CONVENTIONS

- **Workspace deps**: All shared deps in `[workspace.dependencies]`, inherit via `workspace = true`
- **Error flow**: thiserror for library → anyhow for app → ZenError as top-level
- **CLI pattern**: main.rs delegates to `cli::shell()` → Clap Parser → cmd dispatch
- **Config**: Embedded `config.toml` + `~/.zen/config.toml` overlay + `ZEN_*` env vars
- **Tests**: Integration only (no inline `#[cfg(test)]`). Custom ZenTest/ZenOutput harness.
- **Lint**: `bin/lint` → `-D warnings` + `--allow dead_code`

## ANTI-PATTERNS (THIS PROJECT)

- **Typos**: `excute_command` → should be `execute_command` (all cmd files)
- **Tests/main.rs**: Unusual pattern — only declares modules, no `#[test]` functions
- **No VERSION**: Release script expects VERSION file but it's missing
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

### Agentic Commands

| Command | Subcommands | Notes |
|---------|-------------|-------|
| `zen version` | — | Show version |
| `zen session` | `start`, `status`, `list`, `archive` | Session lifecycle with agents |
| `zen agent` | `list`, `select`, `configure` | Agent registry management |
| `zen workspace` | `init`, `status`, `cleanup` | `.zen/` directory structure |
| `zen config` | `show`, `edit`, `validate` | Config layers (workspace/global/embedded) |
| `zen llm` | `route`, `test`, `providers` | LLM routing + connectivity |
| `zen audit` | `log`, `export`, `verify` | Audit log operations |
| `zen serve` | `start`, `stop`, `status` | Gateway daemon control |
| `zen note` | `create` | Create notes with tags |
| `zen search` | `run` | Search knowledge base (tier-aware) |
| `zen similar` | `find` | Vector similarity search (stub) |
| `zen graph` | `query` | Entity graph query (stub) |
| `zen reindex` | `run` | Rebuild knowledge index |
| `zen consolidate` | `run` | Run consolidation pipeline |
| `zen lint` | `run` | Knowledge lint (orphan pages, broken wikilinks) |
| `zen ingest` | `run` | Ingest files into raw knowledge directory |
| `zen starter` | `develop`, `workspace` | Dev tools/workspace init |
| `zen wps` | `archive`, `dotfiles`, `unixtime` | Work process utilities |
| `zen clean` | `all`, `trash`, `cache` | Clean up system artifacts |

## NOTES

- Project uses Rust edition 2024 (latest)
- Gateway/Data/LLM crates are placeholders (2 files each)
- `docs/specs/001-agentic-foundation/` has extensive architecture docs (~400KB)
- Karpathy guidelines skill installed at `.opencode/skills/karpathy-guidelines/`

## Active Technologies
- Rust edition 2024 (stable toolchain, MSRV 1.80+) + clap 4.5 (CLI), tokio 1.47 (async runtime), rusqlite 0.31 (SQLite FTS5 + sqlite-vec), rig-core 0.37 (LLM abstraction), jento-core/jento-context (DI + plugin lifecycle), rmcp 0.1 (MCP server), wasmtime 24 (WASM sandbox), security-framework 3 (macOS Keychain), serde/serde_json 1.0, tera (template engine), include_dir (embedded templates) (001-agentic-foundation)
- SQLite for derived indexes (FTS5, vector embeddings, entity graph, habits, finance), Markdown files as canonical source of truth, TOML for config (config.toml), habits (habits.toml), goals (goals.toml), budgets (budgets.toml), routines (routines.toml) (001-agentic-foundation)

## Recent Changes
- 001-agentic-foundation: Added Rust edition 2024 (stable toolchain, MSRV 1.80+) + clap 4.5 (CLI), tokio 1.47 (async runtime), rusqlite 0.31 (SQLite FTS5 + sqlite-vec), rig-core 0.37 (LLM abstraction), jento-core/jento-context (DI + plugin lifecycle), rmcp 0.1 (MCP server), wasmtime 24 (WASM sandbox), security-framework 3 (macOS Keychain), serde/serde_json 1.0, tera (template engine), include_dir (embedded templates)
