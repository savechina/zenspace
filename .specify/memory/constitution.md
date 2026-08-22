<!--
Sync Impact Report:
- Version: 1.7.0-pending → 1.8.0 (MINOR - Code Documentation & Scope Logic principle added)
- Modified principles: None
- Added principles: XV. Code Documentation & Scope Logic (2026-08-22)
- Removed sections: None
- Templates requiring updates: plan.md Constitution Check tables across feature specs (add XV row)
- Deferred items: Principle XIV ratification requires a dedicated PR with review and explicit approval; spec 003-agentic-plugin FR-035..045 anticipate the principle and implement its intent ahead of ratification. Plan gate is conditional until the amendment lands.
-->

# ZenSpace Constitution

> **v1.7.0-pending** — Principle XIV (Agent Safety) proposed 2026-08-11, pending ratification PR. Spec 003-agentic-plugin FR-004, FR-018..020, FR-023..045 implement the principle's intent ahead of formal ratification. See Governance section for amendment procedure.

## Core Principles

### I. CLI-First

The tool MUST be a command-line interface application. Every feature is exposed via CLI subcommands using the clap framework. Text in/out protocol: args/stdin → stdout, errors → stderr. Support both JSON and human-readable output formats.

Rationale: CLI is the primary interface; ensures debuggability and scriptability.

### II. Robust Error Handling

All errors MUST use thiserror for user-defined error types and anyhow for context-rich error propagation. Every public function MUST return a Result type. Error messages MUST be actionable and include context.

Rationale: Clear error paths improve debugging and user experience.

### III. Observability

Structured logging via tracing with env-filter MUST be used. Log levels MUST be configurable via environment. Key operations MUST include spans for tracing.

Rationale: Production debugging requires structured, filterable logs.

### IV. Configuration Management

Configuration MUST be loaded from .env files using dotenvy. Configuration values MUST be validated at startup. No hardcoded configuration values allowed.

Rationale: Environment-based config enables deployment flexibility.

### V. Template-Driven Scaffolding

Project templates MUST be embedded at compile time using include_dir. Templates MUST support multiple architectural patterns (mvc, ddd). Template rendering MUST use tera.

Rationale: Embedded templates ensure single-binary distribution; tera provides flexible rendering.

### VI. Code Quality

All code changes MUST pass `cargo clippy` with no warnings. Code MUST be formatted with `cargo fmt`. No type errors allowed (`cargo build` must succeed). No `unsafe` blocks unless documented and justified. **Exception**: `unsafe` blocks for `std::env::set_var`/`std::env::remove_var` in test code are permitted under Rust edition 2024 safety rules, provided each block includes a safety comment citing the edition requirement. Dependencies MUST be kept minimal and reviewed for maintenance status.

Rationale: Automated tooling enforcement prevents common bugs and maintains readable, consistent code.

### VII. Architecture Quality

Code MUST follow single responsibility principle - each module has one clear purpose. Public APIs MUST have documentation comments (docstrings). Internal implementation details MUST be private by default. Dependency direction MUST flow toward stable interfaces, not unstable internals.

Rationale: Clean architecture enables maintainability and reduces coupling.

### VIII. Testing Standards

Unit tests MUST accompany every new public function. Integration tests MUST verify CLI command execution end-to-end. Test names MUST describe the scenario being tested (given/when/then format preferred). Tests MUST be independent - no shared mutable state between tests. Mock external dependencies (database, filesystem) in unit tests.

Rationale: Comprehensive test coverage prevents regressions and documents behavior.

### IX. User Experience Consistency

CLI output format MUST be consistent across all commands. Exit codes MUST follow conventions: 0 for success, 1 for general errors, 2 for usage errors. Help text MUST be available for every command and subcommand. Error messages MUST be human-readable, not raw internal errors.

Rationale: Consistent UX reduces cognitive load and improves usability.

### X. Performance Requirements

Cold start time MUST be under 500ms. Memory footprint MUST be under 50MB for typical operations. Operations on large datasets MUST use streaming/chunking to avoid loading entire datasets into memory. Async I/O MUST be used for all blocking operations.

Rationale: CLI tools must feel responsive; users expect low latency.

### XI. Design-First & Reuse Priority

