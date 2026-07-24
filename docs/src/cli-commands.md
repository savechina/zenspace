# CLI Commands Reference

Zen provides 29 CLI commands for knowledge management, agent orchestration, and system administration.

## General

| Command | Description |
|---------|-------------|
| `zen` | Launch TUI (interactive interface) |
| `zen version` | Show version information |
| `zen --help` | Show full help with all commands |

## Workspace & Configuration

| Command | Description |
|---------|-------------|
| `zen workspace init` | Initialize `.zen/` workspace structure |
| `zen config show` | Show effective configuration (all layers merged) |
| `zen provider list` | List available LLM providers |
| `zen provider test <name>` | Test a provider connection |
| `zen auth list` | List stored credentials |

## Knowledge Management

| Command | Description |
|---------|-------------|
| `zen note create <title>` | Create a new note |
| `zen search run <query>` | Search knowledge base (5 tiers) |
| `zen similar find <id>` | Find similar notes by vector similarity |
| `zen notion query <entity>` | Query entity graph |
| `zen wiki list` | List wiki pages |
| `zen wiki show <id>` | Show a wiki page |
| `zen wiki reindex` | Reindex wiki pages |
| `zen wiki lint` | Lint wiki (orphan pages, broken wikilinks) |
| `zen wiki distill` | Distill wiki content |
| `zen ingest <path>` | Ingest files or RSS feeds |
| `zen brief generate` | Generate a brief from recent notes |

## Agentic Sessions

| Command | Description |
|---------|-------------|
| `zen session start` | Start an agentic session |
| `zen session list` | List active sessions |
| `zen session stop <id>` | Stop a session |
| `zen chat` | Interactive LLM chat |
| `zen research <topic>` | Run research task |
| `zen agent list` | List available agents |
| `zen dispatch run <task>` | Dispatch a task to agents |
| `zen dispatch status <id>` | Check task status |
| `zen dispatch list` | List dispatched tasks |
| `zen dispatch cancel <id>` | Cancel a task |

## System

| Command | Description |
|---------|-------------|
| `zen serve` | Start HTTP gateway daemon |
| `zen logs <service>` | View structured logs |
| `zen clean <target>` | Clean up (trash, cache, all) |
| `zen starter <template>` | Generate project scaffold from template |
| `zen wps <action>` | Work process utilities |

## Habit & Goal Tracking

| Command | Description |
|---------|-------------|
| `zen habit log` | Log a habit entry |
| `zen habit list` | List habits |
| `zen goal create` | Create a goal |
| `zen goal list` | List goals |
| `zen goal status <id>` | Check goal progress |

## Plugin Management

| Command | Description |
|---------|-------------|
| `zen plugin list` | List installed plugins |
| `zen plugin install <id>` | Install a plugin |
| `zen plugin remove <id>` | Remove a plugin |

## Quick Reference

```bash
# Initialize
zen workspace init
zen config show

# Daily workflow
zen note create "Daily Log" --tag journal
zen search run "yesterday's decisions"
zen wiki reindex
zen wiki lint

# Agentic work
zen session start
zen research "Rust async patterns"
zen dispatch run "summarize inbox"

# Maintenance
zen clean cache
zen logs agent
```

---

**Full reference:** `zen --help` for the most up-to-date list of commands and flags.
