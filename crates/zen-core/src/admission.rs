//! Pure admission gate for inbound channel work (007 T004, FR-002/FR-003).
//!
//! Host-enforced by construction (research D1, host rule 1): the gate reads
//! ONLY the channel allowlist, accept mode, sender identity, live-ticket
//! thread continuation, and an optional matched inference rule. There is no
//! message-content parameter, so "the message text says I am allowed to do
//! this" is unrepresentable here rather than merely ignored. Callers feed
//! content to the assessment ladder as DATA only.
//!
//! No I/O, no LLM, no async. Deduplication is the caller's (repository)
//! concern — this module decides one already-deduplicated trigger.

pub use crate::config::AcceptMode;
use serde::{Deserialize, Serialize};

/// Effective accept mode (007 deviation closeout, migration 017): a runtime
/// override row (`admission_mode_override`, written by `zen admission mode`)
/// wins over the configured mode; no row leaves config in charge.
///
/// The stored value is validated at write time; an invalid stored value
/// (hand-edited db) degrades loudly through `AcceptMode::parse`'s E11 path
/// (warn + Observe) — never silent, never a gate bypass.
pub fn resolve_effective_accept_mode(
    configured: AcceptMode,
    override_row: Option<&str>,
) -> AcceptMode {
    match override_row {
        None => configured,
        Some(raw) => AcceptMode::parse(Some(raw)),
    }
}

/// How an inbound request is grounded (FR-003) — binary by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdmissionStrength {
    /// The sender is on the channel allowlist, or continues an admitted
    /// ticket's thread.
    Explicit,
    /// An owner-written inference rule matched; never auto-executes on first
    /// sight in v1 (research D5).
    Inferred,
}

/// The gate's verdict for one trigger (data-model.md trigger disposition).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Recorded with the precise reason; nothing executes.
    Rejected { reason: String },
    /// Observe mode: recorded for review. The CALLER enforces zero side
    /// effects (no turn, no reply, no writes) — the gate only decides.
    WouldAccept {
        scope: String,
        strength: AdmissionStrength,
        rule: String,
    },
    /// Auto mode (or a live-ticket continuation): the caller dispatches the
    /// existing hosted-turn pipeline under this mandate.
    Admitted {
        scope: String,
        strength: AdmissionStrength,
        rule: String,
    },
}

impl Disposition {
    /// True when the caller must dispatch work.
    pub fn is_admitted(&self) -> bool {
        matches!(self, Disposition::Admitted { .. })
    }

    /// True when the caller records a rehearsal candidate and does nothing.
    pub fn is_would_accept(&self) -> bool {
        matches!(self, Disposition::WouldAccept { .. })
    }

    /// The rejection reason, when rejected.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Disposition::Rejected { reason } => Some(reason),
            _ => None,
        }
    }
}

/// A complete, recordable decision (FR-006).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionDecision {
    pub disposition: Disposition,
    /// One-line human-readable note for the trust ledger.
    pub policy_note: String,
}

/// Inputs the gate may consume. Deliberately has no content/message field —
/// adding one would make "message text changes permissions" representable.
#[derive(Debug, Clone, Copy)]
pub struct GateInput<'a> {
    pub mode: AcceptMode,
    /// The existing `[channels.qqbot] allowed_users` list: one plain string
    /// list matching BOTH group-member and C2C openid namespaces.
    pub allowlist: &'a [String],
    pub sender: &'a str,
    /// True when this message continues a thread with a LIVE admitted ticket.
    pub is_thread_continuation: bool,
    /// Present when an owner-written inference rule matched.
    pub matched_inference_rule: Option<&'a str>,
}

/// Rule label for an allowlist-grounded decision.
pub const RULE_ALLOWLIST: &str = "allowlist";
/// Rule label for a live-ticket thread continuation.
pub const RULE_THREAD_CONTINUATION: &str = "thread-continuation";
/// Scope label for an allowlist-grounded decision.
pub const SCOPE_SENDER: &str = "sender";
/// Scope label for a live-ticket thread continuation.
pub const SCOPE_THREAD: &str = "thread";
/// Scope label for an inference-rule-grounded decision.
pub const SCOPE_RULE: &str = "rule";

/// Evaluate one trigger (pure function of the inputs).
///
/// Order of adjudication:
/// 1. Live-ticket thread continuation ⇒ `Admitted` with `Explicit` strength,
///    regardless of allowlist or mode — it rides an already-granted mandate
///    (FR-014: mode changes affect future admissions only).
/// 2. No explicit identity and no matched rule ⇒ `Rejected`
///    ("sender not allowlisted"), in every mode.
/// 3. `Off` ⇒ `Rejected` ("mode off"), recorded, no execution.
/// 4. `Observe` ⇒ `WouldAccept` (explicit or inferred).
/// 5. `Auto`: explicit ⇒ `Admitted`; inferred ⇒ `Rejected`
///    ("inferred requires prior class review", v1 honesty per D5).
pub fn evaluate(input: &GateInput<'_>) -> AdmissionDecision {
    if input.is_thread_continuation {
        return admitted(
            SCOPE_THREAD,
            AdmissionStrength::Explicit,
            RULE_THREAD_CONTINUATION,
            "live-ticket thread continuation: rides the granted mandate (FR-014)",
        );
    }

    let grounded = if input.allowlist.iter().any(|id| id == input.sender) {
        Some((SCOPE_SENDER, AdmissionStrength::Explicit, RULE_ALLOWLIST))
    } else {
        input
            .matched_inference_rule
            .map(|rule| (SCOPE_RULE, AdmissionStrength::Inferred, rule))
    };

    let Some((scope, strength, rule)) = grounded else {
        return rejected("sender not allowlisted");
    };

    match input.mode {
        AcceptMode::Off => rejected("mode off"),
        AcceptMode::Observe => AdmissionDecision {
            disposition: Disposition::WouldAccept {
                scope: scope.to_string(),
                strength,
                rule: rule.to_string(),
            },
            policy_note:
                "accept_mode=observe: would-accept recorded for review; zero execution (SC-003)"
                    .to_string(),
        },
        AcceptMode::Auto => match strength {
            AdmissionStrength::Explicit => {
                admitted(scope, strength, rule, "accept_mode=auto: admitted")
            }
            AdmissionStrength::Inferred => rejected("inferred requires prior class review"),
        },
    }
}

fn admitted(scope: &str, strength: AdmissionStrength, rule: &str, note: &str) -> AdmissionDecision {
    AdmissionDecision {
        disposition: Disposition::Admitted {
            scope: scope.to_string(),
            strength,
            rule: rule.to_string(),
        },
        policy_note: note.to_string(),
    }
}

fn rejected(reason: &str) -> AdmissionDecision {
    AdmissionDecision {
        disposition: Disposition::Rejected {
            reason: reason.to_string(),
        },
        policy_note: format!("rejected: {reason}; recorded, no execution"),
    }
}
