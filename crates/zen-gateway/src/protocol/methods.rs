//! Method registry — the table-driven single source of truth for every
//! method, notification, and server-request in the gateway protocol
//! (contracts/02, dsh `RpcMethodMap` pattern).
//!
//! PURPOSE: One const table row per wire method declaring domain,
//! direction, since-minor, and params/result type names. Dispatch, the
//! `-32601 supportedMethods` payload, and the contract suite all derive
//! from this table — adding a method is exactly one row.
//!
//! USAGE: Query via [`MethodRegistry::lookup`] / [`MethodRegistry::supported_methods`].
//! Quadrant classification is static by method (never inferred from channel).
//!
//! EXPECTED: 21 rows (20 at v1.0 freeze + additive `memory/rebuild` since
//! 1.1 — Phase 11 T053/T057 MINOR bump decision recorded here per
//! contract 00 §version rules), exactly the entries enumerated by contracts/02
//! (16 C→S request methods + 4 notifications, with `agent/status`
//! present as BOTH a request and a notification). The contracts/02
//! count-note's "21" is an off-by-one in its arithmetic; the enumerated
//! table is normative.
//!
//! ERRORS: Unknown method → dispatch answers -32601 with
//! `data.supportedMethods` from this table.
//!
//! # MINOR-bump procedure (v1.0 freeze)
//!
//! The registry is frozen at protocol 1.0. A MINOR bump (e.g. 1.1) MUST
//! land in a single PR containing all three of:
//!
//! 1. One new row in [`METHODS`] below (bump its `since` to the new minor).
//! 2. One new row in `contracts/02-method-catalog.md` §Error/method table
//!    (the frozen contract file — edit allowed only for additive rows).
//! 3. One new case in `tests/contract_suite.rs` covering the row's happy
//!    path plus every documented error code.
//!
//! CI gates: `cargo test -p zen-gateway --test contract_suite` plus the
//! suite's registry-coverage test (every registry row exercised) make a
//! partial bump fail the build. Removals/renames/required-ifies are MAJOR
//! and refuse old clients at initialize (`-32001`).
//!
//! # Deferred carriers (out of 004 scope)
//!
//! The qqbot IM carrier (`contracts/05-carrier-qqbot.md`) and a plain
//! WebSocket carrier are explicitly DEFERRED: they reuse this same
//! method registry and error catalog unchanged, so their future arrival
//! is purely additive transport work and requires NO MINOR bump.

/// Interaction quadrant a method lives in — static by method name
/// (contracts/00 rule; direction is never inferred from the channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quadrant {
    /// Q1 — client request, answered by a Q2 response.
    ClientRequest,
    /// Q3 — server request (answerable), answered by a Q4 response.
    ServerRequest,
    /// Notification — either direction, never answered.
    Notification,
}

/// Which protocol phase introduced the method; rows exist from phase 0 but
/// daemons may reject unimplemented rows with -32601 until their phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Phase 1 — daemon baseline.
    Phase1,
    /// Phase 3 — harness hosting.
    Phase3,
}

/// One registry row: the complete static declaration for a wire method.
#[derive(Debug, Clone, Copy)]
pub struct MethodMeta {
    /// Wire method name (`domain/verb`, lowerCamelCase verb).
    pub name: &'static str,
    /// Fixed domain from the closed domain list (contracts/01).
    pub domain: &'static str,
    /// Direction/quadrant, static by method.
    pub quadrant: Quadrant,
    /// Protocol minor that introduced the method ("1.0" or "1.1").
    pub since: &'static str,
    /// Delivery phase per contracts/02 Phase column.
    pub phase: Phase,
    /// Params type sketch as named in contracts/02 (display only).
    pub params: &'static str,
    /// Result type sketch as named in contracts/02 (display only).
    pub result: &'static str,
}