**All implementation MUST follow this mandatory sequence**:
1. **Design before coding** — No implementation without documented design decisions
2. **Reuse existing frameworks** — Search community best practices before custom solutions
3. **Avoid reinventing the wheel** — Use established libraries, patterns, and frameworks
4. **Simplicity over novelty** — Prefer proven, battle-tested solutions over clever implementations

**Enforcement requirements**:
- Every PR MUST document the design rationale (why this approach, what alternatives considered)
- Custom implementations MUST justify why existing solutions were insufficient
- Framework/library selection MUST reference community adoption metrics (GitHub stars, maintenance activity)
- "Simple reuse" is the default; "Custom implementation" requires explicit approval

**Prohibited patterns**:
- Implementing from scratch when a well-maintained library exists
- Creating new abstractions without searching existing patterns
- Preferring novel solutions without documented performance/feature advantages
- Skipping design phase and jumping directly to implementation

Rationale: Industry best practices reduce maintenance burden, improve reliability, and accelerate development. Proven frameworks handle edge cases we haven't discovered yet.

### XII. Unified Data Layer Architecture

**All persistence operations MUST follow the unified data layer design**:

1. **Single Database File**: All application state MUST be stored in a single `state.db` SQLite file. No separate database files for different domains (no kb.db, vec.db, graph.db, sessions.db).
2. **Unified Client**: All database access MUST go through `SqliteClient` which holds both `tokio_rusqlite::Connection` (for writes) and `sqlx::SqlitePool` (for reads). No direct `SqlitePool` or `Connection` usage.
3. **Domain-Driven Repositories**: Data access MUST be organized by business domain, not by technology. Each domain repository holds a `&SqliteClient` reference and exposes domain-specific async methods.
4. **Repository Taxonomy**:
   - `NotesRepo` — FTS5 full-text search operations
   - `EmbeddingsRepo` — Vector similarity search (sqlite-vec)
   - `EntitiesRepo` — Entity graph operations (entities, relationships, aliases)
   - `SelfModelRepo` — Self-knowledge nodes
   - `GoalsRepo` — Goals and paths
   - `BeliefsRepo` — Belief tracking
   - `DispatchRepo` — Task dispatch and scheduling
   - `SessionsRepo` — Session index

**Prohibited patterns**:
- Creating separate database files for different concerns
- Using raw `SqlitePool` or `Connection` directly in business logic
- Technology-named repositories (FtsRepo, VectorRepo, GraphRepo)
- Sync database access methods in async contexts

**Enforcement requirements**:
- All new data access code MUST use domain repositories
- Repository methods MUST be async and take `&SqliteClient`
- Schema migrations MUST be coordinated through `SqliteClient::ensure_schema()`
- Cross-domain queries MUST go through `SqliteClient` to maintain consistency

Rationale: Unified database eliminates schema sync issues, enables cross-domain queries, and reduces operational complexity. Domain-driven naming improves code readability and maintainability.

### XIII. Data Migration Compatibility

**All schema changes MUST preserve user historical data.** The default is forward-only, additive migration. Destructive operations are forbidden without an explicit, tested backup-restore protocol.

1. **Forward-Only Additive Migrations** — Every schema change MUST be expressible as one of:
   - `CREATE TABLE IF NOT EXISTS ...` (new table)
   - `CREATE INDEX IF NOT EXISTS ...` (new index)
   - `CREATE VIRTUAL TABLE IF NOT EXISTS ...` (new FTS5/vec0)
   - `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT ...` (new column; nullable columns need no default)
   - `CREATE TRIGGER IF NOT EXISTS ...` (new trigger)
   Idempotent DDL is mandatory. Every DDL statement MUST be re-runnable without error.

2. **Version Tracking** — Every migration MUST be a numbered file under `crates/zen-repo/migrations/` (`NNN_description.sql`) and applied through `sqlx::migrate!()`. The `_sqlx_migrations` table is the source of truth for what has been applied. No ad-hoc schema mutation outside the migration runner.

3. **Forbidden Without Explicit Exemption**:
   - `DROP TABLE`
   - `DROP COLUMN` (and any `ALTER TABLE ... DROP` form)
   - `DROP INDEX` (except immediately after a same-name `CREATE INDEX` replacement within the same migration, with the rename pattern)
   - `TRUNCATE`
   - `DELETE FROM <schema_table>` as part of a migration (operational deletes from application code are separate)
   These operations destroy data the user cannot recover. If a column or table is no longer used, leave it in place (backward compatibility) — see the `notions.aliases` column precedent in migration 003.

