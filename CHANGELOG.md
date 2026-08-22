# Changelog

All notable changes to Zen are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Agentic TUI — Codex-style inline stream mode** (spec `002-agentic-tui`,
  FR-001..FR-016; the 2026-08-16 implementation of the agent response
  display): `zen` with no subcommand now runs a bottom-anchored inline
  stream UI that writes output into the terminal's native scrollback
  instead of an alternate screen:
  - Scrollback insertion engine (`scrollback_inserter.rs`): committed LLM
    blocks are inserted directly into the native scrollback via
    `insert_before` (FR-001/FR-008), with wrap/truncate modes, wide-char
    (CJK/emoji) safe row copying, and a whitespace-based content-window
    measurement that fixed the blank-band/lost-banner bug (H2, T043/T044).
  - Inline event loop (`inline.rs`): `Viewport::Inline(8)` REPL with
    committed/tail streaming split (FR-005), ~30 fps redraw throttle
    (FR-006), `reanchor_viewport()` on resize (H4 fix), bracketed-paste
    routing, and a keyboard `reading_mode` (PageUp/PageDown, FR-016) that
    defers scrollback insertion while the user reads history.
  - Bottom viewport layout (`inline_ui.rs`): `[filler, tail, popup, input,
    toast, footer]` stack — input and footer always keep full rows, the
    streaming tail (2–4 rows) renders above the input, slash/session/model
    pickers shrink-to-fit on small terminals, and a transient toast row
    (T041/T059/T064). Footer shows model, streaming `⏳`, reading `⏸`,
    token count, session, and workspace (Codex-style).
  - Pre-LLM latency eliminated: background pre-warming of the agent
    orchestrator + knowledge DB at session start (`prewarm.rs`, T053) and
    async submit (`start_async_chat`, T055) cut the Enter-to-first-token
    gap from ~10s to <500ms; `knowledge_search` config (`fast`/`full`/
    `off`, default `fast`) caps expensive search tiers (T054).
  - Streaming markdown with syntax-highlighted fenced code blocks
    (syntect/two-face, `┌─ lang`/`└─` framing, NO_COLOR fallback,
    T028/FR-012); `/thinking` toggle now gates reasoning end-to-end —
    hidden blocks never flash through the viewport (T063).
  - Lazy provider construction (`router.rs`, T060): providers are built and
    API keys resolved only on first use — fixes the macOS Keychain ACL
    prompt at every startup for providers never used.
  - Inline mode is now the default; the alternate-screen full TUI is opt-in
    via `ZEN_TUI_FULLSCREEN` (FR-002).
  - 4-layer test pipeline (`bin/test-tui`): L1/L2 headless unit + scenario
    tests (18 tests, incl. CJK and popup-layout regressions), a guard
    asserting ratatui's `scrolling-regions` feature stays disabled (T039),
    L3 PTY end-to-end via `portable-pty` + `vt100` (`tests/tui_pty.rs`, 8
    `#[ignore]` tests run by `bin/tui-pty-test`), and L4 tmux smoke
    (`bin/tui-smoke-tmux`) verifying banner, input, popup, and scrollback
    survival across exit (FR-015).
- **`shell.exec` tool + host-OS safety hardening** (spec `003-agentic-plugin`
  Phase 17/18, FR-028/FR-035..FR-045):
  - `shell.exec` (Confidential sensitivity) — executes a binary with a
    structured `argv` array (never a shell string), optional `cwd`
    (defaults to workspace root), `stdin`, `env` overrides, and
    `timeout_ms` (default 30s). Child env is scrubbed of all secret-bearing
    vars; the process runs in its own group and is SIGKILLed group-wide on
    timeout; output is bounded (10k chars / 64KB). Blocked network binaries
    (curl/wget) are terminated pre-dispatch by the seatbelt hook, and the
    tool is excluded from external MCP clients.
  - New zen-core safety modules: `env_scrub.rs` (FR-037 — strips
    `*_API_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD`/`*_CREDENTIAL` and
    loader-injection vars), `network_policy.rs` (FR-036 — SSRF-blocking
    `validate_url()` covering loopback, link-local, RFC1918, cloud
    metadata, and metadata hostnames), `process_hardening.rs` (FR-044 —
    startup `prctl`/`ptrace` debugger denial, `RLIMIT_CORE=0`, LD_*/DYLD_*
    strip), `tempfile_lifecycle.rs` (FR-040 — `TempfileDropGuard` RAII +
    boot-time sweep).
  - Sandbox hardening (`sandbox.rs`): symlink canonicalization that
    rejects escapes even for not-yet-existing write targets (FR-024),
    per-tool `ToolArgRegistry` closing seatbelt bypasses for
    `system.daemon`/`plugin.wasm_sandbox` (FR-035), bare-name network
    binary detection (FR-028), and soft-only resource limits (NPROC=50 /
    NOFILE=256 / CORE=0) that no longer permanently wedge busy processes
    (FR-038).
  - fs tool upgrades: `fs.read` byte-range + `max_bytes` streaming with
    base64 mode and BOM handling (FR-023/FR-027), `fs.list` depth/glob/
    include_hidden (FR-025), `fs.delete` `clear_contents` mode + workspace
    root guard (FR-026), `fs.edit` tempfile cleanup (FR-021/FR-040),
    `fs_watcher` 8-watcher cap (FR-045).
  - MCP/web egress policy: `mcp_client` HTTP endpoints, `web.fetch`
    redirect hops, and `web.search` providers all validated against the
    NetworkPolicy (FR-036).
  - TUI signal drain (FR-041): SIGINT/SIGTERM handlers with 5s drain
    window; second Ctrl-C exits immediately and restores the terminal.
  - WASM sandbox permission gate on every invoke with wasmtime
    `StoreLimits` memory cap (FR-029/FR-030); plugin manifest sha256 +
    macOS codesign integrity verification (FR-043).
  - New config sections: `[sandbox.rlimits]`, `[sandbox.env_scrub]`,
    `[sandbox.network_policy]`, `[session] drain_timeout_secs`,
    `[tools.fs_watcher] max_watchers` (FR-038/037/036/041/045).
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
- **Plugin runtime bridge** (FR-033/FR-034, spec `003-agentic-plugin`
  Phase 13b) — plugins now load at agent wiring construction, closing the
  "scaffolding-only" gap found by the 2026-08-15 `/speckit.analyze`:
  - `WasmPlugin` adapter: a manifest `type = "tool"` plugin with a `.wasm`
    entry has its exported `() -> ()` functions registered as namespaced
    tools (`{plugin_id}.{func}`, e.g. `echo.hello`) and executed through the
    WASM sandbox; manifest permissions are checked against the configured
    policy at load and re-checked per invoke (FR-029), and entries that
    failed sha256 integrity at discovery (`Lifecycle::Failed`) are excluded
    from activation (FR-043).
  - `ZenWiring::with_sandbox_mode` self-discovers plugins from
    `[plugin] base_path` when no registry is passed; per-plugin activation
    failures are isolated (warn + `Lifecycle::Failed`, remaining plugins
    continue) and plugin-registered tools default to `Private` sensitivity.
  - New `[sandbox.wasm]` config section (4 permission booleans, default
    deny-all) drives both the `plugin.wasm_sandbox` tool policy and plugin
    loading — the deny-all-in-production gap is closed.
  - SC-013 integration test: an `echo` plugin dropped in the plugin dir is
    discovered, `echo.hello` is registered and callable; a bad-sha256
    neighbor is isolated.
