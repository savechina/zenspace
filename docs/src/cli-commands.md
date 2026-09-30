# CLI Commands Reference

Zen's CLI surface is intentionally small: the TUI plus 23 subcommands. Heavy capabilities (search, research, dispatch, briefs) run autonomously through the background scheduler and distill loop rather than as manual commands — drop notes into `~/.zen/vault/inbox/` and the system works on them.

## General

| Command | Description |
|---------|-------------|
| `zen` | Launch TUI (interactive agent session; first run initializes `~/.zen/`) |
| `zen version` | Show version information |
| `zen --help` | Show full help with all commands |

## Chat & Sessions

| Command | Description |
|---------|-------------|
| `zen chat` | Interactive LLM chat (non-TUI) |
| `zen session start` | Start an agentic session |
| `zen session status` | Show session status |
| `zen session list` | List sessions |
| `zen session archive` | Archive a session |

## Knowledge Pipeline

| Command | Description |
|---------|-------------|
| `zen wiki list` | List wiki pages |
| `zen wiki show <page>` | Show a wiki page |
| `zen wiki reindex` | Rebuild the knowledge index (FTS5 + embeddings; also heals legacy non-NFC alias rows; `--dry-run` preview, `--fts-only` resync) |
| `zen wiki lint` | Lint wiki (orphan pages, broken links, stale claims) |
| `zen wiki distill` | Run the consolidation pipeline (inbox → wiki) |
| `zen wiki rebuild-memory` | Rebuild the gateway memory store from Markdown + session archives (requires running daemon) |
| `zen wiki loop run` | Run one self-learning consolidation cycle now |
| `zen wiki loop status` | Loop status (`--json` includes latency metrics) |
| `zen wiki loop gaps` | Show pending gaps and user questions |
| `zen wiki loop enable` / `disable` | Toggle the background loop |

## Agents, Skills & Self-Learning

| Command | Description |
|---------|-------------|
| `zen agent list` | List registered agents |
| `zen skill list --json` | List learned skills |
| `zen skill run <name>` | Run a skill |
| `zen skill progress` | Show skill learning progress |
| `zen skill show <name>` | Show a skill's details |
| `zen skill precipitate` | Stage a distilled skill draft |
| `zen skill confirm <name>` | Confirm a staged skill |
| `zen discover run` | Run one self-learning cycle (stages 5b/5c/5d) |
| `zen discover stage` / `queue` | Stage and inspect improvement hypotheses |
| `zen discover confirm` / `reject` | Review staged hypotheses |
| `zen discover report` | Learning metrics (routing, stalls, calibration, replay) |
| `zen discover arena` | Run distill regression gate vs baselines |
| `zen discover calibrate --write` | Derive decision thresholds from labeled data |
| `zen discover label status` / `next` / `set` | Label supply workflow |

## System & Daemon

| Command | Description |
|---------|-------------|
| `zen serve start` | Start gateway daemon (`--foreground`, `--http` for loopback HTTP carrier, `--mcp` for stdio MCP mode) |
| `zen serve status` | Daemon health |
| `zen serve stop` | Stop daemon |
| `zen serve test` | Test MCP connectivity |
| `zen serve install` / `uninstall` | Manage macOS launchd auto-start |
| `zen doctor` | System health: 8 liveness probes (`--json` for scripts) |
| `zen clean cache` | Clean caches (ungated) |
| `zen clean all` / `trash` | Destructive cleanup — asks for confirmation unless `--yes` |
| `zen logs <service>` | View structured logs |
| `zen audit` | Audit log operations |
| `zen sandbox test` | Verify sandbox isolation works |

## Workspace & Configuration

| Command | Description |
|---------|-------------|
| `zen workspace init` | Initialize `~/.zen/` workspace structure |
| `zen config show` | Show effective configuration (layers merged) |
| `zen provider list` | List available LLM providers |
| `zen provider test <name>` | Test a provider connection |
| `zen auth list` | List stored credentials |
| `zen model` | Model metadata + routing info |

## Utilities

| Command | Description |
|---------|-------------|
| `zen starter <template>` | Generate project scaffold from template |
| `zen wps <action>` | Work process utilities |
| `zen habit log` / `list` | Habit tracking |
| `zen goal create` / `list` / `status` | Goal management |
| `zen plugin list` / `install` / `enable` / `disable` / `rehash` | Plugin management (WASM tools, MCP) |

## Quick Reference

```bash
# Daily workflow — all through the TUI
zen

# Or scripted
zen doctor                  # health check
zen wiki loop status        # is the learning loop alive?
zen wiki distill            # drain the inbox now
zen discover report         # what did the system learn?

# Maintenance
zen clean cache
zen serve status
```

---

**Full reference:** `zen --help` for the most up-to-date list of commands and flags.
