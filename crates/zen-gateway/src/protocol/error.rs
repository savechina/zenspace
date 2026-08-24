//! Error catalog — closed, additive-only set of JSON-RPC std codes plus the
//! zen `-32000..-32099` range, exactly per contracts/02 §Error catalog.
//!
//! PURPOSE: Single source of truth for every error code, name, and data
//! schema that can appear in a Q2/Q4 `error` member. Codes never change
//! meaning (data-model E6); new codes are additive MINOR bumps.
//!
//! USAGE: Handlers return [`RpcError`] values built by the constructors
//! below; responders materialize them via [`RpcErrorBody::from_error`].
//!
//! EXPECTED: `version_mismatch()` carries `serverVersion` +
//! `serverProtocolVersion` + `reason` + `recovery`; `method_not_found()`
//! carries `supportedMethods`; every constructor's `since` minor is noted
//! in its doc comment.
//!
//! ERRORS: Constructing an error outside the catalog is impossible — there
//! are no free-form constructors.

use serde_json::{Value, json};

/// A materialized catalog error: code + stable name + message + optional
/// data payload. Plain struct (no thiserror) because these are wire values,
/// not Rust control-flow errors.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    /// Integer error code from the closed catalog.
    pub code: i32,
    /// Stable kebab-case identifier string, exactly per contracts/02 table.
    pub name: &'static str,
    /// Human-readable, actionable message.
    pub message: String,
    /// Optional structured payload whose schema is fixed per catalog row.
    pub data: Option<Value>,
}

impl RpcError {
    /// -32700 parse error · since 1.0 · JSON-RPC std.
    pub fn parse_error(detail: &str) -> Self {
        Self {
            code: -32700,
            name: "parse-error",
            message: format!("invalid JSON: {detail}"),
            data: None,
        }
    }

    /// -32600 invalid request · since 1.0 · JSON-RPC std.
    pub fn invalid_request(detail: &str) -> Self {
        Self {
            code: -32600,
            name: "invalid-request",
            message: format!("invalid request frame: {detail}"),
            data: None,
        }
    }

    /// -32602 invalid params · since 1.0 · JSON-RPC std.
    pub fn invalid_params(method: &str, detail: &str) -> Self {
        Self {
            code: -32602,
            name: "invalid-params",
            message: format!("invalid params for {method}: {detail}"),
            data: None,
        }
    }

