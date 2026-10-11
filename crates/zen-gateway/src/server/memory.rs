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
//! `{frameId, snippet, score}` — plus an additive `strength` field and a
//! `score × R` DESC reordering when `[agentic.memory_strength]
//! enabled && search_rerank` (T209; gate-off is byte-identical, ranking-only,
//! hits are never dropped).
//!
//! ERRORS: -32002 when the store handle is absent; -32602 on malformed
//! params; -32603 wrapping store failures.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use zen_memory::memvid_index::{
    PROVENANCE_SOURCE_PATH_KEY, PROVENANCE_TIER_KEY, extract_anchor_date,
};
use zen_memory::memvid_store::memvid_core::SearchHit;
use zen_memory::strength::{resolve_content_anchor, retention_strength};
use zen_memory::{EntityType, MemoryEntry, TRIPLET_MIN_CONFIDENCE, ZenMemvidStore};
use zen_vault::distill::{card_id_from_path, read_reward};

use crate::protocol::RpcError;

/// Store handle shared by daemon and handlers; `None` = unavailable.
pub type SharedStore = Arc<RwLock<Option<ZenMemvidStore>>>;

/// Resolved strength-rerank inputs for `memory/search` (Phase 31 T209,
/// owner decision D-G: the ONE `[agentic.memory_strength] search_rerank`
/// flag drives both this handler and the `select_cards` consumer).
///
/// `None` from [`StrengthContext::resolve`] means the gate is closed or an
/// input failed to load — the handler then returns the pre-T209 response
/// byte-identically (fail-open to unchanged behavior, never to a surprise
/// reordering).
pub(crate) struct StrengthContext {
    /// M2 provenance root (`source_path` is relative to it).
    memory_root: PathBuf,
    /// M3/M4 provenance root.
    wiki_root: PathBuf,
    /// Reward sidecar directory (`memories/.reward`).
    reward_dir: PathBuf,
    half_life_days: f64,
}

impl StrengthContext {
    /// Build the context when `enabled && search_rerank`; `None` otherwise
    /// or on any config/path failure (warn-logged, fail-open to gate-off).
    fn resolve() -> Option<Self> {
        let config = match zen_core::config::load_config() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "memory/search: config load failed — strength rerank stays off"
                );
                return None;
            }
        };
        let ms = &config.agentic.memory_strength;
        if !(ms.enabled_or_default() && ms.search_rerank_or_default()) {
            return None;
        }
        let paths = match zen_core::paths::ZenPaths::detect() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "memory/search: path detection failed — strength rerank stays off"
                );
                return None;
            }
        };
        Some(Self {
            memory_root: paths.memory(),
            wiki_root: paths.wiki(),
            reward_dir: paths.memory().join(".reward"),
            half_life_days: ms.half_life_days_or_default(),
        })
    }

    /// Absolute path of a provenance `source_path` given its tier tag.
    fn resolve_source(&self, source_path: &str, tier: Option<&str>) -> Option<PathBuf> {
        match tier {
            Some("m2") => Some(self.memory_root.join(source_path)),
            Some("m3") | Some("m4") => Some(self.wiki_root.join(source_path)),
            _ => None,
        }
    }
}

/// Retention strength `R ∈ (0,1]` of one search hit (design F1 anchor chain:
/// journal-uri date > source-file mtime > frame created_at).
///
/// Provenance is keyed on `source_path`/`tier` PRESENCE, never on map
/// emptiness — memvid-core injects its own `extractous_metadata` key into
/// `extra_metadata` (T208 empirical consumer contract). A missing/unparsable
/// `created_at` falls back to `now` ⇒ `R = 1.0`: the neutral multiplicative
/// rank factor, so an unknown-age hit is never sunk below its peers.
///
/// Sidecar join honesty: `read_reward` is keyed by
/// `card_id_from_path(source_path)` — the SAME derivation T211 will write
/// memvid-sourced increments with. Today's production writer
/// (`increment_reward_for_query`) keys sidecars by knowledge-note paths from
/// vault search, which do NOT match frame-relative `source_path`s; until
/// T211 lands, the join misses and `events = 0` (anchor-only strength).
fn hit_strength(hit: &SearchHit, ctx: &StrengthContext, now: DateTime<Utc>) -> f64 {
    let meta = hit.metadata.as_ref();
    let extra: Option<&BTreeMap<String, String>> = meta.map(|m| &m.extra_metadata);
    let source_path = extra.and_then(|m| m.get(PROVENANCE_SOURCE_PATH_KEY).map(String::as_str));
    let tier = extra.and_then(|m| m.get(PROVENANCE_TIER_KEY).map(String::as_str));

    let uri_date = extract_anchor_date(&hit.uri)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc());
    let mtime = source_path
        .and_then(|sp| ctx.resolve_source(sp, tier))
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .map(DateTime::<Utc>::from);
    let created_at = meta
        .and_then(|m| m.created_at.as_deref())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now);
    let anchor = resolve_content_anchor(uri_date, mtime, created_at);

    let (events, last_event) = match source_path {
        Some(sp) => {
            let reward = read_reward(&ctx.reward_dir, &card_id_from_path(sp));
            let events = reward
                .access_count
                .saturating_add(reward.downstream_citations);
            (
                u32::try_from(events).unwrap_or(u32::MAX),
                reward.last_reward_at,
            )
        }
        None => (0, None),
    };
    retention_strength(anchor, last_event, events, now, ctx.half_life_days)
}

