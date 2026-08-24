//! JSON-RPC 2.0 wire envelope — Frame enum covering the four interaction
//! quadrants plus notifications (contracts/00 §Envelope).
//!
//! PURPOSE: Defines the single carrier-independent frame type every transport
//! carries. Wire format is JSON-RPC 2.0; UDS serializes one frame per JSONL
//! line, the in-process carrier passes typed frames (round-tripping through
//! serde only in test mode).
//!
//! USAGE: Surfaces never build raw frames — they call typed helpers on
//! [`Frame`] (or the future L3 client). Quadrant classification is static by
//! method (MethodRegistry row), never inferred from the channel.
//!
//! EXPECTED: Responses echo the initiator's id (never mint); notifications
//! carry no id; a request frame serializes with `"method"` + `"id"`.
//!
//! ERRORS: Malformed payloads are rejected by the JSON-RPC peer with
//! -32700 (unparsable) / -32600 (invalid frame shape).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Client-minted request id — Q1 frames only. u64 keeps ids trivially
/// JSON-safe and monotonic per client.
pub type RequestId = u64;

/// Server-minted request id for Q3 frames (e.g. `approval/request`).
/// String form (`"srv-<n>"`) can never collide with client u64 ids on the
/// same connection.
pub type ServerRequestId = String;

/// One JSON-RPC 2.0 frame in any of the five shapes used by the gateway
/// (data-model E4). The untagged serde form matches the wire discriminants:
/// presence of `id` + `method` = request, `id` + `result`/`error` =
/// response, `method` alone = notification.
///
/// Serialization examples:
///
/// ```json
/// {"jsonrpc":"2.0","id":1,"method":"session/turn","params":{...}}
/// {"jsonrpc":"2.0","id":1,"result":{"turnId":"t_01"}}
/// {"jsonrpc":"2.0","id":"srv-1","method":"approval/request","params":{...}}
/// {"jsonrpc":"2.0","id":"srv-1","result":{"decision":"approve"}}
/// {"jsonrpc":"2.0","method":"session/event","params":{...}}
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Frame {
    /// Q1 — client request: expects a [`Frame::ServerResponse`] echoing the id.
    ClientRequest {
        /// Constant `"2.0"` — validated by [`Frame::validate_wire`].
        jsonrpc: JsonRpc,
        /// Client-minted id; echoed verbatim in the response (rpcId echo rule).
        id: RequestId,
        /// Registry method name (`domain/verb`).
        method: String,
        /// Method params object; omitted methods use [`params_none`].
        #[serde(default = "params_none")]
        params: Value,
    },
    /// Q2 — server response to a client request. Exactly one of
    /// `result`/`error` is `Some`; the id is echoed, never minted.
    ServerResponse {
        jsonrpc: JsonRpc,
        /// Echo of the Q1 id.
        id: RequestId,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<RpcErrorBody>,
    },
    /// Q3 — server request (e.g. `approval/request`): expects a
    /// [`Frame::ClientResponse`] echoing the id.
    ServerRequest {
        jsonrpc: JsonRpc,
        /// Server-minted `"srv-N"` id.
        id: ServerRequestId,
        method: String,
        #[serde(default = "params_none")]
        params: Value,
    },
    /// Q4 — client response to a server request; id echoes the Q3 id.
    ClientResponse {
        jsonrpc: JsonRpc,
        /// Echo of the Q3 id.
        id: ServerRequestId,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<RpcErrorBody>,
    },
    /// Notification — either direction, no id, never answered. Unknown
    /// notifications are silently ignored (forward-compat rule, contract 01).
    Notification {
        jsonrpc: JsonRpc,
        method: String,
        #[serde(default = "params_none")]
        params: Value,
    },
}

/// Marker for the JSON-RPC version constant; serializes as `"2.0"`.
/// Construction succeeds only via [`JsonRpc::new`], which enforces the
/// exact string (rejecting e.g. `"2"` or `"2.00"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonRpc(());

impl Serialize for JsonRpc {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpc {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value == "2.0" {
            Ok(JsonRpc(()))
        } else {
            Err(serde::de::Error::custom(
                "jsonrpc must be exactly \"2.0\" (Frame::validate_wire rejects others)",
            ))
        }
    }
}

impl JsonRpc {
    /// Returns the constant if the wire string was exactly `"2.0"`.
    /// The serde rename makes any other value fail deserialization into
    /// [`JsonRpc`], which surfaces as a -32700/-32600 at the carrier.
    pub const fn new() -> Self {
        JsonRpc(())
    }
}

impl Default for JsonRpc {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for JsonRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("2.0")
    }
}

/// serde default for optional params: `{}` on the wire when omitted.
fn params_none() -> Value {
    Value::Object(serde_json::Map::new())
}

/// JSON body of the `error` member in Q2/Q4 frames (contracts/01: `code`
/// int + `name` stable kebab string + `message` + optional `data`).
///
/// This is the wire shape only; canonical codes/names live in
/// [`crate::protocol::error`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcErrorBody {
    /// Integer code — std JSON-RPC or the zen `-32000..-32099` range.
    pub code: i32,
    /// Stable kebab-case identifier (e.g. `"version-mismatch"`).
    pub name: String,
    /// Human-readable, actionable message (FR-004).
    pub message: String,
    /// Optional structured payload; schemas per error catalog row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcErrorBody {
    /// Builds a body from a catalog error, attaching `data` when present.
    /// Used by responders when materializing a catalog entry onto a frame.
    pub fn from_error(err: crate::protocol::error::RpcError) -> Self {
        Self {
            code: err.code,
            name: err.name.to_string(),
            message: err.message.to_string(),
            data: err.data,
        }
    }
}

