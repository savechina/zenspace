//! L2 protocol layer — carrier-independent envelope, method registry, error
//! catalog, and version/capability negotiation (contracts/00).
//!
//! PURPOSE: Everything that defines the *conversation* between gateway
//! client and server, with zero transport knowledge. L1 carriers move
//! [`Frame`]s; L3 clients consume this module; business services (L0) are
//! never reached from here.
//!
//! USAGE: Servers validate handshakes via [`HandshakeState`], negotiate
//! versions via [`negotiate_version`], and answer frames via
//! [`DispatchServer`](crate::server::dispatch::DispatchServer). Clients
//! build initialize params via [`initialize_params`].
//!
//! EXPECTED: `initialize` result echoes `{protocolVersion, serverInfo
//! {name:"zen-gateway", version}, capabilities}`; version acceptance is
//! equal-major && client-minor ≤ server-minor; pre-handshake requests
//! fail with -32000.
//!
//! ERRORS: Version failure → -32001 with `serverVersion`,
//! `serverProtocolVersion`, `reason`, `recovery`.

pub mod envelope;
pub mod error;
pub mod methods;

pub use envelope::{Frame, JsonRpc, RequestId, RpcErrorBody, ServerRequestId};
pub use error::RpcError;
pub use methods::{METHODS, MethodMeta, MethodRegistry, Phase, Quadrant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Protocol version this crate speaks (server-minor for negotiation).
pub const SERVER_PROTOCOL_VERSION: &str = "1.0";

/// Server name advertised in the initialize result (contracts/02).
pub const SERVER_NAME: &str = "zen-gateway";

/// Parsed `"MAJOR.MINOR"` protocol version (data-model E5). Construct via
/// [`ProtocolVersion::parse`] — the wire string form round-trips through
/// `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    /// Major component; mismatch refuses the handshake (MAJOR rule).
    pub major: u32,
    /// Minor component; client ≤ server is required (MINOR rule).
    pub minor: u32,
}

impl ProtocolVersion {
    /// Parses `"MAJOR.MINOR"` (e.g. `"1.0"`); rejects other shapes.
    ///
    /// # Errors
    /// Returns a message-bearing error for malformed strings — the caller
    /// answers the frame with -32001 (handshake) or -32602 (elsewhere).
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let (major, minor) = s
            .split_once('.')
            .ok_or_else(|| anyhow::anyhow!("protocolVersion must be MAJOR.MINOR, got {s:?}"))?;
        let major: u32 = major
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid major component in {s:?}"))?;
        let minor: u32 = minor
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid minor component in {s:?}"))?;
        Ok(Self { major, minor })
    }

    /// String form `"MAJOR.MINOR"` — the wire representation.
    pub fn as_str(&self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// Applies the version-acceptance rule (contracts/00 §Handshake):
/// accept iff equal major && client minor ≤ server minor.
///
/// Returns `Ok(())` when the client version is acceptable, else the exact
/// `-32001 version-mismatch` error carrying `serverVersion`,
/// `serverProtocolVersion`, `reason`, `recovery`.
pub fn negotiate_version(client_version: &str, server_version: &str) -> Result<(), RpcError> {
    let client = match ProtocolVersion::parse(client_version) {
        Ok(v) => v,
        Err(e) => {
            return Err(RpcError::version_mismatch(
                server_version,
                SERVER_PROTOCOL_VERSION,
                &e.to_string(),
            ));
        }
    };
    let server = match ProtocolVersion::parse(SERVER_PROTOCOL_VERSION) {
        Ok(v) => v,
        Err(e) => return Err(RpcError::internal(&e.to_string())),
    };
    if client.major != server.major {
        return Err(RpcError::version_mismatch(
            server_version,
            SERVER_PROTOCOL_VERSION,
            &format!("major mismatch: client {} vs server {}", client, server),
        ));
    }
    if client.minor > server.minor {
        return Err(RpcError::version_mismatch(
            server_version,
            SERVER_PROTOCOL_VERSION,
            &format!(
                "client minor {} exceeds server minor {}",
                client.minor, server.minor
            ),
        ));
    }
    Ok(())
}

/// Client capability flags (data-model E8). All default `false`; every
/// field is an assertion read as a boolean (contracts/01).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Client can answer Q3 `approval/request` (else turns fail-fast -32010).
    #[serde(default)]
    pub approvals: bool,
    /// Client consumes `session/event` streaming notifications.
    #[serde(default)]
    pub streaming: bool,
    /// Client acknowledges notification delivery (future use).
    #[serde(default)]
    pub delivery_ack: bool,
}

/// `{name, version}` info object for both client and server (contracts/01).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    /// Surface or daemon name, e.g. `"zen-tui"`, `"zen-gateway"`.
    pub name: String,
    /// Semver of the peer.
    pub version: String,
}

