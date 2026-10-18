//! W2.2 (D12): the artifact-output path convention. A declared artifact
//! path is validated with the SAME sandbox gate `fs.write` uses before the
//! sub-turn spawns, and only agents that already hold the `fs.write` grant
//! may declare one (the permission matrix is unchanged).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rig_compose::budget::AtomicTokenBudget;
use rig_compose::tool::Tool;
use zen_agents::delegate_task::{DelegateTaskTool, SharedKnowledge, SharedSensitivity};
use zen_agents::wiring::ZenWiring;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmConfig};

fn tool() -> DelegateTaskTool {
    let router = DefaultRouter::new(LlmConfig {
        default_provider: Some("mock".to_string()),
        ..Default::default()
    });
    let sensitivity: SharedSensitivity = Arc::new(Mutex::new(Sensitivity::Public));
    let knowledge: SharedKnowledge = Arc::new(Mutex::new(Vec::new()));
    DelegateTaskTool::new(
        Arc::new(ZenWiring::new()),
        router,
        Vec::new(),
        None,
        sensitivity,
        knowledge,
        Arc::new(AtomicTokenBudget::new(100_000)),
        Duration::from_secs(30),
        1,
        4,
    )
}

/// Isolate each test in its own home + workspace, then build the tool from
/// the derived roots (exactly how `ZenWiring::new` derives them).
fn isolated(dir: &std::path::Path) -> DelegateTaskTool {
    unsafe {
        std::env::set_var("ZEN_HOME", dir.join("home"));
        std::env::set_var("ZEN_WORKSPACE", dir);
    }
    tool()
}

fn clear_env() {
    unsafe {
        std::env::remove_var("ZEN_HOME");
        std::env::remove_var("ZEN_WORKSPACE");
    }
}

#[tokio::test]
async fn artifact_escape_is_rejected_before_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let tool = isolated(dir.path());
    let out = tool
        .invoke(serde_json::json!({
            "agent": "Hephaestus",
            "prompt": "write the report",
            "artifact": "/tmp/zen_artifact_escape.md"
        }))
        .await
        .unwrap();
    assert!(
        out["error"]
            .as_str()
            .unwrap_or_default()
            .contains("artifact path rejected"),
        "escape must be denied by the sandbox gate: {out}"
    );
    assert!(!std::path::Path::new("/tmp/zen_artifact_escape.md").exists());
    clear_env();
}

#[tokio::test]
async fn artifact_env_path_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = isolated(dir.path());
    let target = dir.path().join(".env");
    let out = tool
        .invoke(serde_json::json!({
            "agent": "Hephaestus",
            "prompt": "write the report",
            "artifact": target.to_str().unwrap()
        }))
        .await
        .unwrap();
    assert!(
        out["error"]
            .as_str()
            .unwrap_or_default()
            .contains("artifact path rejected"),
        "env-file artifact must be rejected: {out}"
    );
    clear_env();
}

#[tokio::test]
async fn no_grant_agent_cannot_declare_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let tool = isolated(dir.path());
    let target = dir.path().join("out.md");
    let out = tool
        .invoke(serde_json::json!({
            "agent": "Explore",
            "prompt": "write the report",
            "artifact": target.to_str().unwrap()
        }))
        .await
        .unwrap();
    let error = out["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("fs.write"),
        "an agent without the fs.write grant cannot produce an artifact: {out}"
    );
    assert!(!target.exists(), "nothing may be written: {out}");
    clear_env();
}

#[tokio::test]
async fn valid_artifact_returns_pointer_and_bounded_summary() {
    let dir = tempfile::tempdir().unwrap();
    let tool = isolated(dir.path());
    let target = dir.path().join("reports/out.md");
    let out = tool
        .invoke(serde_json::json!({
            "agent": "Hephaestus",
            "prompt": "write the report",
            "artifact": target.to_str().unwrap()
        }))
        .await
        .unwrap();
    assert!(out["error"].is_null(), "{out}");
    assert_eq!(
        out["artifact_path"].as_str(),
        Some(target.to_str().unwrap()),
        "{out}"
    );
    assert!(
        out["summary"].as_str().is_some(),
        "artifact results carry a bounded summary: {out}"
    );
    clear_env();
}
