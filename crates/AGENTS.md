# CRATES WORKSPACE GUIDE

**Scope:** 6 Rust crates under DDD-inspired architecture

## ARCHITECTURE

```
crates/
├── zen-cli/       # Application layer (binary)
├── zen-core/      # Domain primitives
├── zen-service/   # Business logic
├── zen-gateway/   # API gateway (placeholder)
├── zen-data/      # Data layer (placeholder)
└── zen-provider/       # LLM transport (placeholder)
```

## DEPENDENCY GRAPH

```
zen-cli → zen-service → zen-gateway → zen-data
    ↓           ↓            ↓
zen-core ← zen-core ← zen-core
```

## WHERE TO LOOK

| What | Crate | Path |
|------|-------|------|
| CLI commands | zen-cli | `src/cmd/*_command.rs` |
| Config loading | zen-core | `src/config.rs` |
| Error types | zen-core | `src/errors.rs` (ZenError, ServiceError, AgenticError) |
| Business logic | zen-service | `src/*_service.rs` |
| Integration tests | zen-cli | `tests/` (ZenTest harness) |

## CRATE DETAILS

### zen-cli (Binary)
- Entry: `main.rs` → `cli::shell()` → Clap dispatch
- Commands: starter, wps, clean, version
- Tests: `tests/common.rs` (ZenTest/ZenOutput harness)
- Pattern: Each command in `src/cmd/*_command.rs`

### zen-core (Library)
- Modules: config, errors, paths, constants
- Config: Embedded `config/config.toml` + user `~/.zen/config.toml`
- Errors: `ZenError` (top), `ServiceError`, `AgenticError` (LLM/knowledge)

### zen-service (Library)
- Services: starter, wps, cleanup + utils
- Dependencies: tera (templates), typed-builder, strum

### zen-gateway (Placeholder)
- Current: 27-line stub (`Gateway::start/stop`)
- Planned: Async API gateway for HTTP/WebSocket

### zen-data (Placeholder)
- Current: 3 files (lib.rs, entity.rs, starter_repository.rs)
- Planned: SQLx repositories, entity definitions

### zen-provider (Placeholder)
- Current: 2 files (lib.rs, router.rs)
- Planned: Multi-provider LLM routing (rig-core)

## CONVENTIONS

- **Imports**: `use crate::` for internal, `use zen_*::` for cross-crate
- **Errors**: Return `Result<T, ZenError>` from CLI, `Result<T, ServiceError>` from service
- **Commands**: Match in `cli.rs`, dispatch to `cmd::execute_command()` (note typo)
- **Tests**: Use `ZenTest::new()` for isolated env, `assert!(output.success())`

## ANTI-PATTERNS

- `excute_command` typo in all cmd files (should be `execute`)
- No `lib.rs` in zen-cli (binary-only)
- Gateway/Data/LLM are stubs — don't expect real implementations