# Quick Start

## 1. Initialize a Workspace

```bash
zen workspace init
```

This sets up `~/.zen/` with the default directory structure and embedded configuration.

## 2. Create Your First Note

```bash
zen note create "Meeting Notes: Q3 Planning" --tag project
```

Notes are stored as Markdown in `~/.zen/vault/inbox/` with YAML frontmatter.

## 3. Search Your Knowledge Base

```bash
zen search run "Q3 planning"
```

Zen searches across 5 tiers: ripgrep → FTS5 → vector embeddings → entity graph → LLM.

## 4. View Your Configuration

```bash
zen config show
```

Shows the merged configuration from all 5 layers.

## 5. Explore More Commands

```bash
zen --help
```

Or dive into the [CLI Commands](cli-commands.md) reference.

## Next Steps

- [Configure LLM providers](configuration/providers.md) to unlock AI features
- [Set up model routing](configuration/agent-routing.md) for agent tasks
- Explore the [CLI reference](cli-commands.md) for all available commands
