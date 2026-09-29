# Quick Start

## 1. First Run — The TUI

```bash
zen
```

That's it. The first run initializes `~/.zen/` with the default directory structure and embedded configuration, then drops you into the interactive TUI — a chat session with your personal agent team.

## 2. Talk to Your Agent

Type naturally and hit <kbd>Enter</kbd>. The agent routes your request across 13 specialists (search, execution, planning, review) and streams the answer back.

| Key | Action |
|-----|--------|
| <kbd>Enter</kbd> | Send message |
| <kbd>Shift+Enter</kbd> | Insert newline (multi-line input) |
| <kbd>Ctrl+J</kbd> | Insert newline — tmux/SSH-safe fallback |
| <kbd>Ctrl+D</kbd> | Exit |
| <kbd>PageUp</kbd> / <kbd>PageDown</kbd> | Scroll history |

**Slash commands**: type `/` to open the command popup. It filters as you type (matched characters highlighted, exact matches sorted first), groups commands by category, and <kbd>Esc</kbd> dismisses it. Use `/tools` to expand or collapse tool-call cards and `/thinking` to show reasoning blocks.

**Rich output**: tool calls stream in as collapsible cards (`🔧` started / `✅` done with timing and hit counts), the footer shows `● working Ns` while a turn runs, large pasted text collapses into a `[Pasted N lines / M chars]` pill, and `diff`/`patch` snippets render with red/green deltas.

## 3. Feed the Pipeline

Notes don't need a command — just drop files into the inbox. Markdown and plain text pass through as-is; **PDF, Office documents (docx/xlsx/pptx), and ebooks (epub/odt/rtf) are converted to Markdown automatically** on ingestion:

```bash
mkdir -p ~/.zen/vault/inbox
$EDITOR ~/.zen/vault/inbox/q3-planning.md
cp ~/Downloads/annual-report.pdf ~/.zen/vault/inbox/    # converted at intake
```

The background distill loop picks them up automatically: extracts entities, links them into the knowledge graph, compiles wiki pages, and archives the original. It runs while the TUI is open (or via the `zen serve` daemon) — no manual step.

Format notes:

- **PDF / Office** convert in-process — no extra installs.
- **epub/odt/rtf** convert via [pandoc](https://pandoc.org) if it's on your `PATH` (`brew install pandoc`); without it those files stay queued and are picked up once installed — nothing is lost.
- Files with no extractable text (e.g. scanned image-only PDFs) are quarantined rather than re-processed every cycle; originals are never modified.

## 4. Check System Health

```bash
zen doctor
```

Runs 8 liveness probes (config, database, memories, daemon, loop, provider, vault, outbox) and exits non-zero if anything needs attention. Add `--json` for machine-readable output.

## 5. See What It Built

```bash
zen wiki list
```

Browse the self-built wiki:

```bash
zen wiki show <page>
zen wiki lint        # orphan pages, broken wikilinks
```

## Next Steps

- [Configure LLM providers](configuration/providers.md) to unlock AI features
- [Set up model routing](configuration/agent-routing.md) for agent tasks
- Explore the [CLI reference](cli-commands.md) for all available commands
