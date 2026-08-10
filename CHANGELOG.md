# Changelog

All notable changes to Zen are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Post-implementation review fix batch** (branch `003-agentic-plugin`, follows
  the v0.0.5 release) — reconciles the 27 findings from the 2026-08-09
  `/plan-eng-review` of the shipped agentic plugin:
  - `HttpMcpTransport` — Streamable HTTP client transport for remote MCP servers
    (FR-014): JSON-RPC POST with `Accept: application/json, text/event-stream`,
    SSE response parsing, `Mcp-Session-Id` echo, and JSON-RPC id validation.
    (Hand-rolled per Constitution XI: `rig-mcp` 0.2.5 ships no HTTP client.)
  - `zen plugin mcp reconnect <name>` — connectivity smoke-test CLI (FR-013).
  - Lexical path-traversal protection in the sandbox: `..` escapes from the
    workspace root are rejected on read/write, even for not-yet-existing files
    (FR-004 hardening).
  - Web search provider precedence corrected to Brave → Tavily → DuckDuckGo;
    DuckDuckGo now handles 429/Retry-After and percent-decodes `uddg=` redirect
    URLs; `max_results` clamped to [1, 50] (FR-008).
  - `WebFetchConfig` wired into `web.fetch` with a `[web_fetch]` config section
    (FR-011); audit records now include call duration (FR-020).
  - Approval/confidentiality hooks wired through `build_sandbox_hooks`;
    mutating tools gated in `Ask` mode, cloud tools gated under `Confidential`
    sessions (FR-018/FR-019).

## [0.0.5] - 2026-08-09

### Added — Agentic Plugin (File I/O, Web Tools, MCP Integration)

First release of the agentic plugin surface: the agent can now touch the
workspace and the web, and connect to external MCP servers — all behind the
existing permission/sandbox pipeline. (Spec: `docs/specs/003-agentic-plugin/`.)

- **File I/O tools** (FR-001..FR-005, FR-021, FR-022):
  - `fs.read` — read any text file under the configured workspace root.
  - `fs.write` — create/overwrite files, subject to sandbox mode.
  - `fs.list` — list directory entries within the workspace root.
  - `fs.edit` — surgical unified-diff edits (via `diffy`), with atomic
    write + `.bak` backup guarantee.
  - `fs.delete` / `fs.move` / `fs.copy` — full OS-level file operations.
  - `fs.grep` / `fs.glob` — pattern search and path globbing.
  - Protected paths rejected in all modes: `.git/`, `.zen/`, `~/.ssh/`,
    `~/.aws/`, `~/.gnupg/`, `.env` (and `*.env` variants), Keychain (FR-004).
  - Read-only sandbox mode rejects writes with a clear report (FR-005).
- **Web search** (FR-006..FR-009):
  - `web.search` with three pluggable providers: DuckDuckGo (zero-config
    default), Brave Search API (auto-upgrade when `BRAVE_SEARCH_API_KEY` set),
    Tavily API (auto-upgrade when `TAVILY_API_KEY` set). Provider overridable
    per-call via config. Results include title, URL, snippet.
  - Blocked entirely when the session is tagged `Confidential`.
- **Web content scraper** (FR-010..FR-012):
  - `web.fetch` — URL → primary human-readable Markdown via `reqwest` +
    `readabilityrs` + `htmd`, with a Jina Reader fallback for JS-rendered
    pages (`"source": "jina_reader"` annotated).
  - Truncation at 50KB / 2000 lines (configurable) with explicit truncation
    notice; non-HTML resources return metadata (content-type, size).
- **MCP client integration** (FR-013, FR-014):
  - Connect to external MCP servers declared in config via stdio (launched as
    subprocess) with crash recovery: auto-restart with exponential backoff
    (1s → 2s → 4s, max 3 attempts), tools marked unavailable after 3
    consecutive failures.
  - First-trust prompt persisted per-server in `~/.zen/mcp_trust.json`;
    untrusted servers skipped with a warning (FR-018).