/// Map memvid hits to the contract JSON. Gate OFF (`ctx = None`) is
/// byte-identical to the pre-T209 mapping: same fields, same order, no
/// `strength` key. Gate ON reorders by `score × R` DESC (stable — ties and
/// NaN keep memvid's order; ranking-only, hits are never dropped or
/// filtered) and adds the additive per-hit `strength` field.
fn render_hits(hits: Vec<SearchHit>, ctx: Option<&StrengthContext>) -> Vec<Value> {
    let Some(ctx) = ctx else {
        return hits
            .into_iter()
            .map(|h| {
                json!({
                    "frameId": h.frame_id.to_string(),
                    "snippet": h.text,
                    "score": h.score,
                })
            })
            .collect();
    };

    let now = Utc::now();
    let mut scored: Vec<(SearchHit, f64)> = hits
        .into_iter()
        .map(|h| {
            let r = hit_strength(&h, ctx, now);
            (h, r)
        })
        .collect();
    scored.sort_by(|a, b| {
        let ra = f64::from(a.0.score.unwrap_or(0.0)) * a.1;
        let rb = f64::from(b.0.score.unwrap_or(0.0)) * b.1;
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
        .into_iter()
        .map(|(h, r)| {
            json!({
                "frameId": h.frame_id.to_string(),
                "snippet": h.text,
                "score": h.score,
                "strength": r,
            })
        })
        .collect()
}

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
    let request: zen_memory::memvid_store::memvid_core::SearchRequest =
        serde_json::from_value(json!({
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
    Ok(json!({
        "hits": render_hits(response.hits, StrengthContext::resolve().as_ref())
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use zen_memory::memvid_store::memvid_core::{SearchHit, SearchHitMetadata};

    fn make_hit(
        frame_id: u64,
        uri: &str,
        score: Option<f32>,
        extra: BTreeMap<String, String>,
        created_at: Option<&str>,
    ) -> SearchHit {
        SearchHit {
            rank: 0,
            frame_id,
            uri: uri.to_string(),
            title: None,
            range: (0, 1),
            text: format!("snippet-{frame_id}"),
            matches: 1,
            chunk_range: None,
            chunk_text: None,
            score,
            metadata: Some(SearchHitMetadata {
                created_at: created_at.map(str::to_string),
                extra_metadata: extra,
                ..Default::default()
            }),
        }
    }

    fn provenance(source_path: &str, tier: &str) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        // memvid-core injects its own key alongside ours (T208 contract):
        // presence-keying must tolerate it.
        m.insert("extractous_metadata".to_string(), "{}".to_string());
        m.insert(
            PROVENANCE_SOURCE_PATH_KEY.to_string(),
            source_path.to_string(),
        );
        m.insert(PROVENANCE_TIER_KEY.to_string(), tier.to_string());
        m
    }

    fn test_ctx(root: &std::path::Path) -> StrengthContext {
        StrengthContext {
            memory_root: root.to_path_buf(),
            wiki_root: root.join("wiki"),
            reward_dir: root.join(".reward"),
            half_life_days: 30.0,
        }
    }

    fn journal_uri_days_ago(days: i64) -> String {
        format!(
            "journal-{}",
            (Utc::now() - TimeDelta::try_days(days).unwrap()).format("%Y-%m-%d")
        )
    }

    #[test]
    fn gate_off_mapping_is_byte_identical() {
        let hits = vec![
            make_hit(2, "journal-2020-01-01", Some(0.4), BTreeMap::new(), None),
            make_hit(1, "knowledge-base", Some(0.9), BTreeMap::new(), None),
        ];
        let rendered = render_hits(hits, None);
        assert_eq!(rendered.len(), 2, "never drops hits");
        // Pre-T209 order preserved (memvid order, NOT score order).
        assert_eq!(rendered[0]["frameId"], "2");
        assert_eq!(rendered[1]["frameId"], "1");
        for hit in &rendered {
            assert!(
                hit.get("strength").is_none(),
                "gate-off must not add the strength field: {hit}"
            );
            assert_eq!(
                hit.as_object().unwrap().len(),
                3,
                "exactly frameId/snippet/score"
            );
        }
    }

    #[test]
    fn gate_on_reorders_by_score_times_strength_and_adds_field() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        // Old journal anchor: R ≈ 0.5^(2000/30) ≈ 0 despite the top score.
        let old = make_hit(
            1,
            &journal_uri_days_ago(2000),
            Some(1.0),
            BTreeMap::new(),
            None,
        );
        // Fresh anchor: R ≈ 0.5^(1/30) ≈ 0.977 ⇒ 0.6 × 0.977 beats 1.0 × ~0.
        let fresh = make_hit(
            2,
            &journal_uri_days_ago(1),
            Some(0.6),
            BTreeMap::new(),
            None,
        );

        let rendered = render_hits(vec![old, fresh], Some(&ctx));
        assert_eq!(rendered.len(), 2, "ranking-only: never drops");
        assert_eq!(rendered[0]["frameId"], "2", "score × R decides");
        assert_eq!(rendered[1]["frameId"], "1");
        for hit in &rendered {
            let r = hit["strength"].as_f64().expect("strength field present");
            assert!(r > 0.0 && r <= 1.0);
        }
        assert!(
            rendered[0]["strength"].as_f64().unwrap() > rendered[1]["strength"].as_f64().unwrap()
        );
    }

    #[test]
    fn sidecar_events_raise_strength_via_source_path_join() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let now = Utc::now();
        let uri = journal_uri_days_ago(60);

        let bare = make_hit(1, &uri, Some(1.0), BTreeMap::new(), None);
        let joined = make_hit(2, &uri, Some(1.0), provenance("journal/x.md", "m2"), None);

        let r_bare = hit_strength(&bare, &ctx, now);
        let r_joined_no_sidecar = hit_strength(&joined, &ctx, now);
        assert_eq!(
            r_bare, r_joined_no_sidecar,
            "uri-date anchor outranks mtime; absent sidecar ⇒ events 0 (the honest pre-T211 join)"
        );

        zen_vault::distill::write_reward(
            &ctx.reward_dir,
            &card_id_from_path("journal/x.md"),
            &zen_vault::distill::MemoryReward {
                access_count: 7,
                downstream_citations: 0,
                correction_count: 3, // corrections never reinforce (design F1-D5)
                last_reward_at: None,
            },
        )
        .unwrap();
        let r_reinforced = hit_strength(&joined, &ctx, now);
        assert!(
            r_reinforced > r_joined_no_sidecar,
            "7 reinforcement events must extend stability: {r_reinforced} vs {r_joined_no_sidecar}"
        );
    }

    #[test]
    fn anchor_chain_uri_date_outranks_mtime_outranks_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let now = Utc::now();
        // Fresh mtime (file created now) under memory_root/journal/.
        std::fs::create_dir_all(ctx.memory_root.join("journal")).unwrap();
        std::fs::write(ctx.memory_root.join("journal/fresh.md"), "# fresh\n").unwrap();

        // mtime beats an OLD created_at (no uri date).
        let by_mtime = make_hit(
            1,
            "knowledge-base",
            Some(1.0),
            provenance("journal/fresh.md", "m2"),
            Some("2020-01-01T00:00:00Z"),
        );
        // uri date beats the same fresh mtime.
        let by_uri = make_hit(
            2,
            "journal-2020-01-01",
            Some(1.0),
            provenance("journal/fresh.md", "m2"),
            None,
        );
        let r_mtime = hit_strength(&by_mtime, &ctx, now);
        let r_uri = hit_strength(&by_uri, &ctx, now);
        assert!(r_mtime > 0.99, "fresh mtime ⇒ R ≈ 1, got {r_mtime}");
        assert!(
            r_uri < 0.2,
            "2020 uri date ⇒ deeply decayed despite fresh mtime, got {r_uri}"
        );
    }

    #[test]
    fn missing_metadata_falls_back_to_neutral_strength_one() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        // No uri date, no provenance, no created_at (legacy frame shape).
        let legacy = make_hit(1, "knowledge-base", Some(0.5), BTreeMap::new(), None);
        let r = hit_strength(&legacy, &ctx, Utc::now());
        assert_eq!(r, 1.0, "unknown age ⇒ neutral multiplier, never sunk");
    }

    #[test]
    fn resolve_maps_gate_and_fail_open() {
        // Defaults gate CLOSED (T206) ⇒ None regardless of env-free config.
        // (Env-driven enablement is covered by the config crate's own tests;
        // here we only pin that resolve() cannot panic and honors the gate.)
        let _ = StrengthContext::resolve();
    }
}
