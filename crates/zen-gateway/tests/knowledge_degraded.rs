//! G4-01: the `knowledge/search` response carries an additive `degraded`
//! array, and its absence on the happy path is byte-identical to the
//! pre-G4-01 `{"notes":[...]}` shape.
//!
//! O16: a query-side embedding degradation (hash fallback, or no embedding at
//! all) is surfaced through the same array instead of staying log-only.
//!
//! The assembly is pinned through its deterministic, I/O-free core
//! (`build_search_response`, `query_embedding_degraded_entry`) rather than a
//! live `SearchService`, so the present-only-on-failure contract is exercised
//! without a database or a provider.

use serde_json::json;
use zen_gateway::server::knowledge::{build_search_response, query_embedding_degraded_entry};

#[test]
fn happy_path_is_byte_identical_without_degraded_key() {
    let notes = vec![json!({"path": "/a.md", "content": "hi"})];
    let response = build_search_response(notes.clone(), Vec::new());

    assert!(response.get("notes").is_some(), "notes always present");
    assert!(
        response.get("degraded").is_none(),
        "no failures ⇒ no `degraded` key (byte-identical happy path): {response}"
    );
    assert_eq!(response, json!({ "notes": notes }));
}

#[test]
fn degraded_key_present_only_when_a_tier_failed() {
    let degraded = vec![json!({"tier": "vec", "error": "embedding model unavailable"})];
    let response = build_search_response(Vec::new(), degraded.clone());

    let entries = response
        .get("degraded")
        .and_then(|v| v.as_array())
        .expect("`degraded` array present on failure");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["tier"], json!("vec"));
    assert_eq!(entries[0]["error"], json!("embedding model unavailable"));
    assert_eq!(response, json!({ "notes": [], "degraded": degraded }));
}

#[test]
fn healthy_query_embedding_produces_no_entry() {
    assert_eq!(query_embedding_degraded_entry(false, false, &[]), None);
}

#[test]
fn hash_fallback_query_embedding_is_surfaced_as_a_vec_entry() {
    let entry = query_embedding_degraded_entry(true, false, &[]).expect("degraded ⇒ entry");
    assert_eq!(entry["tier"], json!("vec"));
    let error = entry["error"].as_str().expect("error is a string");
    assert!(
        error.contains("hash fallback") && error.contains("not semantic"),
        "the message must name the cause and the effect: {error}"
    );

    let response = build_search_response(vec![json!({"path": "/a.md"})], vec![entry]);
    let entries = response["degraded"].as_array().expect("degraded present");
    assert_eq!(entries.len(), 1, "one entry, not one per scan dir");
    assert_eq!(
        response["notes"].as_array().map(Vec::len),
        Some(1),
        "hits are still returned"
    );
}

#[test]
fn unavailable_query_embedding_wins_over_degraded() {
    let entry = query_embedding_degraded_entry(true, true, &[]).expect("unavailable ⇒ entry");
    let error = entry["error"].as_str().expect("error is a string");
    assert!(error.contains("unavailable"), "{error}");
}

#[test]
fn an_existing_vec_entry_is_not_duplicated() {
    let existing = vec![json!({"tier": "vec", "error": "tier blew up"})];
    assert_eq!(query_embedding_degraded_entry(true, true, &existing), None);
}
