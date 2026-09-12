//! Zen-native telemetry schema (replaces rig-tap 0.3.0). Codex-style: own the
//! domain schema, emit via tracing. MIT OR Apache-2.0 derived schema shape;
//! wire format is zen's own.

use std::time::{SystemTime, UNIX_EPOCH};

/// `tracing` target all zen telemetry events are emitted on. Consumers filter
/// on this target to route events without parsing the JSON payload.
pub const EVENT_TARGET: &str = "zen_tap";

/// Wire-schema version. Bump on any breaking change to [`TelemetryEvent`].
pub const SCHEMA_VERSION: u32 = 1;

/// Prompt-lifecycle event payloads. Tagged on the wire as `"type"` with
/// snake_case variant names (`prompt_started`, `prompt_completed`,
/// `prompt_failed`).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// A prompt is about to be sent to the model provider.
    PromptStarted {
        /// Model name as declared on the agent.
        model: String,
        /// Number of messages in the history at the time of the call.
        messages_in: usize,
    },
    /// A prompt finished; the model returned a completion response.
    PromptCompleted {
        /// Model name as reported by the provider response.
        model: String,
        /// Provider-reported input tokens, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_in: Option<u64>,
        /// Provider-reported output tokens, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_out: Option<u64>,
        /// Number of tokens pulled from prefix cache, if reported.
        #[serde(skip_serializing_if = "Option::is_none")]
        cached_tokens_in: Option<u64>,
        /// Number of reasoning (chain-of-thought) tokens generated, if reported.
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<u64>,
        /// Producer-computed USD cost, if available.
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        /// Reason why generation stopped (e.g. "stop", "length", "tool_calls").
        #[serde(skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
        /// Provider response ID, if supplied.
        #[serde(skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        /// Server-side chain ancestor when the producer is on a stateful
        /// endpoint. `None` for one-shot completions or the first turn.
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_response_id: Option<String>,
        /// Time elapsed between call start and the first token yielded, if streaming.
        #[serde(skip_serializing_if = "Option::is_none")]
        time_to_first_token_ms: Option<u64>,
        /// Total time elapsed for the prompt execution, if the producer tracks it.
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// A prompt failed to complete successfully.
    PromptFailed {
        /// Model name as reported by the provider/system.
        model: String,
        /// Classification of the error.
        error_class: ErrorClass,
        /// Displayed message or stringified error.
        message: String,
        /// Indicates if the failure was deemed retriable.
        retriable: bool,
        /// Provider-specific error code if available.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_error_code: Option<String>,
        /// HTTP status code if the error was a transport or server error.
        #[serde(skip_serializing_if = "Option::is_none")]
        http_status: Option<u16>,
    },
}

/// High-level classification of a prompt failure. Grows on demand — only
/// `Unknown` is emitted today.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorClass {
    /// The failure could not be classified into a more specific class.
    Unknown,
}

/// Flat, versioned envelope for a single telemetry event. The `kind` payload
/// is flattened so the `type` tag and variant fields sit at the top level.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TelemetryEvent {
    /// Wire-schema version ([`SCHEMA_VERSION`]).
    pub version: u32,
    /// Correlation id for the conversation the event belongs to.
    pub conversation_id: String,
    /// Wall-clock timestamp in milliseconds since the Unix epoch.
    pub occurred_at_millis: u64,
    /// Prompt-lifecycle payload, flattened into the envelope.
    #[serde(flatten)]
    pub kind: EventKind,
}

impl EventKind {
    /// Returns the wire `type` discriminant for this event.
    pub fn name(&self) -> &'static str {
        match self {
            EventKind::PromptStarted { .. } => "prompt_started",
            EventKind::PromptCompleted { .. } => "prompt_completed",
            EventKind::PromptFailed { .. } => "prompt_failed",
        }
    }
}

