# Changelog

All notable changes to Zen are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — Critical Memory System Gap Fixes (2026-06-27)

Second audit pass identified and fixed 7 critical implementation gaps across the self-learning memory system:

#### Signal Persistence & Data Model

- **Fact struct implementation (G1)**: DESIGN.md §4 Phase C specified `Fact` as the core extracted knowledge type, but the struct was missing. Created `fact.rs` with full markdown persistence (`save()`/`load()`/`load_all()`), YAML frontmatter + body format, UUID-based IDs, entity associations, and 12 unit tests. Exported as `pub use fact::Fact` from zen-memory.
- **Signal persistence wiring (G3)**: Three signal types had `to_markdown()`/`from_markdown()` but no file persistence. Added `save(dir)`/`load(path)`/`load_all(dir)` methods to `ReflectionSignal`, `AntiPatternSignal`, and `MentalModelSignal`, following the exact pattern from `correction.rs`. Signals can now be written to and loaded from disk.

#### Search Tier Consistency

- **Tier3 table name fix (G2)**: `tier3.rs` queried `note_meta` but schema creates `notes_meta` (plural). Fixed table name and added JOIN with `notes_fts` to retrieve the `content` column (which exists in FTS5 table but not in metadata table).
- **FTS5 table name unification (G5)**: `tier2.rs` created its own `note_fts` table (6 columns) but `init_kb_schema` creates `notes_fts` (4 columns). Aligned Tier2 to use `notes_fts` + `notes_meta` matching the schema, with proper JOIN for file_path retrieval.

#### Dead Code Cleanup & Naming

- **Dead code removal (G6)**: Removed `update_knowledge()` function (~120 lines including `TECH_KEYWORDS` constant and `find_entity_match()` helper) from `dream.rs`. Cleaned up unused imports (`HashMap`, `EntityData`, `RelationType`, `Relationship`, `WikiCompiler`). Updated doc comments to reference `recompute_entities()` instead.
- **JournalWorker → MemoryCurator rename (G7)**: DESIGN.md §10.1 specifies `MemoryCurator` name. Renamed struct, file (`journal_worker.rs` → `memory_curator.rs`), worker ID (`journal-worker` → `memory-curator`), and all references in `workers/mod.rs`, `scheduler/mod.rs`, `dream.rs`, and `marker_state.rs`.
- **recompute_entities doc fix (G8)**: Doc comment incorrectly stated function was a no-op stub returning `Ok(0)`. Implementation was already correct (scans `wiki/entities/*.md` and upserts to graph.db). Fixed doc to accurately describe the real behavior.

### Added — Self-Learning Memory Integration Audit Fixes (2026-06-27)

Reverse audit of self-learning memory system against DESIGN.md identified 15 issues across 3 review lenses (Engineering, CEO, Memory Design). All fixable issues addressed.

#### Signal Flow Wiring

- **Priority scoring injection (E1)**: `priority_items` was computed every session via `format_priority_for_prompt()` but never injected into the prompt. Added `PromptAssemblyBuilder::priority_items()` builder method and wired `signals.priority_items` into executor injection loop. DESIGN.md §8.4 attention allocation now functional.
- **Signal sections in all prompt paths (E2)**: Self-learning signal sections (corrections, feedback, beliefs, virtue_logs, reflections, mental_models, decisions) were only rendered in `build_default_18_sections()`. Coordinator, agent-definition, and custom prompt paths silently dropped all signals. Added signal section rendering to all 4 prompt assembly paths.
- **Reinforcement tracker wiring (C7)**: `ReinforcementTracker::record_retrieval()` existed but was never called. Wired into `SelfLearningSignals::load()` so every signal retrieval increments hit-count. DESIGN.md §8.2 reinforcement/decay rules now active.

#### Write/Read Path Corrections

