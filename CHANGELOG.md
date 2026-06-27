# Changelog

All notable changes to Zen are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