4. **Destructive Migration Protocol (when ALTER is impossible)** — Some changes cannot be expressed additively:
   - Changing a `vec0` virtual table dimension (vec0 has no `ALTER`)
   - Changing an `FTS5` virtual table column set (FTS5 has no column-level `ALTER`)
   - Changing a column type incompatibly (e.g., `TEXT` → `INTEGER`)
   When unavoidable, the migration MUST follow this exact sequence, all within a single numbered `.sql` file:
   1. `CREATE TABLE _backup_NNN AS SELECT * FROM <old_table>;` (snapshot)
   2. `CREATE TABLE <new_table> ...` (or `CREATE VIRTUAL TABLE`)
   3. `INSERT INTO <new_table> (...) SELECT ... FROM _backup_NNN;` (transform + load)
   4. `DROP TABLE <old_table>;` (now safe — data is in `_backup_NNN` and `<new_table>`)
   5. `ALTER TABLE <new_table> RENAME TO <old_table>;`
   6. Recreate indexes, triggers, FTS5 sync
   The `_backup_NNN` table MUST be retained (not dropped) until the next migration confirms the cutover succeeded. The migration MUST include a verification `SELECT count(*) FROM <renamed_table>` comment showing expected row count.

5. **Backward Compatibility Window** — When a column or table is deprecated, it MUST remain in the schema for at least one minor release before removal is considered. Application code MUST stop writing to it but continue to tolerate reading it. Removal requires a follow-up migration after the window expires.

6. **Migration Test Verification** — Every new migration file MUST be accompanied by an integration test that:
   - Seeds a database at the prior schema version with representative data
   - Applies the new migration
   - Asserts row counts are preserved (no data loss)
   - Asserts the new schema elements exist (`PRAGMA table_info`, `SELECT FROM sqlite_master`)
   - Asserts application queries against the post-migration schema still work
   The test MUST live in `crates/zen-repo/tests/` and be named `migration_NNN_description.rs`.

7. **Vec0 Dimension Forward-Compatibility** — When declaring `vec0` virtual tables, choose the maximum supported dimension (`FLOAT[4096]`) and handle smaller embedding models via application-layer zero-padding (see `pad_to_dim()` in `zen-vault/src/tindy/embeddings.rs`). This avoids dimension-change migrations entirely. Smaller-dimension tables are a legacy state and MUST be tolerated via `IF NOT EXISTS` (never recreated).

8. **FTS5 Rebuild Capability** — When a migration changes FTS5 sync triggers or underlying columns, the migration MUST include the standard FTS5 rebuild command so any existing rows are reindexed:
   ```sql
   INSERT INTO <fts_table>(<fts_table>) VALUES('rebuild');
   ```
   This is idempotent and safe to run on a freshly created FTS5 table (no-op).

**Enforcement requirements**:
- All schema changes MUST go through numbered migration files in `crates/zen-repo/migrations/`
- `SqliteClient::ensure_schema()` (per Principle XII) MUST run all pending migrations on connect
- Migration files MUST be reviewed for forbidden statements before merge (CI grep gate recommended)
- AGENTS.md schema documentation MUST be updated in the same PR that adds or alters tables (documentation drift is a violation)
- The `migration_NNN_*.rs` integration test MUST pass in CI before the migration ships

**Prohibited patterns**:
- `DROP TABLE` in a migration without the backup-recreate-reimport sequence above
- Application code calling `conn.execute("CREATE TABLE ...")` outside the migration runner
- Skipping the migration test because "the change is trivial"
- Bumping a vec0 dimension via drop-and-recreate (use application-layer padding instead)
- Renaming a table without a deprecation alias period (breaks downstream queries)
- Leaving stale schema docs in AGENTS.md that reference tables/columns that no longer exist or were renamed

Rationale: The knowledge base is the user's accumulated work product. A destructive migration that loses notes, entities, beliefs, or embeddings is not a bug — it is a data loss incident. Additive-only migrations, version tracking, and tested destructive-protocols make "the schema can evolve" a guarantee rather than a hope. The forward-compatibility techniques (max-dim vec0, dead-column tolerance, FTS5 rebuild) cost nothing at write time and eliminate whole classes of future migration pain.