impl std::fmt::Display for RpcErrorBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} ({})", self.code, self.name, self.message)
    }
}

impl std::error::Error for RpcErrorBody {}

impl Frame {
    /// Method name for request/notification frames; `None` for responses.
    pub fn method(&self) -> Option<&str> {
        match self {
            Frame::ClientRequest { method, .. }
            | Frame::ServerRequest { method, .. }
            | Frame::Notification { method, .. } => Some(method),
            Frame::ServerResponse { .. } | Frame::ClientResponse { .. } => None,
        }
    }

    /// Builds a Q1 frame with an empty params object.
    pub fn request(id: RequestId, method: &str) -> Self {
        Frame::ClientRequest {
            jsonrpc: JsonRpc::new(),
            id,
            method: method.to_string(),
            params: params_none(),
        }
    }

    /// Builds a Q1 frame with the given params value.
    pub fn request_with(id: RequestId, method: &str, params: Value) -> Self {
        Frame::ClientRequest {
            jsonrpc: JsonRpc::new(),
            id,
            method: method.to_string(),
            params,
        }
    }

    /// Builds a Q2 success response echoing `id`.
    pub fn response(id: RequestId, result: Value) -> Self {
        Frame::ServerResponse {
            jsonrpc: JsonRpc::new(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Builds a Q2 error response echoing `id`.
    pub fn error_response(id: RequestId, err: crate::protocol::error::RpcError) -> Self {
        Frame::ServerResponse {
            jsonrpc: JsonRpc::new(),
            id,
            result: None,
            error: Some(RpcErrorBody::from_error(err)),
        }
    }

    /// Builds a Q3 server request frame.
    pub fn server_request(id: ServerRequestId, method: &str, params: Value) -> Self {
        Frame::ServerRequest {
            jsonrpc: JsonRpc::new(),
            id,
            method: method.to_string(),
            params,
        }
    }

    /// Builds a Q4 success response echoing the Q3 server id.
    pub fn client_response(id: ServerRequestId, result: Value) -> Self {
        Frame::ClientResponse {
            jsonrpc: JsonRpc::new(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Builds a Q4 error response echoing the Q3 server id.
    pub fn client_error(id: ServerRequestId, err: crate::protocol::error::RpcError) -> Self {
        Frame::ClientResponse {
            jsonrpc: JsonRpc::new(),
            id,
            result: None,
            error: Some(RpcErrorBody::from_error(err)),
        }
    }

    /// Builds a notification frame (no id).
    pub fn notification(method: &str, params: Value) -> Self {
        Frame::Notification {
            jsonrpc: JsonRpc::new(),
            method: method.to_string(),
            params,
        }
    }

    /// Rejects structurally impossible frames: a response carrying both
    /// `result` and `error`, or neither. Returns `false` → carrier answers
    /// -32600 invalid-request.
    pub fn validate_wire(&self) -> bool {
        match self {
            Frame::ServerResponse { result, error, .. }
            | Frame::ClientResponse { result, error, .. } => result.is_some() ^ error.is_some(),
            _ => true,
        }
    }

    /// Serializes the frame to a single JSON line (UDS JSONL form: one
    /// frame per line, no embedded newlines).
    ///
    /// # Errors
    /// Fails only if a params/result value is not JSON-serializable —
    /// impossible for values that themselves deserialized from JSON.
    pub fn to_json_line(&self) -> anyhow::Result<String> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }

    /// Parses one JSONL line into a frame; trailing newline optional.
    ///
    /// # Errors
    /// Returns an error for unparsable JSON (-32700 at the carrier) or a
    /// JSON value that fits no envelope shape (-32600).
    pub fn from_json_line(line: &str) -> anyhow::Result<Self> {
        let frame: Frame = serde_json::from_str(line.trim_end())?;
        if !frame.validate_wire() {
            anyhow::bail!("frame carries both result and error");
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_request_wire_form() {
        let f = Frame::request(7, "session/turn");
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains(r#""jsonrpc":"2.0""#));
        assert!(s.contains(r#""id":7"#));
        assert!(s.contains(r#""method":"session/turn""#));
    }

    #[test]
    fn notification_has_no_id() {
        let f = Frame::notification("session/event", serde_json::json!({}));
        let s = serde_json::to_string(&f).unwrap();
        assert!(!s.contains(r#""id""#));
    }

    #[test]
    fn server_request_id_is_string() {
        let f = Frame::server_request("srv-1".into(), "approval/request", serde_json::json!({}));
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains(r#""id":"srv-1""#));
    }

    #[test]
    fn response_result_error_mutex() {
        let ok = Frame::response(1, serde_json::json!({}));
        assert!(ok.validate_wire());
        let bad = Frame::ServerResponse {
            jsonrpc: JsonRpc::new(),
            id: 1,
            result: Some(serde_json::json!(1)),
            error: Some(RpcErrorBody {
                code: -32603,
                name: "internal".into(),
                message: "x".into(),
                data: None,
            }),
        };
        assert!(!bad.validate_wire());
    }

    #[test]
    fn jsonl_round_trip() {
        let f = Frame::request_with(3, "memory/search", serde_json::json!({"query":"rust"}));
        let line = f.to_json_line().unwrap();
        assert!(line.ends_with('\n'));
        let back = Frame::from_json_line(&line).unwrap();
        assert_eq!(f, back);
    }
}
