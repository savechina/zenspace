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
    for dir in &state.scan_dirs {
        if !dir.is_dir() {
            continue;
        }
        let results = match service
            .search(&query, dir, client, tier, None, Some(limit as usize))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "knowledge/search tier failed");
                continue;
            }
        };
        for r in results {
            if seen.insert(normalize_path(&r.file)) {
                notes.push(json!({
                    "path": r.file.display().to_string(),
                    "content": r.content,
                    "sensitivity": "public",
                    "relevance": 1.0,
                }));
            }
            if notes.len() >= limit as usize {
                return Ok(json!({ "notes": notes }));
            }
        }
    }
    Ok(json!({ "notes": notes }))
}

/// Canonical dedupe key: canonicalized when possible, raw otherwise.
fn normalize_path(p: &Path) -> String {
    p.canonicalize()
        .map(|c| c.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}
