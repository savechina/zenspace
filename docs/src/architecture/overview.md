# System Architecture

Zen is a **Rust CLI productivity suite** with agentic workspace architecture. It follows a binary/library split with 12 workspace crates.

## High-Level Architecture

```
┌─────────────────────────────────────────────┐
│  zen (binary, 13 lines)                     │
│  loads .env, calls zen_cli::shell()         │
└──────────────────┬──────────────────────────┘
                   │
┌──────────────────▼──────────────────────────┐
│  zen-cli (library)                           │
│  clap Parser (29 commands), TUI (ratatui)   │
│  command dispatcher                          │
└──┬────┬────┬────┬────┬────┬────┬────┬───────┘
   │    │    │    │    │    │    │    │
   ▼    ▼    ▼    ▼    ▼    ▼    ▼    ▼
┌────┬────┬────┬────┬────┬────┬────┬──────┐
│zen │zen │zen │zen │zen │zen │zen │zen   │
│core│repo│vault│agents│mem │prov│auth│...  │
└────┴────┴────┴────┴────┴────┴────┴──────┘
```

## Crates Overview

| Crate | Role | Key Modules |
|-------|------|-------------|
| **zen** | Binary entry (13-line main.rs) | `.env` loading, dispatches to `zen_cli::shell()` |
| **zen-cli** | CLI library | 29 commands, TUI (ratatui), clap derive dispatch |
| **zen-core** | Core infrastructure | 5-layer config, error taxonomy, path scoping, constants, secrets |
| **zen-service** | Business logic | Starter/wps/cleanup services |
| **zen-repo** | Data layer | sqlx + rusqlite dual API, FTS5, vec0, graph schema |
| **zen-vault** | Knowledge services | Note, Wiki, 5-tier search, consolidation, lint, ingest |
| **zen-agents** | Agent system | 13 agents, 4 tiers, blackboard, QualityPipeline, registry |
| **zen-provider** | LLM routing | 13 providers, 3 protocol types, DefaultRouter, auth resolution |
| **zen-memory** | Identity context | SOUL.md, MEMORY.md loading and management |
| **zen-auth** | Credential management | Keychain integration, SecretRef resolution |
| **zen-plugin** | Extension system | WASM sandbox (wasmtime), MCP server |
| **zen-gateway** | HTTP daemon | Axum-based HTTP server (placeholder) |

## Key Architectural Patterns

### Binary/Library Split

The `zen` binary (13 lines in `crates/zen/src/main.rs`) loads `.env`, calls `zen_core::config::load_config()`, then delegates to `zen_cli::shell().await`. All logic lives in library crates.

### 5-Layer Configuration

```
Embedded defaults → ~/.zen/config.toml → .zen/config.toml → ZEN_* env vars
```

Each layer merges into the previous one — higher layers override only the keys they set.

### 4-Tier Agent Architecture

```
Orchestrator (L0) → Planner (L1) → Specialist (L2) → Worker (L3)
```

- **Orchestrator**: Session coordination, routing (ZenCoordinator)
- **Planner**: Task decomposition, planning (AgentOrchestrator, Prometheus)
- **Specialist**: Domain expertise (search, consolidate, research)
- **Worker**: Execution, tool calling (AgentExecutor)

### 5-Tier Search Pipeline

```
ripgrep → FTS5 → vector embeddings → entity graph → LLM
```

Each tier adds depth: keyword search first, semantic when needed.

### 13 Built-In Agents

| Agent | Tier | Role |
|-------|------|------|
| Sisyphus | L0 | Lead orchestrator |
| Prometheus | L1 | Task planner |
| Metis | L1 | Plan correctness review |
| Momus | L1 | Plan quality review |
| Zeus | L1 | Escalation handler |
| Oracle | L2 | Architecture consultation |
| Explore | L2 | Codebase exploration |
| Librarian | L2 | External reference search |
| Argus | L2 | Monitoring & observation |
| Hephaestus | L3 | Tool execution |
| Atlas | L3 | Knowledge pipeline |
| Junior | L3 | General task execution |
| Hermes | L3 | Safety & audit |

### Data Flow: Note Creation to Wiki

```
zen note create → zen-vault (NoteService) → Markdown file in inbox/
                                        → zen-repo (sqlx insert)
                                        
zen consolidate → zen-vault (ConsolidationPipeline)
                → zen-provider (entity extraction)
                → zen-repo (graph entities)
                → zen-vault (WikiPage generation)
                
zen search run → zen-vault (SearchService)
               → tier 1: ripgrep
               → tier 2: FTS5 (notes_fts)
               → tier 3: vec0 embeddings
               → tier 4: entity graph
               → tier 5: LLM semantic reranking
```

---

**Learn more:** [AGENTS.md](https://github.com/savechina/zenspace/blob/main/AGENTS.md) for the full project knowledge base.