### XIV. Agent Safety *(PROPOSED 2026-08-11 — pending ratification PR)*

**Status**: PROPOSED. This principle was identified as missing during the 2026-08-11 `/speckit.analyze` audit of 003-agentic-plugin, which surfaced 25 safety FRs without a constitution-level anchor. Per Governance below, amendment requires a dedicated PR with documented review and explicit approval. Until ratification, the principle is informational; the spec FRs (FR-004, FR-018..020, FR-023..045) carry the binding force. Once ratified, all agent-bearing features MUST comply.

**The agent runs on the user's host operating system. Every host-OS touch point MUST be mediated by an explicit, auditable safety layer. "The user installed zen" is not authorization for the agent to read secrets, spawn arbitrary subprocesses, exfiltrate data, or persist state outside its workspace.**

The agent's host-OS surface MUST be designed against the threat model: a **misled or compromised LLM** (prompt injection, malicious MCP server, poisoned plugin, adversarial web content) attempting to (a) read credentials, (b) spawn malicious subprocesses, (c) probe internal network, (d) destroy user data, or (e) persist beyond the session.

1. **Path-Bounded File Access** — All file operations MUST be scoped to a configured workspace root. Paths MUST be canonicalized before validation to defeat symlink escapes (per spec FR-024). A closed protected-path list (`PROTECTED_PATHS` per FR-004) MUST block credential directories regardless of sandbox mode; the list is fixed in code, not user-configurable (per `/speckit.clarify` 2026-08-11).

2. **Subprocess Sandbox** — Any agent-initiated subprocess (`shell.exec`, MCP stdio transport, plugin-loaded native code) MUST run inside an OS-level sandbox: macOS Seatbelt, Linux Bubblewrap+Landlock, Windows AppContainer. Bare `tokio::process::Command` without sandbox wrapping is forbidden on the agent path. Resource limits (RLIMIT_NPROC, RLIMIT_NOFILE, RLIMIT_CORE) MUST be applied to the agent process at wiring time.

3. **Network Egress Policy** — All outbound network calls from agent-reachable tools (web fetch/search, MCP HTTP, LLM providers) MUST pass a domain/IP allowlist. Link-local (169.254.0.0/16 cloud metadata), loopback (127.0.0.0/8 except explicitly-allowlisted provider hosts), and RFC1918 ranges MUST be denied by default to prevent SSRF. Per-domain overrides require explicit user approval.

4. **Environment Scrubbing** — Subprocess spawn (shell.exec, MCP stdio) MUST scrub the parent environment of secret-bearing variables (`*_API_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`) before exec. The child env is reconstructed explicitly via tool args, never inherited verbatim.

5. **Permission Pipeline** — Every tool invocation MUST pass through a multi-stage hook pipeline (confidentiality → budget → seatbelt → audit → approval) before dispatch. No tool may bypass the pipeline. Mutating tools MUST require interactive approval when the session is in `Ask` mode (FR-019). The pipeline MUST cover **every registered tool's** path/command-bearing args, regardless of arg name (per FR-035).

6. **Audit Trail** — Every tool invocation — successful, failed, or cancelled — MUST produce an audit record with metadata (tool name, args redacted of secrets, outcome, duration, caller, sandbox mode). For privacy-bearing tools (`fs.read`), the record MUST NOT include file content, hashes, or byte previews (per `/speckit.clarify` 2026-08-11). Audit logs MUST be append-only and safe to ship to shared telemetry.

7. **Plugin Integrity** — Loaded plugins (WASM or native) MUST declare their entry-point hash in the manifest; the loader MUST verify the hash before instantiation (FR-043). Native `.dylib`/`.so` plugins MUST additionally pass code-signing verification on platforms that support it (macOS `codesign --verify --strict`). Unsigned or hash-mismatched plugins MUST be rejected.

8. **Process Hardening** — The zen process MUST harden itself at startup: disable core dumps (RLIMIT_CORE=0), deny debugger/ptrace attachment (Linux `PR_SET_DUMPABLE=0`, macOS `PT_DENY_ATTACH`), and strip library-injection env vars (`LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`) from its own environment before any other code runs (FR-044).

