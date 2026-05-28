# Zen Space (Zen)

**Zen** is a Rust CLI productivity suite with agentic workspace architecture. Knowledge-base-first design with LLM routing, vector search, note management, and session lifecycle.

## Features

- **Knowledge base** — Notes, wikis, and search with tier-aware indexing
- **Agentic sessions** — Start sessions with agents, track status, archive history
- **Consolidation pipeline** — Auto-extract entities from notes into wiki pages
- **Multi-provider LLM** — Route tasks across providers (entity extraction, synthesis, dispatch)
- **Vector similarity** — Find similar notes via embeddings (stub)
- **Entity graph** — Query relationships between extracted entities (stub)
- **Knowledge lint** — Detect orphan pages, broken wikilinks, stale claims
- **Audit logging** — Full lifecycle audit with export and integrity verification
- **Gateway daemon** — HTTP daemon for API access (stub)

## Quick Start

```bash
# Build from source
git clone https://github.com/savechina/zenspace.git
cd zenspace
cargo build --release
./target/release/zen --help

# Create a note
./target/release/zen note create "my first note" --tag work

# Search the knowledge base
./target/release/zen search run "first note"
```

## Commands

| Command | Subcommands | Description |
| ------- | ----------- | ----------- |
| `zen` | — | Launch Agentic TUI (planned) |
| `zen version` | — | Show version |
| `zen session` | `start`, `status`, `list`, `archive` | Session lifecycle with agents |
| `zen agent` | `list`, `select`, `configure` | Agent registry management |
| `zen workspace` | `init`, `status`, `cleanup` | `.zen/` directory structure |
| `zen config` | `show`, `edit`, `validate` | Config layers (workspace/global/embedded) |
| `zen llm` | `route`, `test`, `providers` | LLM routing + connectivity |
| `zen audit` | `log`, `export`, `verify` | Audit log operations |
| `zen serve` | `start`, `stop`, `status` | Gateway daemon control |
| `zen note` | `create` | Create notes with tags |
| `zen search` | `run` | Search knowledge base |
| `zen similar` | `find` | Vector similarity search (stub) |
| `zen graph` | `query` | Entity graph query (stub) |
| `zen consolidate` | `run` | Run consolidation pipeline |
| `zen reindex` | `run` | Rebuild knowledge index |
| `zen lint` | `run` | Knowledge lint (orphan pages, broken wikilinks) |
| `zen ingest` | `run` | Ingest files into raw knowledge directory |
| `zen starter` | `develop`, `workspace` | Initialize dev tools/workspace |
| `zen wps` | `archive`, `dotfiles`, `unixtime` | Work process utilities |
| `zen clean` | `all`, `trash`, `cache` | Clean up system artifacts |

## Project Structure

```
zenspace/
├── crates/               # Rust workspace (10 crates, agentic architecture)
│   ├── zen-cli/          # CLI entry point + 18 commands
│   ├── zen-core/         # Config, errors, paths, constants
│   ├── zen-service/      # Starter/wps/cleanup business logic
│   ├── zen-data/         # SQLite entities + repositories
│   ├── zen-knowledge/    # Note, wiki, search, consolidation, lint
│   ├── zen-memory/       # Identity context (SOUL.md, MEMORY.md)
│   ├── zen-agents/       # Agent registry + tool permissions
│   ├── zen-auth/         # Keychain + credential resolution
│   ├── zen-provider/          # Multi-provider LLM routing
│   └── zen-gateway/      # HTTP daemon (stub)
├── bin/                  # Helper scripts (build, test, lint, release)
├── config/               # Embedded config.toml
├── docs/specs/           # Agentic foundation specs (~400KB)
├── templates/            # Tera templates (empty)
├── Cargo.toml            # Workspace manifest
└── README.md             # This file
```

## Development

### Rust Development

```bash
# Build all crates
cargo build

# Run integration tests
cargo test

# Run linter (fmt check + clippy)
bin/lint

# Format code
cargo fmt --all

# Run binary locally
cargo run --bin zen -- --help
```