- **Plugin runtime hardening** (FR-046..051, spec `003-agentic-plugin`
  Phase 18) — production-grade security and lifecycle for the plugin bridge:
  - Config-driven agent tool grants via `[agents.tools]` overlay: exact
    names, `prefix.*` wildcards, and `plugin:*` (excludes reserved
    namespaces and builtin collisions) merged through 5-layer config
    (FR-046).
  - Strict `sha256` integrity required for `*.wasm` plugin entries;
    `zen plugin install` auto-writes the hash; `zen plugin rehash <id>`
    recomputes after deliberate updates (FR-049).
  - `zen plugin enable/disable` persists to `{plugin-dir}/state.json`
    (`{"disabled": [...]}`); takes effect at next session start, no
    hot-unload; corrupt file fails open with a loud `warn` (FR-047).
  - Plugin dispatch hooks wrapped in a fail-closed isolation adapter:
    hook `Err` denies that invocation only (audit-correlated), never
    aborts the round; plugin hooks invisible to `Confidential`
    invocations (FR-048).
  - Plugin id validation (`^[a-z0-9_-]+$`) plus reserved namespace
    prefixes (`fs`, `web`, `system`, `plugin`, `shell`) rejected at
    registration; spoofed builtin names blocked per-tool (FR-050).
  - Lazy WASM module precompile: `WasmPluginTool` caches compiled
    `Module` via `OnceLock`, wasm bytes held as `Arc<Vec<u8>>`
    (FR-051).

### Fixed

- **TUI inline stream display bugs** (2026-08-16, spec `002-agentic-tui`
  T039-T052):
  - Blank band / lost banner in streamed output — root-caused to
    `measure_wrapped_height` measuring every block as 10,000 rows (the
    `Cell::symbol()` predicate never matches empty cells); replaced with a
    whitespace-based `measure_wrapped_bounds` content window (H2, T043/T044).
  - Stray space after every CJK/wide character in scrollback — `insert_before`
    sent every cell verbatim, including continuation cells; `copy_row` now
    emits a no-op print for continuation cells (T052).
  - Floating viewport / blank band after terminal grow — resize now
    re-anchors the cursor to the bottom of the new screen before `resize`
    (H4, T049).
  - `v` key hijacked into text-selection whenever output existed, making it
    untypeable after the first response; command-mode `v` still enters
    selection, and the first character typed after leaving command mode is
    no longer swallowed (user report 2026-08-16).
  - Slash popup clipped the input box in small viewports — popup now shrinks
    to fit, input + footer always keep their rows (T059).
  - Paste events swallowed by the catch-all key handler (H1).
- **`fs.delete` workspace-root guard bypass** (CHK023, 2026-08-16): a
  symlink alias resolving to the workspace root could bypass the
  `path == root` compare and wipe the workspace via `clear_contents` —
  the guard now canonicalizes and compares both sides.
- **`shell.exec` seatbelt assurance** (CHK024, 2026-08-16): network
  binaries (curl/wget) are terminated pre-dispatch by the arg-registry
  seatbelt hook; the tool never falls back to unsandboxed execution when
  sandbox-exec/bubblewrap is absent (FR-028 Clarification 2026-08-15).
- **`fs.read` unbounded read was an OOM vector** under concurrent file
  growth — all read branches now bounded by `max_bytes` (FR-023).
- **`apply_resource_limits` hard caps were irreversible** — soft-only
  limits now prevent permanent EMFILE/EAGAIN on busy processes (FR-038).
- **Seatbelt only inspected `command`/`path` args** — `system.daemon` and
  `plugin.wasm_sandbox` could bypass validation; closed by `ToolArgRegistry`
  (FR-035).
- Linux (glibc) build: `apply_resource_limits` typed the rlimit resource as
  `libc::c_int`, which glibc's `getrlimit`/`setrlimit` reject (they take
  `__rlimit_resource_t`/u32). Now aliased to libc's per-target type.

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
