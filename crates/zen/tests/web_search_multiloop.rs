//! T104 (T059 contract): `web.search` provider wiring — Brave→Tavily→DDG
//! fallback order with clean failures when keys are absent.
//!
//! The live multi-search leg needs network (DDG); it runs `#[ignore]`d so
//! the offline gate stays hermetic. The executable tests pin the wiring:
//! missing-arg rejection and provider-override key errors resolve without
//! touching the network.

use rig_compose::tool::Tool;
use tempfile::tempdir;
use zen_core::config::invalidate_config_cache;
use zen_plugin::tools::web_search::WebSearchTool;

/// Global-layer isolation (`<ZEN_HOME>/config.toml` — the unambiguous
/// layer; the workspace layer's `.zen/` vs bare path is disputed
/// code-vs-docs, out of scope here). All phases run in ONE test: sibling
/// tests share the process env, so parallel env mutation would race.
/// One frozen ZEN_HOME for all phases (`user_root()` is a process-once
/// `LazyLock` in non-test builds); the config FILE is rewritten +
/// cache-invalidated between phases.
/// One binary, sequential phases — no env races with siblings.
fn frozen_home() -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    unsafe { std::env::set_var("ZEN_HOME", temp.path()) };
    unsafe { std::env::remove_var("ZEN_WORKSPACE") };
    unsafe { std::env::remove_var("BRAVE_SEARCH_API_KEY") };
    unsafe { std::env::remove_var("TAVILY_API_KEY") };
    temp
}

fn stage_global(home: &std::path::Path, toml_body: &str) {
    std::fs::write(home.join("config.toml"), toml_body).unwrap();
    invalidate_config_cache();
}

#[tokio::test]
async fn provider_wiring_rejects_without_network() {
    // SAFETY: test-only env mutation; no other (non-ignored) test in this
    // binary touches these vars.
    let home = frozen_home();
    let tool = WebSearchTool::new();

    stage_global(home.path(), "");
    let err = tool
        .invoke(serde_json::json!({"max_results": 3}))
        .await
        .expect_err("missing query must be rejected");
    assert!(err.to_string().contains("query"), "got: {err}");

    stage_global(home.path(), "[web_search]\ndefault_provider = \"brave\"\n");
    let err = tool
        .invoke(serde_json::json!({"query": "rust async"}))
        .await
        .expect_err("brave without key must fail, not hang or panic");
    assert!(
        err.to_string().contains("brave"),
        "error must name the provider, got: {err}"
    );

    stage_global(home.path(), "[web_search]\ndefault_provider = \"tavily\"\n");
    let err = tool
        .invoke(serde_json::json!({"query": "rust async"}))
        .await
        .expect_err("tavily without key must fail, not hang or panic");
    assert!(
        err.to_string().contains("tavily"),
        "error must name the provider, got: {err}"
    );
}

/// Live leg (T059 quickstart §8): two consecutive searches in one turn
/// resolve through the DDG fallback with `🔧/✅` intermediates before
/// synthesis. Requires network — run explicitly with `-- --ignored`.
#[tokio::test]
#[ignore]
async fn live_two_consecutive_ddg_searches() {
    let home = frozen_home();
    stage_global(home.path(), "");
    let tool = WebSearchTool::new();
    for query in [
        "rust tokio spawn_blocking",
        "rust async_trait object safety",
    ] {
        let out = tool
            .invoke(serde_json::json!({"query": query, "max_results": 2}))
            .await
            .expect("live DDG search");
        assert_eq!(out["provider"], "duckduckgo");
        assert!(out["count"].as_u64().unwrap() >= 1, "got: {out}");
    }
}
