# Directory Structure

## Zen Home Directory (~/.zen/)

```
~/.zen/
├── config.toml              # User-level config (overrides embedded)
├── knowledge/
│   ├── inbox/               # New notes land here before consolidation
│   ├── wiki/                # Curated wiki pages
│   │   ├── sources/         # Source references
│   │   ├── entities/        # Extracted entities
│   │   ├── concepts/        # Concept pages
│   │   ├── coding/          # Code-related wiki pages
│   │   ├── research/        # Research notes
│   │   ├── reports/         # Generated reports
│   │   ├── index.md         # Wiki index
│   │   └── log.md           # Wiki change log
│   └── raw/                 # Ingested raw files
├── sessions/                # Session data (active + archived)
├── skills/                  # Agent skill definitions
├── db/
│   ├── kb.db                # Knowledge base (SQLite)
│   ├── vec.db               # Vector index (SQLite)
│   └── graph.db             # Entity graph (stub)
├── logs/                    # Application logs
└── daemon.pid               # Gateway daemon PID file
```

## Workspace Directory (.zen/ in project root)

Each project can have its own `.zen/` directory for workspace-local config and knowledge:

```
.project/
└── .zen/
    ├── config.toml          # Workspace-level config
    ├── knowledge/           # Workspace-local knowledge
    │   ├── inbox/
    │   ├── raw/
    │   └── wiki/
    ├── sessions/            # Workspace-local sessions
    ├── skills/              # Workspace-local skill definitions
    ├── finance/             # Workspace finance data
    └── memory/              # Workspace-specific memory
```

## Crate Layout

```
zenspace/
├── crates/
│   ├── zen-cli/src/
│   │   ├── main.rs          # Entry point
│   │   └── cmd/             # 18 command modules
│   │       ├── note.rs
│   │       ├── search_command.rs
│   │       ├── session.rs
│   │       ├── agent.rs
│   │       ├── consolidate_command.rs
│   │       └── ... (13 more)
│   ├── zen-core/src/
│   │   ├── config.rs        # Config loading + layers
│   │   ├── errors.rs        # ZenError, ServiceError, AgenticError
│   │   ├── paths.rs         # ZenPaths (global + workspace detection)
│   │   └── constants.rs     # APP_NAME, VERSION, etc.
│   ├── zen-vault/src/
│   │   ├── note.rs          # NoteService (create notes)
│   │   ├── search.rs        # SearchService (tier-aware search)
│   │   ├── consolidation.rs # ConsolidationPipeline
│   │   ├── linter.rs        # Linter (orphans, broken wikilinks)
│   │   ├── ingester.rs      # SourceIngester
│   │   └── wiki.rs          # Wiki page management
│   ├── zen-repo/src/
│   │   ├── entity.rs        # SQLite entities (Note, Session, etc.)
│   │   └── repository.rs    # SQLite repositories
│   ├── zen-memory/src/
│   │   └── ...              # SOUL.md, MEMORY.md templates
│   ├── zen-agents/src/
│   │   └── ...              # Agent registry, tool permissions
│   ├── zen-provider/src/
│   │   ├── lib.rs           # LLM routing interface
│   │   └── router.rs        # Provider selection logic
│   └── zen-gateway/src/
│       ├── lib.rs           # HttpGateway stub
│       └── config.rs        # HttpConfig
```