/// Typed `initialize` params (Q1). Field names are wire-exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Client protocol version, `"MAJOR.MINOR"`.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// Client identity.
    #[serde(rename = "clientInfo")]
    pub client_info: Info,
    /// Client capabilities; absent flags default false.
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// Typed `initialize` result (Q2). Echoes the negotiated protocol version,
/// server identity, and the effective capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// Server protocol version (always the server's own, not the client's).
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// Server identity: `{name: "zen-gateway", version: crate version}`.
    #[serde(rename = "serverInfo")]
    pub server_info: Info,
    /// Effective capabilities for the connection.
    pub capabilities: Capabilities,
}

impl InitializeResult {
    /// Builds the canonical result for an accepted handshake: protocol
    /// version from this crate, server name/version from build constants.
    pub fn for_server() -> Self {
        Self {
            protocol_version: SERVER_PROTOCOL_VERSION.to_string(),
            server_info: Info {
                name: SERVER_NAME.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            capabilities: Capabilities::default(),
        }
    }

    /// Overrides the effective capabilities (the daemon stores what the
    /// client declared; `approvals` gates Q3 routing).
    pub fn with_capabilities(mut self, caps: Capabilities) -> Self {
        self.capabilities = caps;
        self
    }
}

/// Connection handshake state (data-model E2 subset): `Connecting` until
/// `initialize` succeeds, `Initialized` once the `initialized` notification
/// arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandshakeState {
    /// Pre-initialize: any non-initialize request → -32000.
    #[default]
    Connecting,
    /// Handshake complete: normal dispatch.
    Initialized,
}

/// Validates an `initialize` frame's params and produces the result.
///
/// # Errors
/// - [`RpcError::version_mismatch`] (-32001) when the acceptance rule
///   fails — carries `serverVersion` + `serverProtocolVersion` + recovery.
/// - [`RpcError::invalid_params`] (-32602) when params fail to deserialize
///   as [`InitializeParams`].
pub fn handle_initialize(params: &Value) -> Result<InitializeResult, RpcError> {
    let parsed: InitializeParams = serde_json::from_value(params.clone())
        .map_err(|e| RpcError::invalid_params("initialize", &e.to_string()))?;
    negotiate_version(&parsed.protocol_version, env!("CARGO_PKG_VERSION"))?;
    Ok(InitializeResult::for_server().with_capabilities(parsed.capabilities))
}

/// Builds the wire params for the client side of `initialize`
/// (L3 helper — surfaces never hand-assemble JSON).
pub fn initialize_params(
    protocol_version: &str,
    client_name: &str,
    client_version: &str,
    capabilities: Capabilities,
) -> Value {
    json!({
        "protocolVersion": protocol_version,
        "clientInfo": { "name": client_name, "version": client_version },
        "capabilities": capabilities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse_round_trip() {
        let v = ProtocolVersion::parse("1.0").unwrap();
        assert_eq!((v.major, v.minor), (1, 0));
        assert_eq!(v.as_str(), "1.0");
        assert!(ProtocolVersion::parse("1").is_err());
        assert!(ProtocolVersion::parse("1.x").is_err());
        assert!(ProtocolVersion::parse("v1.0").is_err());
    }

    #[test]
    fn version_matrix() {
        for (client, ok) in [
            ("1.0", true),
            ("1.5", false),
            ("2.0", false),
            ("0.9", false),
        ] {
            assert_eq!(negotiate_version(client, "0.0.8").is_ok(), ok, "{client}");
        }
    }

    #[test]
    fn mismatch_data_carries_recovery() {
        let err = negotiate_version("2.0", "0.0.8").unwrap_err();
        assert_eq!(err.code, -32001);
        let d = err.data.unwrap();
        assert_eq!(d["serverVersion"], "0.0.8");
        assert_eq!(d["serverProtocolVersion"], "1.0");
        assert_eq!(d["recovery"], "restart gateway with matching version");
        assert!(d["reason"].as_str().unwrap().contains("major"));
    }

    #[test]
    fn initialize_result_shape() {
        let r = InitializeResult::for_server();
        assert_eq!(r.protocol_version, "1.0");
        assert_eq!(r.server_info.name, "zen-gateway");
        assert_eq!(r.server_info.version, env!("CARGO_PKG_VERSION"));
        assert!(!r.capabilities.approvals);
    }

    #[test]
    fn capabilities_default_false_on_wire() {
        let v = serde_json::from_str::<Capabilities>("{}").unwrap();
        assert_eq!(v, Capabilities::default());
        let v = serde_json::from_str::<Capabilities>(r#"{"approvals":true}"#).unwrap();
        assert!(v.approvals && !v.streaming && !v.delivery_ack);
    }

    #[test]
    fn handle_initialize_accepts_and_rejects() {
        let ok = handle_initialize(&initialize_params(
            "1.0",
            "zen-tui",
            "0.0.8",
            Capabilities {
                approvals: true,
                ..Default::default()
            },
        ))
        .unwrap();
        assert!(ok.capabilities.approvals);

        let err = handle_initialize(&initialize_params("9.9", "x", "1", Capabilities::default()))
            .unwrap_err();
        assert_eq!(err.code, -32001);
    }
}