- **Mental model promotion (E3)**: WisdomSynthesizer wrote mental model candidates to `wiki/wisdom/suggestions/` but `SelfLearningSignals::load_mental_models()` read from `wiki/wisdom/models/`. Fixed write path to write accepted candidates directly to `wiki/wisdom/models/{slug}.md`.
- **Anti-pattern promotion (E4)**: Same disconnection — WisdomSynthesizer wrote to `suggestions/`, SessionJournaler's `check_anti_pattern_match()` read from `wiki/wisdom/anti-patterns/`. Fixed write path to write to `wiki/wisdom/anti-patterns/{slug}.md`.
- **Dead memory_content removal (E5)**: `SelfLearningSignals::memory_content` was loaded but never used (IdentityContext handles MEMORY.md injection separately). Removed field and load function to eliminate redundant file I/O.

#### KPI and Product Features

- **Commitment completion rate KPI (C1)**: DESIGN.md §15.2 defines `commitment_completion_rate` as the system's success metric. Implemented `compute_commitment_completion_rate()` in new `kpi.rs` module. KPI = (commitments with >=1 milestone achieved within review_at window) / (total commitments). Exposed via `zen memory kpi` command.
- **Anti-talk indicator (C2)**: DESIGN.md §8.5 defines `mention_to_achievement_ratio`. Ratio > 5 triggers "空谈警报". Implemented in `CommitmentTracker::compute_anti_talk_indicator()`.
- **Echo chamber mitigation (C6)**: Added monthly "fresh-eyes" extraction mode to SessionJournaler. When `fresh_eyes_mode = true`, M4/M5 context injection is skipped, allowing unbiased extraction. DESIGN.md §15.1 risk #3 mitigation.

#### Worker Fixes

- **ReflectionWorker LLM synthesis (E7)**: DESIGN.md §10.2 specifies ReflectionWorker as "Yes LLM" — synthesize M2 reflections into M4 anti-pattern candidates. Was a pure file parser. Added LLM synthesis call that generates anti-pattern candidates from aggregated reflections.
- **recompute_entities() implementation (E6)**: Was a stub returning `Ok(0)`. Implemented graph rebuild logic: scan `wiki/entities/*.md` frontmatter, upsert entities into graph.db, normalize via `entity_aliases` table.
- **Prompt injection detection (E8)**: `safety_hook.rs:134` TODO. Implemented pattern-based detection for common prompt injection patterns in tool arguments (role hijacking, delimiter injection, instruction override).

### Changed

- `PromptAssemblyBuilder` now has 8 signal section builder methods (was 7). Added `priority_items()`.
- `SelfLearningSignals` now has 8 fields (was 9). Removed `memory_content`, added `priority_items` injection.
- `WisdomSynthesizer` writes mental models to `wiki/wisdom/models/{slug}.md` and anti-patterns to `wiki/wisdom/anti-patterns/{slug}.md` (was `wiki/wisdom/suggestions/{date}.md` for both).
- `SessionJournaler` accepts `fresh_eyes_mode: bool` parameter. When true, skips M4/M5 injection.

### Deprecated

- `SelfLearningSignals::memory_content` field — removed (was dead code, IdentityContext handles MEMORY.md).

### Known Limitations (Documented for Future Phases)

- **Self-Model 6-layer axis (C3)**: DESIGN.md §5.2 specifies 6 introspective layers for `self` entity (Knowledge, Skill, SocialRole, SelfConcept, Trait, Motivation) with `humility_score` and `optionality_count`. Not yet implemented — Phase E+ feature.
- **GoalNode/PathNode/BeliefNode graph types (C4)**: DESIGN.md §6.1 specifies 3 new graph node types. Not yet implemented — Phase E+ feature.
- **PARA directory structure (C5)**: DESIGN.md §5.1 specifies projects/areas/resources/archive structure. Deferred per DESIGN.md §15.2 recommendation "tags first, physical restructure later."

## [0.1.0] - 2026-05-15

### Added

- Initial release of Zen CLI productivity suite
- 12 workspace crates with binary/library split
- 24 CLI commands with clap derive
- 5-tier search pipeline (ripgrep → FTS5 → vec0 → graph → LLM)
- 13 LLM providers across 3 protocol types
- 13 agents in 4 tiers with 4-channel blackboard
- Self-learning memory system: 10 signal types, 14 workers, 3 quality gates
- WASM sandbox via wasmtime, MCP server support
- macOS Keychain integration
