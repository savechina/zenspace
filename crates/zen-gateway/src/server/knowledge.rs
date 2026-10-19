//! Knowledge handler (task T014, contracts/02 §Knowledge) — server-side
//! 5-tier search that retires client-side duplication (research.md A/B).
//!
//! PURPOSE: Implements `knowledge/search` `{query, tiers?, limit?=10}` →
//! `{notes:[{path, content, sensitivity, relevance}]}` using
//! [`SearchService`] + [`SqliteClient`] owned by the daemon, scanning the
//! inbox and wiki roots.
//!
//! USAGE: The daemon builds [`KnowledgeState`] once at startup (router +
//! db) and installs [`search`] via `DispatchServer::handle`.
//!
//! EXPECTED: results dedupe by path across scanned dirs; when no tier is
//! named the service auto-selects via `TierSelector::select_tier`.
//!
//! ERRORS: -32602 malformed params / unknown tier name; -32603 when the
//! search stack failed to initialize (config or db unavailable).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use zen_repo::SqliteClient;
use zen_vault::search::SearchService;

use crate::protocol::RpcError;

/// Immutable search stack built at daemon startup; `None` fields mean the
/// corresponding subsystem failed to initialize (degraded mode).
pub struct KnowledgeState {
    service: Option<SearchService>,
    client: Option<SqliteClient>,
    scan_dirs: Vec<PathBuf>,
}

impl KnowledgeState {
    /// Assembles the search stack from an initialized router/client pair;
    /// either may be `None` (daemon logs and runs degraded).
    pub fn new(
        service: Option<SearchService>,
        client: Option<SqliteClient>,
        scan_dirs: Vec<PathBuf>,
    ) -> Self {
        Self {
            service,
            client,
            scan_dirs,
        }
    }

    /// Spec D16 skew probe over the owned pool: `Some((db_max, embedded_max))`
    /// when the on-disk schema is ahead of this binary's embedded set (the
    /// old-binary-vs-newer-DB direction), `None` otherwise — fresh/equal/behind
    /// databases and a failed db client both read as "no signal".
    ///
    /// Read-only: `detect_schema_skew` runs a bare `SELECT MAX(version)`, never
    /// migrates and never takes the write lock, so the daemon startup path can
    /// call it without perturbing the sole-owner store discipline.
    pub async fn detect_schema_skew(&self) -> Option<(i64, i64)> {
        let client = self.client.as_ref()?;
        zen_repo::detect_schema_skew(client.pool()).await
    }
}

/// Maps contract tier names onto SearchService numeric tiers. The first
/// recognized entry wins; unknown names are rejected so clients never get
/// silently-wrong routing.
fn resolve_tier(tiers: Option<&Vec<Value>>) -> Result<Option<u8>, RpcError> {
    const METHOD: &str = "knowledge/search";
    let Some(list) = tiers else { return Ok(None) };
    let first = list
        .iter()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params(METHOD, "tiers must be string names"))?;
    match first {
        "ripgrep" => Ok(Some(1)),
        "fts" => Ok(Some(2)),
        "vec" => Ok(Some(3)),
        "graph" => Ok(Some(4)),
        "llm" => Ok(Some(5)),
        other => Err(RpcError::invalid_params(
            METHOD,
            &format!("unknown tier {other:?}; expected ripgrep|fts|vec|graph|llm"),
        )),
    }
}

