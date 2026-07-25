<!--
Sync Impact Report:
- Version: 1.5.0 → 1.6.0 (MINOR - Data Migration Compatibility principle added)
- Modified principles: None
- Added principles: XIII. Data Migration Compatibility
- Removed sections: None
- Templates requiring updates: None
- Deferred items: None
-->

# ZenSpace Constitution

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

**Version**: 1.6.0 | **Ratified**: 2026-02-24 | **Last Amended**: 2026-07-25