/// Serialize and emit a single telemetry event on [`EVENT_TARGET`].
///
/// The JSON-encoded [`TelemetryEvent`] rides the `event` field; `kind`,
/// `conversation_id`, and `version` are surfaced as scalar `tracing`
/// attributes so collectors can route without parsing JSON. Serialization
/// failure degrades to a `serialize_error` envelope rather than panicking on
/// the hot path.
pub fn emit_kind(conversation_id: &str, kind: EventKind) {
    let occurred_at_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let event = TelemetryEvent {
        version: SCHEMA_VERSION,
        conversation_id: conversation_id.to_string(),
        occurred_at_millis,
        kind,
    };
    let json = serde_json::to_string(&event).unwrap_or_else(|e| {
        format!(
            "{{\"type\":\"serialize_error\",\"error\":{}}}",
            serde_json::to_string(&e.to_string()).unwrap_or_else(|_| "\"unknown\"".to_string())
        )
    });
    tracing::info!(
        target: EVENT_TARGET,
        event = %json,
        kind = event.kind.name(),
        conversation_id = %conversation_id,
        version = SCHEMA_VERSION,
        "zen telemetry event",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(kind: EventKind) -> TelemetryEvent {
        TelemetryEvent {
            version: SCHEMA_VERSION,
            conversation_id: "conv-1".into(),
            occurred_at_millis: 1_715_000_000_000,
            kind,
        }
    }

    #[test]
    fn prompt_started_serializes_with_type_tag_and_version() {
        let json = serde_json::to_string(&envelope(EventKind::PromptStarted {
            model: "gpt-4o".into(),
            messages_in: 3,
        }))
        .unwrap();
        assert!(json.contains("\"type\":\"prompt_started\""));
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"model\":\"gpt-4o\""));
        assert!(json.contains("\"messages_in\":3"));
    }

    #[test]
    fn prompt_completed_serializes_and_omits_none_fields() {
        let json = serde_json::to_string(&envelope(EventKind::PromptCompleted {
            model: "gpt-4o".into(),
            tokens_in: Some(10),
            tokens_out: Some(20),
            cached_tokens_in: None,
            reasoning_tokens: None,
            cost_usd: None,
            finish_reason: None,
            response_id: None,
            previous_response_id: None,
            time_to_first_token_ms: None,
            duration_ms: Some(742),
        }))
        .unwrap();
        assert!(json.contains("\"type\":\"prompt_completed\""));
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"tokens_in\":10"));
        assert!(json.contains("\"duration_ms\":742"));
        // Clean log lines: absent Option fields are omitted, not null.
        assert!(!json.contains("cached_tokens_in"));
        assert!(!json.contains("cost_usd"));
        assert!(!json.contains("finish_reason"));
    }

    #[test]
    fn prompt_failed_serializes_with_error_class() {
        let json = serde_json::to_string(&envelope(EventKind::PromptFailed {
            model: "gpt-4o".into(),
            error_class: ErrorClass::Unknown,
            message: "boom".into(),
            retriable: false,
            provider_error_code: None,
            http_status: None,
        }))
        .unwrap();
        assert!(json.contains("\"type\":\"prompt_failed\""));
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"error_class\":\"unknown\""));
        assert!(json.contains("\"retriable\":false"));
        assert!(!json.contains("provider_error_code"));
        assert!(!json.contains("http_status"));
    }

    #[test]
    fn event_kind_names_match_wire_types() {
        assert_eq!(
            EventKind::PromptStarted {
                model: "m".into(),
                messages_in: 1,
            }
            .name(),
            "prompt_started"
        );
        assert_eq!(
            EventKind::PromptCompleted {
                model: "m".into(),
                tokens_in: None,
                tokens_out: None,
                cached_tokens_in: None,
                reasoning_tokens: None,
                cost_usd: None,
                finish_reason: None,
                response_id: None,
                previous_response_id: None,
                time_to_first_token_ms: None,
                duration_ms: None,
            }
            .name(),
            "prompt_completed"
        );
        assert_eq!(
            EventKind::PromptFailed {
                model: "m".into(),
                error_class: ErrorClass::Unknown,
                message: "e".into(),
                retriable: false,
                provider_error_code: None,
                http_status: None,
            }
            .name(),
            "prompt_failed"
        );
    }
}