- **Safety & permission integration** (FR-018..FR-020):
  - All new tools pass through the allow/deny/ask hook pipeline — no bypass.
  - New `Ask` sandbox mode: mutating tools (`fs.write`, `fs.edit`,
    `fs.delete`, `fs.move`, `fs.copy`, `web.search`, `web.fetch`) require
    explicit approval via an `ApprovalCallback` (TUI prompt in the CLI,
    auto-deny in the gateway).
  - Every tool invocation (success or failure) recorded to `logs/audit.jsonl`
    with sanitized arguments and outcome (FR-020).

## [0.0.4] - 2026-08-03

### Changed

- TUI: inline stream mode for agent responses (`zen` with no subcommand).
- Upgraded `rig` to the current release (LLM abstraction layer).
- Fixed TUI display issues in stream mode.
- Updated install documentation.

## [0.0.3] - 2026-07-26

### Changed

- Release workflow + CI build pipeline fixes (GitHub Actions).
- Fixed lint errors (including `fastembed` feature-gated code).

## [0.0.2] - 2026-07-25

### Added

- **Self-learning memory audit fixes** (two audit passes, 2026-06-27):
  - `Fact` struct with full Markdown persistence (`save`/`load`/`load_all`),
    YAML frontmatter + body format, UUID ids, entity associations.
  - Signal persistence for `ReflectionSignal`, `AntiPatternSignal`,
    `MentalModelSignal` (`to_markdown`/`from_markdown` → disk).
  - Priority scoring (`priority_items`) injected into prompt assembly;
    signal sections rendered in all four prompt paths.
  - Reinforcement tracker wired into `SelfLearningSignals::load()`.
  - Mental model / anti-pattern promotion: accepted candidates now written
    directly to `wiki/wisdom/models/` and `wiki/wisdom/anti-patterns/`.
  - KPI module (`CommitmentOkr` + `compute_commitment_completion_rate`):
    commitment-completion rate consumed by the `CommitmentTracker` worker;
    anti-talk indicator (mention→achievement ratio > 5 warns).
  - ReflectionWorker now synthesizes M4 anti-pattern candidates via LLM.
  - Prompt-injection detection in tool arguments (role hijacking, delimiter
    injection, instruction override).
  - `JournalWorker` renamed to `MemoryCurator` per DESIGN.md §10.1.
- **Vault / knowledge graph**: entity-graph rebuild logic
  (`recompute_entities`), FTS5 tier consistency fixes (`notes_fts` table name
  unification, Tier3 `notes_meta` join).

### Changed

- `PromptAssemblyBuilder` — renders 8 self-learning signal sections into all
  four prompt assembly paths (default, coordinator, agent, custom).
- `WisdomSynthesizer` — writes to `wiki/wisdom/models/{slug}.md` and
  `wiki/wisdom/anti-patterns/{slug}.md` (was dated suggestion files).
- `SessionJournaler` — accepts `fresh_eyes_mode` for unbiased extraction.

## [0.0.1] - 2026-06-07

### Added

- Initial release of Zen CLI productivity suite.
- 12 workspace crates with binary/library split (`zen` binary → `zen-cli` lib).
- 27 CLI commands with clap derive; no-arg invocation launches the agentic TUI.
- 5-tier search pipeline: ripgrep → FTS5 → vec0 embeddings → entity graph → LLM
  synthesis (FR-007..FR-011 of 001-agentic-foundation).
- 13 LLM providers across 3 protocol types (rig-native, openai-compatible,
  anthropic-compatible) with 4-tier auth resolution.
- 13 agents in 4 tiers, 3-layer permission model, 4-channel blackboard,
  QualityPipeline (Metis → Momus → Hermes → Zeus).
- Memory foundation: daily logs (`zen-memory`), `MEMORY.md` identity context
  (SOUL.md / MEMORY.md / AGENTS.md), session lifecycle management.
- WASM sandboxed plugins via `wasmtime` (FR-036..FR-039); MCP server support
  via the gateway (FR-035).
- macOS Keychain credential storage via `security-framework` (FR-061).
- 3-layer sandbox: `read-only` / `workspace-write` / `danger-full-access`
  modes (FR-059), metadata-path protection `.git/` `.zen/` `.ssh/` `.aws/`
  `.gnupg/` (FR-060), and resource limits: `setrlimit` (RLIMIT_NPROC=50,
  RLIMIT_NOFILE=256, RLIMIT_CORE=0), 300s timeout, 20 exec/min rate limit
  (FR-064).