### Ruby Development

Ruby implementation not yet available in this workspace.

## CI/CD

CI/CD pipelines not yet configured. Planned automation:
- **Rust**: Build, test, lint (Clippy), publish to crates.io
- **Release**: Cross-platform binaries via `bin/release` script

## Release Process

### Semantic Versioning

This project follows [Semantic Versioning](https://semver.org/) (MAJOR.MINOR.PATCH):
- **MAJOR**: Breaking changes
- **MINOR**: New features (backward compatible)
- **PATCH**: Bug fixes (backward compatible)

### Automated Release Workflow

**Note**: Create `VERSION` file first before using release script.

When you push a version tag (e.g., `v0.1.2`), the planned CI will:

1. ✅ Builds binaries for all platforms:
   - Linux x86_64 & ARM64
   - macOS x86_64 & ARM64 (Apple Silicon)
   - Windows x86_64
2. ✅ Packages binaries as `.tar.gz` (Unix) or `.zip` (Windows)
3. ✅ Runs full test suite on all platforms
4. ✅ Creates GitHub Release with changelog and assets
5. ✅ Publishes to crates.io

### Create a Release

**Option 1: Using the release helper script (Recommended)**

```bash
# Auto-increment patch version (0.1.0 → 0.1.1)
./bin/release patch

# Auto-increment minor version (0.1.1 → 0.2.0)
./bin/release minor

# Auto-increment major version (0.1.1 → 1.0.0)
./bin/release major

# Use specific version
./bin/release 0.2.0

# Dry run (test without committing)
./bin/release patch dry-run
```

**Option 2: Manual release**

```bash
# 1. Update VERSION file
echo "0.1.2" > VERSION

# 2. Update Cargo.toml version
sed -i.bak 's/^version = "[^"]*"/version = "0.1.2"/' Cargo.toml

# 3. Commit and tag
git add VERSION Cargo.toml
git commit -m "release: v0.1.2"
git tag "v0.1.2"

# 4. Push to trigger release
git push origin main
git push origin "v0.1.2"
```

### Monitor Release

After pushing a tag, monitor the release progress:

```bash
# View GitHub Actions
open https://github.com/savechina/zenspace/actions

# Or use GitHub CLI
gh run watch
```

### Release Assets

Each release includes:
- `zen-{version}-x86_64-unknown-linux-gnu.tar.gz` — Linux x86_64
- `zen-{version}-aarch64-unknown-linux-gnu.tar.gz` — Linux ARM64
- `zen-{version}-x86_64-apple-darwin.tar.gz` — macOS Intel
- `zen-{version}-aarch64-apple-darwin.tar.gz` — macOS Apple Silicon
- `zen-{version}-x86_64-pc-windows-msvc.zip` — Windows x86_64

### Installing from Release

```bash
# Linux/macOS
curl -L https://github.com/savechina/zenspace/releases/latest/download/zen-$(uname -m)-unknown-linux-gnu.tar.gz | tar xz
sudo mv zen /usr/local/bin/

# macOS (Homebrew - coming soon)
# brew install zen

# Windows
# Download .zip from releases page and add to PATH
```

### Prerequisites

Before releasing, ensure you have:
- Write access to the repository
- `VERSION` file exists with current version
- Clean git working directory
- All tests passing locally (`cargo test`)

## License

MIT License — see [LICENSE.txt](LICENSE.txt)

## Contributing

Bug reports and pull requests are welcome on GitHub at https://github.com/savechina/zenspace

## Code of Conduct

Everyone interacting in the Zen project's codebases, issue trackers, chat rooms and mailing lists is expected to follow the [code of conduct](CODE_OF_CONDUCT.md).

---

## Detailed Documentation

- [AGENTS.md](AGENTS.md) — Project architecture guide
- [crates/AGENTS.md](crates/AGENTS.md) — Workspace crate details
- [docs/specs/AGENTS.md](docs/specs/AGENTS.md) — Specification overview