9. **Graceful Drain** — The session MUST install SIGINT/SIGTERM handlers that drain in-flight tool calls within a configurable window (default 5s) rather than killing the process mid-dispatch. Cancellation MUST produce an `outcome: "cancelled"` audit record per interrupted call and clean up child processes via process-group signal (FR-041).

10. **Tempfile Lifecycle** — All temporary files (`.bak`, `.tmp`) created by tools MUST be managed by `Drop` guards that remove them on early-return or panic, AND swept at workspace-open time to recover from prior crashes (FR-040).

11. **Default Deny** — When a new tool is added and its safety posture is unclear, the default is `Sensitivity::Confidential` (excluded from external MCP clients) and `SandboxMode::Ask` (prompt before each invocation). Promotion to `Public`/`Private` and `WorkspaceWrite` requires explicit spec-level justification.

**Enforcement requirements**:
- All agent-bearing features MUST enumerate their host-OS touch points in the feature spec and map each to a safety FR.
- Cross-artifact analysis (`/speckit.analyze`) MUST be run before `/speckit.implement` and MUST verify safety-FR coverage.
- The audit log MUST be inspectable via `zen audit` and shipped safely to shared telemetry without redaction post-processing.
- New tools MUST declare their path/command-bearing arg names in the seatbelt arg registry (FR-035); tools that fail to do so are rejected at wiring time.
- New subprocess-spawning paths MUST be reviewed against principles 2 (sandbox), 4 (env scrub), and 9 (drain).
- Principle IX (UX Consistency) remains in force — safety prompts MUST be human-readable, with the full `binary + args + cwd` or `path + operation` displayed.

**Prohibited patterns**:
- Bare `tokio::process::Command::new(...).spawn()` in agent-reachable code without a sandbox wrapper.
- `tokio::fs::read(path)` where `path` came from agent input and was not canonicalized first.
- Inheriting the parent env verbatim when spawning child processes from agent tools.
- New tools registered with bypassed or partial hook-pipeline wiring.
- Storing file content in the audit log for `fs.read` calls.
- Loading `.wasm`/`.dylib`/`.so` plugins without a verified `sha256` in the manifest.
- Adding a new agent tool without listing its path/command-bearing args in the seatbelt arg registry.

**Reference systems** (consulted during principle formulation, 2026-08-11):
- **OpenAI Codex CLI** — strongest sandbox: Seatbelt + Bubblewrap + Landlock + AppContainer; Starlark exec-policy engine; in-process MITM network proxy; `process-hardening` via `#[ctor::ctor]`; tree-sitter compound-command decomposition.
- **Anthropic Claude Code** — richest permission UX: 6 permission modes; per-tool glob rules; `sandbox.credentials.files` + `envVars` deny list; `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB`; managed settings for org lockdown.
- **sst/opencode** — minimal-but-clean permission model: `allow`/`deny`/`ask` with `once`/`always` persistence; `.env` denied by default; `external_directory` gating; doom-loop detection.
- **Aider** — explicit anti-pattern: no sandbox, no permission system; safety relies on git undo. NOT a model for zen.
- **Block Goose** — anti-pattern by default (YOLO mode); opt-in macOS Seatbelt is the right direction but too little by default.

Rationale: The agent is the highest-privilege software most users will ever run on their workstation — it reads what they read, writes what they write, and (post-FR-028) executes what they execute. Without an anchored safety principle, every feature spec re-litigates the same tradeoffs and every regression weakens the floor. A constitution-level mandate makes "is this safe?" a gate, not a question.

## Technology Stack

- **Language**: Rust (edition 2024)
- **CLI Framework**: clap for argument parsing
- **Logging**: tracing with env-filter
- **Database**: Single SQLite file (state.db) via SqliteClient with dual-architecture (tokio-rusqlite for writes, sqlx for reads)
- **Vector Search**: sqlite-vec extension with 384-dim embeddings
- **Full-Text Search**: SQLite FTS5 with porter tokenizer
- **Error Handling**: thiserror + anyhow
- **Template Engine**: tera with include_dir for embedding
- **Configuration**: dotenvy