/// The complete method table. Row order mirrors contracts/02 section order.
/// Single source of truth — do not hand-maintain derived lists.
pub const METHODS: &[MethodMeta] = &[
    // Lifecycle
    MethodMeta {
        name: "initialize",
        domain: "initialize",
        quadrant: Quadrant::ClientRequest,
        since: "1.0",
        phase: Phase::Phase1,
        params: "{protocolVersion, clientInfo, capabilities}",
        result: "{protocolVersion, serverInfo, capabilities}",
    },
    MethodMeta {
        name: "initialized",
        domain: "initialized",
        quadrant: Quadrant::Notification,
        since: "1.0",
        phase: Phase::Phase1,
        params: "{}",
        result: "—",
    },
    MethodMeta {
        name: "health/status",
        domain: "health",
        quadrant: Quadrant::ClientRequest,
        since: "1.0",
        phase: Phase::Phase1,
        params: "—",
        result: "{serverVersion, protocolVersion, clients, uptimeMs, storeHealth, activeTurns}",
    },
    MethodMeta {
        name: "shutdown",
        domain: "shutdown",
        quadrant: Quadrant::ClientRequest,
        since: "1.0",
        phase: Phase::Phase1,
        params: "—",
        result: "{drained, cancelled}",
    },
    // Memory
    MethodMeta {
        name: "memory/retrieve",
        domain: "memory",
        quadrant: Quadrant::ClientRequest,
        since: "1.0",
        phase: Phase::Phase1,
        params: "{sessionId, limit?}",
        result: "{entries[]}",
    },
    MethodMeta {
        name: "memory/putEntry",
        domain: "memory",
        quadrant: Quadrant::ClientRequest,
        since: "1.0",
        phase: Phase::Phase1,
        params: "{sessionId, role, content, entityType, metadata?}",
        result: "{frameId}",
    },
    MethodMeta {
        name: "memory/search",
        domain: "memory",
        quadrant: Quadrant::ClientRequest,
        since: "1.0",
        phase: Phase::Phase1,
        params: "{query, topK?, sessionId?}",
        result: "{hits[]}",
    },
    MethodMeta {
        name: "memory/stats",
        domain: "memory",
        quadrant: Quadrant::ClientRequest,
        since: "1.0",
        phase: Phase::Phase1,
        params: "—",
        result: "{frames, capacityBytes, generation}",
    },
    MethodMeta {
        name: "memory/rebuild",
        domain: "memory",
        quadrant: Quadrant::ClientRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "—",
        result: "{filesScanned, chunksIndexed, errors[], replay}",
    },
    // Knowledge
    MethodMeta {
        name: "knowledge/search",
        domain: "knowledge",
        quadrant: Quadrant::ClientRequest,
        since: "1.0",
        phase: Phase::Phase1,
        params: "{query, tiers?, limit?}",
        result: "{notes[]}",
    },
    // Session (P3)
    MethodMeta {
        name: "session/start",
        domain: "session",
        quadrant: Quadrant::ClientRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "{sessionId?, agent?}",
        result: "{sessionId, agent}",
    },
    MethodMeta {
        name: "session/turn",
        domain: "session",
        quadrant: Quadrant::ClientRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "{turnId, sessionId, prompt, knowledge?}",
        result: "{turnId, response}",
    },
    MethodMeta {
        name: "session/cancel",
        domain: "session",
        quadrant: Quadrant::ClientRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "{turnId}",
        result: "{outcome}",
    },
    MethodMeta {
        name: "session/resume",
        domain: "session",
        quadrant: Quadrant::ClientRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "{turnId, lastSeq}",
        result: "replay | {snapshot, response, events}",
    },
    // Agent / Skill reads (P3)
    MethodMeta {
        name: "agent/list",
        domain: "agent",
        quadrant: Quadrant::ClientRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "—",
        result: "{agents[]}",
    },
    MethodMeta {
        name: "agent/status",
        domain: "agent",
        quadrant: Quadrant::ClientRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "{agent}",
        result: "{agent, busy, budgetAvailable, budgetConsumed}",
    },
    MethodMeta {
        name: "skill/list",
        domain: "skill",
        quadrant: Quadrant::ClientRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "—",
        result: "{skills[]}",
    },
    // Notifications
    MethodMeta {
        name: "session/event",
        domain: "session",
        quadrant: Quadrant::Notification,
        since: "1.1",
        phase: Phase::Phase3,
        params: "{turnId, seq, kind, payload?}",
        result: "—",
    },
    MethodMeta {
        name: "agent/status",
        domain: "agent",
        quadrant: Quadrant::Notification,
        since: "1.1",
        phase: Phase::Phase3,
        params: "{agent, state, budgetConsumed}",
        result: "—",
    },
    MethodMeta {
        name: "health/ping",
        domain: "health",
        quadrant: Quadrant::Notification,
        since: "1.0",
        phase: Phase::Phase1,
        params: "—",
        result: "—",
    },
    // Server→client request
    MethodMeta {
        name: "approval/request",
        domain: "approval",
        quadrant: Quadrant::ServerRequest,
        since: "1.1",
        phase: Phase::Phase3,
        params: "{turnId, invocation, reason}",
        result: "{decision, remember?}",
    },
];

/// Lookup interface over the const [`METHODS`] table.
pub struct MethodRegistry;

impl MethodRegistry {
    /// Returns the registry row for `method`, or `None` when unknown
    /// (dispatch then answers -32601 with [`Self::supported_methods`]).
    pub fn lookup(method: &str) -> Option<&'static MethodMeta> {
        METHODS.iter().find(|m| m.name == method)
    }

    /// Every method name in the table — the payload for
    /// `-32601 data.supportedMethods` and the contract-suite coverage check.
    pub fn supported_methods() -> Vec<&'static str> {
        METHODS.iter().map(|m| m.name).collect()
    }

    /// Only Q1 client-request names (surfaces may call these).
    pub fn client_request_methods() -> Vec<&'static str> {
        METHODS
            .iter()
            .filter(|m| m.quadrant == Quadrant::ClientRequest)
            .map(|m| m.name)
            .collect()
    }

    /// Only Q3 server-request names (clients must implement handlers).
    pub fn server_request_methods() -> Vec<&'static str> {
        METHODS
            .iter()
            .filter(|m| m.quadrant == Quadrant::ServerRequest)
            .map(|m| m.name)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_contracts_count() {
        // 21 rows since 1.1 (additive MINOR bump): memory/rebuild (Phase 11 T053/T057).
        assert_eq!(METHODS.len(), 21);
        assert_eq!(MethodRegistry::supported_methods().len(), 21);
    }

    #[test]
    fn lookup_finds_and_misses() {
        assert_eq!(
            MethodRegistry::lookup("memory/putEntry").unwrap().domain,
            "memory"
        );
        assert!(MethodRegistry::lookup("memory/entryCreate").is_none());
    }

    #[test]
    fn exactly_one_server_request() {
        assert_eq!(
            MethodRegistry::server_request_methods(),
            vec!["approval/request"]
        );
    }
}
