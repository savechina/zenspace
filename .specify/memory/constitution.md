<!--
Sync Impact Report:
- Version: 1.2.0 → 1.3.0 (MINOR - Rust 2024 unsafe clarification + role alignment)
- Modified principles: Code Quality §VI (unsafe blocks clarified for Rust 2024 edition)
- Added principles: None
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

## Technology Stack

- **Language**: Rust (edition 2024)
- **CLI Framework**: clap for argument parsing
- **Logging**: tracing with env-filter
- **Database**: SQLx with MySQL + SQLite support (agentic module uses SQLite)
- **Error Handling**: thiserror + anyhow
- **Template Engine**: tera with include_dir for embedding
- **Configuration**: dotenvy

Rationale: Selected for CLI tool suitability, runtime performance, and developer productivity.

## Development Workflow

- All code changes MUST pass `cargo clippy` and `cargo fmt`
- All public APIs MUST include documentation comments
- Integration tests MUST cover CLI command execution
- **Workspace structure**: zen-cli is the binary crate (no lib.rs). Domain crates (zen-core, zen-service, zen-data, zen-gateway, zen-provider) are library crates with lib.rs for DDD architecture. This separation enables reusable domain logic while maintaining a single CLI entry point.

Rationale: Enforces code quality and maintains project structure conventions. DDD architecture requires domain separation via library crates.

## Governance

This constitution supersedes all other practices. Amendments require:
1. Documentation of the proposed change
2. Review and approval via PR
3. Migration plan if applicable

All PRs MUST verify compliance with these principles. The AGENTS.md file serves as runtime development guidance.

**Version**: 1.3.0 | **Ratified**: 2026-02-24 | **Last Amended**: 2026-05-28
