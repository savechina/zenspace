//! Zen gateway — sole-owner agent daemon + multi-carrier protocol host.
//!
//! # PURPOSE
//! Hosts the knowledge store (single RW opener), the hosted agent
//! session stack, and the JSON-RPC 2.0 method registry behind three
//! carriers: UDS socket (primary, `zen serve start` — detached by default),
//! in-process pair (contract tests), and loopback HTTP (`/api/v1`).
//! Protocol behavior is frozen by the contracts in
//! `docs/specs/004-agentic-gateway/contracts/`:
//!
//! | Contract | Scope |
//! |---|---|
//! | `00-protocol-overview.md` | envelope, quadrants (Q1–Q4) |
//! | `01-naming-conventions.md` | method naming + versioning rules |
//! | `02-method-catalog.md` | normative request/response/error schemas |
//! | `03-cli-interface-mapping.md` | `zen chat`/command dispositions |
//! | `04-tui-interface-mapping.md` | TUI slash commands + banner strings |
//! | `05-carrier-qqbot.md` | deferred IM carrier staging |
//!
//! # MODULE MAP
//! - [`protocol`] — envelope frames, method registry, error catalog
//! - [`transport`] — carrier traits + UDS/in-process implementations
//! - [`channel`] — outbound platform carriers (qqbot: WS gateway +
//!   loopback-HTTP bridge implementing the generic `Channel` trait)
//! - [`client`] — client runtime ([`GatewayClient`](client::GatewayClient),
//!   [`SurfaceClient`](client::SurfaceClient) facade)
//! - [`server`] — dispatch, E2 connections, hosted turns (US4),
//!   guards, approval broker, memory/knowledge handlers
//! - [`daemon`] — E1 `GatewayService` sole-owner lifecycle + drain
//!
//! # USAGE
//! Daemon side: `GatewayService::serve(config)` (blocks, drains on
//! shutdown). Client side: `SurfaceClient::open_default(...)` for chat/
//! TUI surfaces; raw [`GatewayClient`](client::GatewayClient) for tools.
//!
//! # EXPECTED
//! Exactly one live daemon per user/machine; second bind attempts fail
//! fast (sole-owner claim). All protocol ops emit tracing spans keyed by
//! `rpc_id`/`turn_id`/`session_id` (FR-014).
//!
//! # ERRORS
//! Closed catalog `-32000..-32099` per contract 01 — codes never change
//! meaning; additions require a MINOR version bump.

mod daemon;
// Pre-004 batching experiment (analyze F7): no production callers since the
// legacy HTTP stack retired; retained with its unit tests for future use.
#[allow(dead_code)]
mod inference_gateway;
pub mod subconscious;

pub mod channel;
pub mod client;
pub mod mcp_server;
pub mod protocol;
pub mod server;
pub mod transport;

pub use daemon::{
    GatewayDaemonConfig, GatewayService, HttpConfig, is_pid_alive, pid_record_alive, read_pid,
    read_pid_record, remove_pid, write_pid, write_pid_for,
};
pub use mcp_server::McpServer;

/// Builds the Confidential-filtered MCP tool registry served on every
/// carrier (FR-020: stdio and HTTP must expose identical tools).
///
/// # Errors
/// Never fails — registry construction degrades internally; a build
/// failure yields an empty (but valid) registry.
pub fn mcp_registry_for_http() -> rig_compose::registry::ToolRegistry {
    zen_agents::wiring::ZenWiring::new().build_mcp_registry()
}
