# CLI Commands Reference

Zen's CLI surface is intentionally small: the TUI plus 24 subcommands. Heavy capabilities (search, research, dispatch, briefs) run autonomously through the background scheduler and distill loop rather than as manual commands — drop notes into `~/.zen/vault/inbox/` and the system works on them.

**JSON convention:** list/status/report-shaped subcommands honor `--json` (human output stays the default; action subcommands are excluded).

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
| `zen session status` | Show session status (`--json`) |
| `zen session list` | List sessions (`--json`) |
| `zen session archive` | Archive a session |

## Knowledge Pipeline

| Command | Description |
|---------|-------------|
| `zen wiki list` | List wiki pages (`--json`) |
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
| `zen agent list` | List registered agents (`--json`) |
| `zen skill list --json` | List learned skills |
| `zen skill run <name>` | Run a skill |
| `zen skill progress` | Show skill learning progress |
| `zen skill show <name>` | Show a skill's details |
| `zen skill precipitate` | Stage a distilled skill draft |
| `zen skill confirm <name>` | Confirm a staged skill |
| `zen discover run` | Run one self-learning cycle (stages 5b/5c/5d) |
| `zen discover stage` / `queue` | Stage and inspect improvement hypotheses (`queue --json`) |
| `zen discover confirm` / `reject` | Review staged hypotheses |
| `zen discover confirm-workflow <name>` | Promote a staged workflow draft into `vault/wiki/wisdom/workflow-templates/` |
| `zen discover reject-workflow <name>` | Reject a staged workflow draft and quarantine its shape signature |
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
| `zen doctor` | System health: 11 liveness probes (`--json` for scripts) |
| `zen clean cache` | Clean caches (ungated) |
| `zen clean all` / `trash` | Destructive cleanup — asks for confirmation unless `--yes` |
| `zen logs <service>` | View structured logs |
| `zen audit` | Audit log operations (`zen audit log --session <id> --json`) |
| `zen admission list` | Trust ledger: would-accepts, rejections, unresolved, admitted, duplicates (`--mode` filter, `--json`) |
| `zen admission show <id>` | Full why-when-what decision record (sender, rule, scope, mode, policy version, trigger) |
| `zen admission mode [<off\|observe\|auto>]` | Show effective accept mode + source + policy hash, or flip it at runtime via the state.db override row — hot effect on the live daemon, `--clear` reverts to config (`--channel`, `--json`) |
| `zen sandbox test` | Verify sandbox isolation works |

## Workspace & Configuration

| Command | Description |
|---------|-------------|
| `zen workspace init` | Initialize `~/.zen/` workspace structure |
| `zen config show` | Show effective configuration (layers merged, `--json`) |
| `zen provider list` | List available LLM providers (`--json`) |
| `zen provider test <name>` | Test a provider connection |
| `zen auth list` | List stored credentials (`--json`) |
| `zen model` | Model metadata + routing info (`zen model list --json`) |

## Utilities

| Command | Description |
|---------|-------------|
| `zen starter <template>` | Generate project scaffold from template |
| `zen wps <action>` | Work process utilities |
| `zen habit log` / `list` | Habit tracking (`list --json`, `zen habit stats <name> --json`) |
| `zen goal create` / `list` / `status` | Goal management (`list --json`) |
| `zen plugin list` / `install` / `enable` / `disable` / `rehash` | Plugin management (WASM tools, MCP; `list --json`, `zen plugin tools --json`) |

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
