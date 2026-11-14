//! Per-turn ambient context — the single sanctioned source for turn-scoped
//! sensitivity/knowledge/history (spec D30, SDD review F1+OV-1).
//!
//! # Why this exists
//!
//! One daemon-wide orchestrator serves all hosted sessions, and sessions
//! overlap (Parallel Execution Contract §4). Before D30 the turn's
//! sensitivity/knowledge/history traveled through last-writer-wins shared
//! cells written at turn entry and read later at delegate composition and
//! tool dispatch — a concurrent Public session could overwrite a
//! Confidential session's context inside a seconds-wide window (cloud
//! routing leak + cross-session knowledge/history injection).
//!
//! # Scope logic (Constitution XV)
//!
//! - Functionality: `TURN_CONTEXT` carries `{sensitivity, knowledge,
//!   history}` for the duration of one orchestrator turn. Turn-path
//!   consumers (delegate composition, the confidentiality hook, the plan
//!   quality gate, the `plan.todos` sensitivity tag) read it FIRST and fall
//!   back to the shared cells only when it is unset.
//! - User impact: concurrent hosted sessions can no longer leak policy or
//!   context across each other; single-session behavior is byte-identical.
//! - Default: unset — direct CLI/test callers and scheduler workers outside
//!   any turn scope keep the documented shared-cell fallback (D30 §4:
//!   cells are demoted, not deleted; deletion is a later trigger-based
//!   cleanup once every consumer reads TurnContext).
//! - Interaction: task-locals survive `block_in_place` (`run_blocking_llm`)
//!   and `join_all` (plan batches) but NOT `tokio::spawn` — background
//!   admission captures the context synchronously and scopes it around the
//!   spawned future (`BACKGROUND_TASK.scope` precedent).
//!
//! Layering: defined here in zen-core because zen-plugin hooks (the
//! confidentiality cloud-tool gate) must read it and zen-agents writes it.

use crate::types::{RetrievedNote, Sensitivity};

/// One mirrored parent-conversation entry (D25 SC-021 `history` fork).
///
/// Moved from `zen-agents::delegate_task` to zen-core so the turn-scoped
/// task-local can carry history without a dependency cycle. The producer
/// (orchestrator turn entry) strips tool-call noise; composition strips
/// again defensively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentHistoryMessage {
    /// Lowercase role label: `"user"`, `"assistant"`, or `"developer"`.
    pub role: String,
    /// Message prose (assistant entries must already be tool-block-stripped
    /// by the producer; composition strips again defensively).
    pub content: String,
}

/// The ambient context of one orchestrator turn (D30).
///
/// Built at turn entry AFTER knowledge injection (the carried knowledge is
/// the final injected M1 block, not the pre-retrieval session state) and
/// scoped around the remainder of `execute`/`execute_stream`.
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// Session sensitivity policy — routing + hook gates classify under it.
    pub sensitivity: Sensitivity,
    /// The turn's retrieved/injected M1 knowledge block.
    pub knowledge: Vec<RetrievedNote>,
    /// Stripped parent conversation (user/assistant prose only) — the
    /// `history` fork source.
    pub history: Vec<ParentHistoryMessage>,
}

tokio::task_local! {
    /// Per-turn ambient context (D30). Scoped by the orchestrator around
    /// `execute`/`execute_stream`; read via the accessors below. Unset ⇒
    /// consumers fall back to the shared cells (non-turn callers).
    pub static TURN_CONTEXT: TurnContext;
}

/// The turn's sensitivity, or `None` outside any turn scope.
///
/// Turn-path consumers (confidentiality hook, delegate composition, plan
/// gate, `plan.todos` tag) call this FIRST; `None` means the caller is a
/// non-turn path and the shared-cell fallback applies.
#[must_use]
pub fn current_sensitivity() -> Option<Sensitivity> {
    TURN_CONTEXT.try_with(|ctx| ctx.sensitivity).ok()
}

/// The turn's knowledge block (cloned), or `None` outside any turn scope.
///
/// Same clone cost as the shared-cell snapshot it replaces on the turn path.
#[must_use]
pub fn current_knowledge() -> Option<Vec<RetrievedNote>> {
    TURN_CONTEXT.try_with(|ctx| ctx.knowledge.clone()).ok()
}

/// The turn's stripped parent conversation (cloned), or `None` outside any
/// turn scope.
#[must_use]
pub fn current_history() -> Option<Vec<ParentHistoryMessage>> {
    TURN_CONTEXT.try_with(|ctx| ctx.history.clone()).ok()
}

/// Snapshot the whole turn context (cloned), or `None` outside any turn
/// scope. Used by background admission to capture the context synchronously
/// before `tokio::spawn` (task-locals do not cross spawn boundaries).
#[must_use]
pub fn current() -> Option<TurnContext> {
    TURN_CONTEXT.try_with(Clone::clone).ok()
}
