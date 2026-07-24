# Development

## Prerequisites

- Rust 1.80+ (edition 2024)
- macOS (primary development platform)

## Build

```bash
# Build all crates
cargo build

# Build release binary
bin/build
```

## Test

```bash
# Run all tests
cargo test

# Run with nextest (recommended for env-isolated tests)
cargo nextest run

# Run specific crate tests
cargo test -p zen-core
cargo test -p zen-provider
cargo test -p zen-agents
```

> **Note:** Some tests (particularly path tests) require `cargo nextest` for process isolation because they modify environment variables.

## Lint & Format

```bash
# Run linter
bin/lint

# Or individually:
cargo clippy -- -D warnings
cargo fmt --all --check

# Format code
cargo fmt --all
```

## Project Structure

```
zenspace/
├── crates/               # 12 Rust workspace crates
│   ├── zen/              # Binary entry (13-line main.rs)
│   ├── zen-cli/          # CLI library + TUI
│   ├── zen-core/         # Core infrastructure (13 modules)
│   ├── zen-service/      # Business logic
│   ├── zen-repo/         # Data layer (sqlx + rusqlite)
│   ├── zen-vault/        # Knowledge services
│   ├── zen-agents/       # Agent system
│   ├── zen-provider/     # LLM provider routing
│   ├── zen-memory/       # Identity context
│   ├── zen-auth/         # Credential management
│   ├── zen-plugin/       # WASM sandbox + MCP
│   └── zen-gateway/      # HTTP daemon
├── config/               # Embedded config.toml
├── templates/            # Tera templates
├── docs/                 # Documentation
│   ├── src/              # User guide (mdBook source)
│   │   ├── introduction.md
│   │   ├── installation.md
│   │   ├── quickstart.md
│   │   └── ...
│   ├── theme/            # mdBook custom theme
│   ├── book/             # Generated HTML output
│   └── specs/            # Architecture specifications
└── bin/                  # Build/test/lint/release scripts
```

## Release

```bash
# Automated release
bin/release patch    # 0.1.0 → 0.1.1
bin/release minor    # 0.1.0 → 0.2.0
bin/release major    # 0.1.0 → 1.0.0

# Manual release
echo "0.1.2" > VERSION
git commit -am "release: v0.1.2"
git tag v0.1.2
git push origin main --tags
```

## Code Conventions

- **Imports**: `use crate::` for internal, `use zen_*::` for cross-crate
- **Errors**: `thiserror` for libraries, `anyhow` for app-level
- **CLI commands**: Each command in `src/cmd/{name}_command.rs` with `pub fn execute_command()`
- **Config**: Embedded `config/config.toml` + user `~/.zen/config.toml`
- **Tests**: Integration tests only (no inline `#[cfg(test)]` in most crates)

## Design Principles

See [AGENTS.md](https://github.com/savechina/zenspace/blob/main/AGENTS.md) for the full project constitution:

1. **CLI-First** — Every feature via CLI subcommands
2. **Robust Error Handling** — `thiserror` for types, `anyhow` for propagation
3. **Observability** — Structured logging via `tracing`
4. **Configuration** — `.env` via `dotenvy`, 5-layer config inheritance
5. **Template-Driven** — Embedded templates via `include_dir`
6. **Code Quality** — Zero warnings, `unsafe` blocks justified
7. **Single Responsibility** — 12 crates with clear boundaries
8. **Testing** — Unit + integration tests required
9. **UX Consistency** — JSON/human-readable dual output
10. **Performance** — <500ms cold start, <50MB footprint
11. **Design-First & Reuse** — Reuse frameworks, don't reinvent