    /// -32601 method not found · since 1.0 · carries
    /// `data.supportedMethods` listing every registry name.
    pub fn method_not_found(supported: &[&'static str]) -> Self {
        Self {
            code: -32601,
            name: "method-not-found",
            message: "unknown method".to_string(),
            data: Some(json!({ "supportedMethods": supported })),
        }
    }

    /// -32603 internal error · since 1.0 · JSON-RPC std.
    pub fn internal(detail: &str) -> Self {
        Self {
            code: -32603,
            name: "internal",
            message: detail.to_string(),
            data: None,
        }
    }

    /// -32000 not-initialized · since 1.0 · request arrived before the
    /// `initialize`/`initialized` handshake completed.
    pub fn not_initialized() -> Self {
        Self {
            code: -32000,
            name: "not-initialized",
            message: "connection not initialized; call initialize first".to_string(),
            data: None,
        }
    }

    /// -32001 version-mismatch · since 1.0 · carries `serverVersion`,
    /// `serverProtocolVersion`, `reason`, `recovery` (FR-004).
    pub fn version_mismatch(
        server_version: &str,
        server_protocol_version: &str,
        reason: &str,
    ) -> Self {
        Self {
            code: -32001,
            name: "version-mismatch",
            message: format!("protocol version mismatch: {reason}"),
            data: Some(json!({
                "serverVersion": server_version,
                "serverProtocolVersion": server_protocol_version,
                "reason": reason,
                "recovery": "restart gateway with matching version",
            })),
        }
    }

    /// -32002 store-unavailable · since 1.0 · carries `storeHealth`.
    pub fn store_unavailable(health: &str) -> Self {
        Self {
            code: -32002,
            name: "store-unavailable",
            message: "memory store unavailable".to_string(),
            data: Some(json!({ "storeHealth": health })),
        }
    }

    /// -32003 session-not-found · since 1.1 · carries `sessionId`.
    pub fn session_not_found(session_id: &str) -> Self {
        Self {
            code: -32003,
            name: "session-not-found",
            message: format!("session not found: {session_id}"),
            data: Some(json!({ "sessionId": session_id })),
        }
    }

    /// -32004 turn-already-completed · since 1.1 · carries `response`
    /// (the final result for idempotent replay, no re-execution).
    pub fn turn_already_completed(response: Value) -> Self {
        Self {
            code: -32004,
            name: "turn-already-completed",
            message: "turn already completed; returning final result".to_string(),
            data: Some(json!({ "response": response })),
        }
    }

    /// -32010 approval-unsupported · since 1.1 · carrier/session declared
    /// `approvals:false`; fail-fast at turn submit.
    pub fn approval_unsupported() -> Self {
        Self {
            code: -32010,
            name: "approval-unsupported",
            message: "approvals not supported on this connection".to_string(),
            data: None,
        }
    }

    /// -32011 approval-timeout · since 1.1 · carries `turnId`; turn
    /// cancels and an audit record is written.
    pub fn approval_timeout(turn_id: &str) -> Self {
        Self {
            code: -32011,
            name: "approval-timeout",
            message: format!("approval request timed out for turn {turn_id}"),
            data: Some(json!({ "turnId": turn_id })),
        }
    }

    /// -32012 rate-limited · since 1.1 · carries `retryAfterMs`.
    pub fn rate_limited(retry_after_ms: u64) -> Self {
        Self {
            code: -32012,
            name: "rate-limited",
            message: "too many requests".to_string(),
            data: Some(json!({ "retryAfterMs": retry_after_ms })),
        }
    }

    /// -32020 guard-rejected · since 1.1 · carries `guard` and `reason`
    /// (watchdog/breaker/doom-loop/slow-pool).
    pub fn guard_rejected(guard: &str, reason: &str) -> Self {
        Self {
            code: -32020,
            name: "guard-rejected",
            message: format!("rejected by {guard} guard: {reason}"),
            data: Some(json!({ "guard": guard, "reason": reason })),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} ({})", self.code, self.name, self.message)
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zen_codes_and_names_match_catalog() {
        let cases: [(i32, &'static str); 9] = [
            (-32000, "not-initialized"),
            (-32001, "version-mismatch"),
            (-32002, "store-unavailable"),
            (-32003, "session-not-found"),
            (-32004, "turn-already-completed"),
            (-32010, "approval-unsupported"),
            (-32011, "approval-timeout"),
            (-32012, "rate-limited"),
            (-32020, "guard-rejected"),
        ];
        let built = [
            RpcError::not_initialized(),
            RpcError::version_mismatch("0.0.8", "1.0", "major mismatch"),
            RpcError::store_unavailable("unavailable"),
            RpcError::session_not_found("s1"),
            RpcError::turn_already_completed(json!("done")),
            RpcError::approval_unsupported(),
            RpcError::approval_timeout("t1"),
            RpcError::rate_limited(500),
            RpcError::guard_rejected("watchdog", "deadline"),
        ];
        for ((code, name), err) in cases.iter().zip(built) {
            assert_eq!(err.code, *code);
            assert_eq!(err.name, *name);
        }
    }

    #[test]
    fn version_mismatch_data_shape() {
        let e = RpcError::version_mismatch("0.0.8", "1.0", "client too new");
        let d = e.data.unwrap();
        assert_eq!(d["serverVersion"], "0.0.8");
        assert_eq!(d["serverProtocolVersion"], "1.0");
        assert_eq!(d["reason"], "client too new");
        assert_eq!(d["recovery"], "restart gateway with matching version");
    }

    #[test]
    fn method_not_found_lists_supported() {
        let e = RpcError::method_not_found(&["initialize", "health/status"]);
        let d = e.data.unwrap();
        assert_eq!(
            d["supportedMethods"],
            json!(["initialize", "health/status"])
        );
    }
}