/// knowledge/search handler per contracts/02 §Knowledge.
///
/// G4-01: a tier failure is no longer swallowed into a bare empty result
/// set — it is collected into the additive `degraded` array (omitted when
/// nothing failed, so the happy path stays byte-identical).
pub async fn search(state: Arc<KnowledgeState>, params: Value) -> Result<Value, RpcError> {
    const METHOD: &str = "knowledge/search";
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(METHOD, "missing \"query\""))?;
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10);
    let tier = resolve_tier(params.get("tiers").and_then(Value::as_array))?;

    let (Some(service), Some(client)) = (state.service.as_ref(), state.client.as_ref()) else {
        return Err(RpcError::internal(
            "knowledge search unavailable (config/db init failed)",
        ));
    };

    let mut seen = std::collections::HashSet::new();
    let mut notes = Vec::new();
    let mut degraded = Vec::new();
    let mut query_embedding_degraded = false;
    let mut query_embedding_unavailable = false;
    // Each dir runs a full independent pipeline (fused tiers per dir), so the
    // scans are run CONCURRENTLY: wall time is max(dirs) instead of the sum,
    // and the duplicated embedding/ripgrep work across dirs overlaps instead
    // of stacking. Results still merge in scan_dirs order with the same
    // dedup, cap, and degraded semantics as the former sequential loop.
    let scans: Vec<_> = state
        .scan_dirs
        .iter()
        .filter(|d| d.is_dir())
        .map(|dir| {
            let dir = dir.clone();
            async {
                let r = service
                    .search_with_diagnostics(&query, &dir, client, tier, None, Some(limit as usize))
                    .await;
                (dir, r)
            }
        })
        .collect();
    for (dir, outcome) in futures_util::future::join_all(scans).await {
        let (results, diagnostics) = match outcome {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "knowledge/search tier failed");
                degraded.push(degraded_entry(tier, &e.to_string()));
                continue;
            }
        };
        query_embedding_degraded |= diagnostics.query_embedding_degraded;
        query_embedding_unavailable |= diagnostics.query_embedding_unavailable;
        for r in results {
            // Quota check BEFORE the push (the sequential version exited the
            // whole scan via `break 'scan`; per-dir loops must not let a later
            // dir's first hit exceed the cap while breaking).
            if notes.len() >= limit as usize {
                break;
            }
            if seen.insert(normalize_path(&r.file)) {
                notes.push(json!({
                    "path": r.file.display().to_string(),
                    "content": r.content,
                    "sensitivity": "public",
                    "relevance": 1.0,
                }));
            }
        }
    }
    if let Some(entry) = query_embedding_degraded_entry(
        query_embedding_degraded,
        query_embedding_unavailable,
        &degraded,
    ) {
        degraded.push(entry);
    }
    Ok(build_search_response(notes, degraded))
}

/// O16: turns a silent query-side embedding degradation into a `degraded`
/// entry (the vec0 signal is fused but is not semantic, or was skipped).
///
/// Returns `None` on the healthy path and when the vec tier already reported
/// an entry, so a successful search stays byte-identical. Public as the
/// deterministic, I/O-free core of the decision so it can be pinned.
pub fn query_embedding_degraded_entry(
    degraded: bool,
    unavailable: bool,
    existing: &[Value],
) -> Option<Value> {
    let vec_already_reported = existing
        .iter()
        .any(|d| d.get("tier").and_then(Value::as_str) == Some("vec"));
    if vec_already_reported {
        return None;
    }
    if unavailable {
        return Some(degraded_entry(
            Some(3),
            "query embedding unavailable; vec0 list skipped",
        ));
    }
    degraded.then(|| {
        degraded_entry(
            Some(3),
            "query embedding degraded to local hash fallback (no embedding provider \
             reachable); vec0 list is not semantic",
        )
    })
}

/// Contract name for a resolved numeric tier (`auto` when the caller let the
/// service auto-select). Mirrors [`resolve_tier`]'s mapping.
fn tier_label(tier: Option<u8>) -> &'static str {
    match tier {
        Some(1) => "ripgrep",
        Some(2) => "fts",
        Some(3) => "vec",
        Some(4) => "graph",
        Some(5) => "llm",
        _ => "auto",
    }
}

/// One `{"tier": name, "error": msg}` record for the additive `degraded`
/// array (G4-01).
fn degraded_entry(tier: Option<u8>, error: &str) -> Value {
    json!({ "tier": tier_label(tier), "error": error })
}

/// Assembles the knowledge/search response. `degraded` is OMITTED entirely
/// when empty, so a fully successful search is byte-identical to the
/// pre-G4-01 `{"notes":[...]}` shape. Public as the deterministic, I/O-free
/// core of [`search`] so the present-only-on-failure contract can be pinned.
pub fn build_search_response(notes: Vec<Value>, degraded: Vec<Value>) -> Value {
    let mut response = json!({ "notes": notes });
    if !degraded.is_empty() {
        response["degraded"] = Value::Array(degraded);
    }
    response
}

/// Canonical dedupe key: canonicalized when possible, raw otherwise.
fn normalize_path(p: &Path) -> String {
    p.canonicalize()
        .map(|c| c.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}
