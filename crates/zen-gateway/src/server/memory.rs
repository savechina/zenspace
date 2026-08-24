//! Memory handlers (task T013, contracts/02 §Memory) — sole-owner
//! surface over [`ZenMemvidStore`].
//!
//! PURPOSE: Implements memory/retrieve, memory/putEntry, memory/search,
//! memory/stats exactly per contract schemas. Every handler resolves the
//! shared store handle first: when absent the store is unavailable and
//! ops fail fast with -32002 `store-unavailable` (FR-001 sole-owner).
//!
//! USAGE: The daemon wraps its `SharedStore` in closures and installs
//! these via `DispatchServer::handle`; handlers are transport-blind.
//!
//! EXPECTED: retrieve filters cards below TRIPLET_MIN_CONFIDENCE; putEntry
//! returns the persisted frame id; search maps memvid hits to
//! `{frameId, snippet, score}`.
//!
//! ERRORS: -32002 when the store handle is absent; -32602 on malformed
//! params; -32603 wrapping store failures.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::RwLock;
use zen_memory::{EntityType, MemoryEntry, TRIPLET_MIN_CONFIDENCE, ZenMemvidStore};

use crate::protocol::RpcError;

/// Store handle shared by daemon and handlers; `None` = unavailable.
pub type SharedStore = Arc<RwLock<Option<ZenMemvidStore>>>;

/// Resolves the live store or fails with -32002.
async fn require_store(
    store: &SharedStore,
) -> Result<tokio::sync::RwLockReadGuard<'_, Option<ZenMemvidStore>>, RpcError> {
    let guard = store.read().await;
    if guard.is_none() {
        return Err(RpcError::store_unavailable("unavailable"));
    }
    Ok(guard)
}

fn required_str(method: &str, params: &Value, key: &str) -> Result<String, RpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(method, &format!("missing {key:?}")))
}

/// memory/retrieve — `{sessionId, limit?=20}` → confidence-filtered cards.
pub async fn retrieve(store: SharedStore, params: Value) -> Result<Value, RpcError> {
    const METHOD: &str = "memory/retrieve";
    let session_id = required_str(METHOD, &params, "sessionId")?;
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;

    let guard = require_store(&store).await?;
    let zstore = guard.as_ref().expect("checked non-None");
    let cards = zstore
        .store()
        .entity_memories(&session_id)
        .map_err(|e| RpcError::internal(&format!("memory read failed: {e}")))?;
    let entries: Vec<Value> = cards
        .into_iter()
        .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
        .take(limit)
        .map(|c| {
            json!({
                "kind": c.kind.to_string(),
                "entity": c.entity,
                "slot": c.slot,
                "value": c.value,
                "confidence": c.confidence,
            })
        })
        .collect();
    Ok(json!({ "entries": entries }))
}

/// memory/putEntry — persist a typed entry; returns `{frameId}`.
pub async fn put_entry(store: SharedStore, params: Value) -> Result<Value, RpcError> {
    const METHOD: &str = "memory/putEntry";
    let session_id = required_str(METHOD, &params, "sessionId")?;
    let role = required_str(METHOD, &params, "role")?;
    let content = required_str(METHOD, &params, "content")?;
    let entity_type = match params.get("entityType").and_then(Value::as_str) {
        Some("session") => EntityType::Session,
        Some("user") => EntityType::User,
        Some("knowledge") => EntityType::Knowledge,
        _ => {
            return Err(RpcError::invalid_params(
                METHOD,
                "entityType must be \"session\"|\"user\"|\"knowledge\"",
            ));
        }
    };
    let metadata: HashMap<String, Value> = params
        .get("metadata")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    let entry = MemoryEntry {
        session_id,
        role,
        content,
        entity_type,
        metadata,
    };

    let guard = require_store(&store).await?;
    let zstore = guard.as_ref().expect("checked non-None");
    let frame_id = zstore
        .put_entry(&entry)
        .map_err(|e| RpcError::internal(&format!("memory write failed: {e}")))?;
    Ok(json!({ "frameId": frame_id.to_string() }))
}

/// memory/search — full-text query restricted optionally to one session.
pub async fn search(store: SharedStore, params: Value) -> Result<Value, RpcError> {
    const METHOD: &str = "memory/search";
    let query = required_str(METHOD, &params, "query")?;
    let top_k = params.get("topK").and_then(Value::as_u64).unwrap_or(10);
    let uri = params.get("sessionId").and_then(Value::as_str);

    // SearchRequest has serde defaults for every non-essential field;
    // building via JSON keeps us forward-compatible with memvid-core.
    let request: rig_memvid::memvid_core::SearchRequest = serde_json::from_value(json!({
        "query": query,
        "top_k": top_k as usize,
        "snippet_chars": 160,
        "uri": uri,
    }))
    .map_err(|e| RpcError::invalid_params(METHOD, &format!("bad search request: {e}")))?;

    let guard = require_store(&store).await?;
    let zstore = guard.as_ref().expect("checked non-None");
    let response = zstore
        .store()
        .search(request)
        .map_err(|e| RpcError::internal(&format!("memory search failed: {e}")))?;
    let hits: Vec<Value> = response
        .hits
        .into_iter()
        .map(|h| {
            json!({
                "frameId": h.frame_id.to_string(),
                "snippet": h.text,
                "score": h.score,
            })
        })
        .collect();
    Ok(json!({ "hits": hits }))
}

/// memory/stats — archive counters for observability surfaces.
pub async fn stats(store: SharedStore, _params: Value) -> Result<Value, RpcError> {
    let guard = require_store(&store).await?;
    let zstore = guard.as_ref().expect("checked non-None");
    let s = zstore
        .store()
        .stats()
        .map_err(|e| RpcError::internal(&format!("memory stats failed: {e}")))?;
    Ok(json!({
        "frames": s.frame_count,
        "capacityBytes": s.capacity_bytes,
        "generation": s.seq_no.unwrap_or(0),
    }))
}