Rationale: Selected for CLI tool suitability, runtime performance, and developer productivity. Unified database architecture eliminates schema sync issues and enables cross-domain queries.

## Development Workflow

- All code changes MUST pass `cargo clippy` and `cargo fmt`
- All public APIs MUST include documentation comments
- Integration tests MUST cover CLI command execution
- **Workspace structure**: zen-cli is the binary crate (no lib.rs). Domain crates (zen-core, zen-service, zen-repo, zen-gateway, zen-provider) are library crates with lib.rs for DDD architecture. This separation enables reusable domain logic while maintaining a single CLI entry point.

Rationale: Enforces code quality and maintains project structure conventions. DDD architecture requires domain separation via library crates.

## Governance

This constitution supersedes all other practices. Amendments require:
1. Documentation of the proposed change
2. Review and approval via PR
3. Migration plan if applicable

All PRs MUST verify compliance with these principles. The AGENTS.md file serves as runtime development guidance.

**Pending amendments**:
- **v1.7.0 — Principle XIV: Agent Safety** (proposed 2026-08-11). Pending: dedicated PR with documented review. The principle text is in this file under §XIV but is informational until the PR merges. Spec 003-agentic-plugin FR-004, FR-018..020, FR-023..045 implement the principle's intent ahead of ratification. Once ratified, all agent-bearing feature specs MUST pass a Principle XIV compliance check in their plan.md Constitution Check table.

### XV. Code Documentation & Scope Logic

**All code MUST be documented with standardized, actionable descriptions that explain functionality, not just syntax. Code without documentation is incomplete code.**

1. **Standardized Code Block Format** — Every code block in documentation (README, AGENTS.md, specs) MUST follow this structure:
   ```markdown
   ```<language>
   # <PURPOSE>: One-line description of what this code does
   # <USAGE>: When to use this code
   # <EXPECTED>: What the user should see after running
   # <ERRORS>: Common failure modes and fixes

   <actual code>
   ```
   ```

2. **Scope Logic Documentation** — All scope-defining code (feature flags, mode switches, conditional logic) MUST include:
   - **Functionality**: What the code enables/disables
   - **User impact**: How the scope change affects behavior
   - **Default behavior**: What happens when not explicitly set
   - **Interaction**: How this scope interacts with other modes/flags

3. **Public API Documentation** — Every public function, struct, enum, and trait MUST have:
   - **Description**: What it does (not what it is)
   - **Parameters**: Each parameter's purpose and constraints
   - **Returns**: What the return value represents
   - **Errors**: When and why errors occur
   - **Examples**: At least one usage example

4. **CLI Command Documentation** — Every CLI command and subcommand MUST have:
   - **Description**: One-line purpose
   - **Usage**: Common invocation patterns
   - **Examples**: Both command and expected output
   - **Scope logic**: All flags with functionality, user impact, and defaults

5. **AGENTS.md Synchronization** — The AGENTS.md file MUST be updated in the same PR that adds or modifies:
   - New CLI commands or subcommands
   - New feature flags or scope logic
   - New code blocks in documentation
   - Changes to existing functionality affecting user behavior

**Enforcement requirements**:
- All new CLI commands MUST have corresponding AGENTS.md documentation
- Feature flags MUST be documented with functionality description and user impact
- Code blocks in specs MUST pass a documentation review before merge
- AGENTS.md changes MUST be reviewed for accuracy and completeness
- Public APIs MUST have doc comments that explain purpose, not just syntax

**Prohibited patterns**:
- Code blocks without language identifiers
- Code blocks without explanations of what they do
- Feature flags without scope logic descriptions
- CLI commands without AGENTS.md documentation
- Scope logic without user impact descriptions
- Public APIs without doc comments
- Documentation that describes syntax instead of functionality

Rationale: Undocumented code is technical debt. Standardized documentation reduces support burden, improves onboarding, prevents misuse, and makes the codebase self-documenting. AGENTS.md serves as the runtime development guide and MUST reflect the current state of the codebase.

**Version**: 1.8.0 | **Ratified**: 2026-02-24 | **Last Amended**: 2026-08-22 (v1.8.0 — Principle XV Code Documentation & Scope Logic) | **Pending Amendment**: 2026-08-11 (v1.7.0 — Principle XIV Agent Safety)
