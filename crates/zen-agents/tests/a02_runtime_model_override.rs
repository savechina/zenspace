//! A-02 (D2b) runtime model override + row13 forecast/cost producer pins.
//!
//! One capturing OpenAI-compatible echo server observes the model NAME the
//! wired provider actually received, so each pin discriminates the real
//! seam (not a label). Provider selection is exercised through the same
//! `DefaultRouter` the executor drives — sensitivity routing runs BEFORE
//! the override, which only renames the model within the routed provider.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rig_compose::budget::AtomicTokenBudget;
use rig_compose::tool::Tool;
use zen_agents::delegate_task::{DelegateTaskTool, SharedKnowledge, SharedSensitivity};
use zen_agents::plan_task::PlanExecuteTool;
use zen_agents::review::QualityPipeline;
use zen_agents::wiring::ZenWiring;
use zen_core::types::Sensitivity;
use zen_provider::DefaultRouter;

const ECHO_KEY_ENV: &str = "ZEN_TEST_A02_MODEL_KEY";
const CONFIGURED_DEFAULT: &str = "test-model";
const HINT: &str = "hint-x";
/// Priced echo usage: 111 prompt + 22 completion tokens at 1.0 / 2.0 USD
/// per million ⇒ 0.000111 + 0.000044.
const EXPECTED_COST_USD: f64 = 0.000_155;

struct Echo {
    port: u16,
    captured: Arc<Mutex<Vec<String>>>,
}

impl Echo {
    fn models(&self) -> Vec<String> {
        let bodies = self.captured.lock().unwrap_or_else(|p| p.into_inner());
        bodies
            .iter()
            .filter_map(|raw| {
                let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(raw);
                serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(String::from))
            })
            .collect()
    }

    fn request_count(&self) -> usize {
        self.captured
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }
}

/// Serves any number of `/chat/completions` calls, capturing each request
/// body and echoing a fixed completion with a REAL usage block (W12.12
/// harness shape, extended to keep the requests).
fn spawn_capturing_echo() -> Echo {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    std::thread::spawn(move || {
        let body = r#"{"id":"chatcmpl-a02","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"echo reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":111,"completion_tokens":22,"total_tokens":133}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break buf.len(),
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
            let content_length: usize = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            let target = head_end + 4 + content_length;
            while buf.len() < target {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            sink.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(String::from_utf8_lossy(&buf).to_string());
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    Echo { port, captured }
}

fn router_for(port: u16) -> DefaultRouter {
    DefaultRouter::from_agentic(&zen_core::config::ZenConfig {
        default_provider: Some("echo-test".to_string()),
        providers: HashMap::from([(
            "echo-test".to_string(),
            zen_core::config::ProviderConfig {
                provider_type: Some("openai".to_string()),
                base_url: Some(format!("http://127.0.0.1:{port}/v1")),
                default_model: Some(CONFIGURED_DEFAULT.to_string()),
                api_key_env: Some(ECHO_KEY_ENV.to_string()),
                input_cost_per_million: Some(1.0),
                output_cost_per_million: Some(2.0),
                ..Default::default()
            },
        )]),
        ..Default::default()
    })
}

fn delegate_tool(router: DefaultRouter, sensitivity: Sensitivity) -> DelegateTaskTool {
    let sensitivity: SharedSensitivity = Arc::new(Mutex::new(sensitivity));
    let knowledge: SharedKnowledge = Arc::new(Mutex::new(Vec::new()));
    DelegateTaskTool::new(
        Arc::new(ZenWiring::new()),
        router,
        Vec::new(),
        None,
        sensitivity,
        knowledge,
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(AtomicTokenBudget::new(100_000)),
        Duration::from_secs(30),
        1,
        4,
    )
}

fn plan_tool(router: DefaultRouter, state_db: std::path::PathBuf) -> PlanExecuteTool {
    let delegate = delegate_tool(router, Sensitivity::Public);
    PlanExecuteTool::new(
        Arc::new(delegate),
        QualityPipeline::new(),
        4,
        Some(state_db),
    )
}

/// Isolate ZEN_HOME/ZEN_WORKSPACE and stage a model catalog containing the
/// hint (admission validates hints against `[providers.*.models]` keys).
fn isolate() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var(ECHO_KEY_ENV, "test-key");
        std::env::set_var("ZEN_HOME", dir.path());
        std::env::set_var("ZEN_WORKSPACE", dir.path());
    }
    std::fs::write(
        dir.path().join("config.toml"),
        format!("[providers.openai.models.{HINT}]\nmodel = \"{HINT}\"\n"),
    )
    .unwrap();
    zen_core::config::invalidate_config_cache();
    dir
}

fn clear_env() {
    unsafe {
        std::env::remove_var(ECHO_KEY_ENV);
        std::env::remove_var("ZEN_HOME");
        std::env::remove_var("ZEN_WORKSPACE");
    }
    zen_core::config::invalidate_config_cache();
}

#[tokio::test]
async fn a02_delegate_hint_switches_runtime_model() {
    let _home = isolate();
    let echo = spawn_capturing_echo();
    let tool = delegate_tool(router_for(echo.port), Sensitivity::Public);

    let out = tool
        .invoke(serde_json::json!({
            "agent": "Librarian",
            "prompt": "organize the shelf",
            "model": HINT
        }))
        .await
        .unwrap();

    assert!(out["error"].is_null(), "sub-turn must succeed: {out}");
    assert_eq!(out["model_source"], "hint", "{out}");
    assert_eq!(out["model"], HINT, "the real model name is recorded: {out}");
    assert_eq!(
        echo.models(),
        vec![HINT.to_string()],
        "the provider must receive the HINT model name"
    );
    clear_env();
}

#[tokio::test]
async fn a02_profile_wins_over_hint_byte_identical() {
    let _home = isolate();
    let echo = spawn_capturing_echo();
    let tool = delegate_tool(router_for(echo.port), Sensitivity::Public);

    // Explore carries an explicit CloudOnly LlmPreference — the ladder's
    // Profile rung wins, so the hint must never reach the wire.
    let out = tool
        .invoke(serde_json::json!({
            "agent": "Explore",
            "prompt": "survey the docs",
            "model": HINT
        }))
        .await
        .unwrap();

    assert!(out["error"].is_null(), "sub-turn must succeed: {out}");
    assert_eq!(out["model_source"], "profile", "{out}");
    assert_eq!(out["model"], CONFIGURED_DEFAULT, "{out}");
    assert_eq!(
        echo.models(),
        vec![CONFIGURED_DEFAULT.to_string()],
        "profile-wins keeps the provider default (override=None)"
    );
    clear_env();
}

#[tokio::test]
async fn a02_sensitivity_gate_still_governs_provider() {
    let _home = isolate();
    let echo = spawn_capturing_echo();
    // Private data + a cloud-routed default: the gate refuses BEFORE any
    // wire call. The hint can never widen egress.
    let tool = delegate_tool(router_for(echo.port), Sensitivity::Private);

    let out = tool
        .invoke(serde_json::json!({
            "agent": "Librarian",
            "prompt": "organize private notes",
            "model": HINT
        }))
        .await
        .unwrap();

    assert!(
        out["error"].is_string(),
        "the sensitivity gate must refuse the cloud turn: {out}"
    );
    assert_eq!(
        echo.request_count(),
        0,
        "no request may reach the cloud provider for Private data"
    );
    clear_env();
}

#[tokio::test]
async fn a02_no_hint_is_byte_identical() {
    let _home = isolate();
    let echo = spawn_capturing_echo();
    let tool = delegate_tool(router_for(echo.port), Sensitivity::Public);

    let out = tool
        .invoke(serde_json::json!({
            "agent": "Librarian",
            "prompt": "organize the shelf"
        }))
        .await
        .unwrap();

    assert!(out["error"].is_null(), "sub-turn must succeed: {out}");
    assert_eq!(out["model_source"], "default", "{out}");
    assert_eq!(out["model"], CONFIGURED_DEFAULT, "{out}");
    assert_eq!(
        echo.models(),
        vec![CONFIGURED_DEFAULT.to_string()],
        "no hint ⇒ the router receives None and the configured default is used"
    );
    clear_env();
}

#[tokio::test]
async fn row13_checkpoint_records_real_model_and_cost() {
    let _home = isolate();
    let echo = spawn_capturing_echo();
    let state_db = _home.path().join("state.db");
    let tool = plan_tool(router_for(echo.port), state_db.clone());

    let out = tool
        .invoke(serde_json::json!({
            "name": "a02",
            "tasks": [{ "id": "a", "agent": "Librarian", "prompt": "x", "model": HINT }]
        }))
        .await
        .unwrap();
    assert!(out["error"].is_null(), "plan must succeed: {out}");

    let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
    let repo = zen_repo::WorkflowRepo::new(&client);
    let history = repo.duration_history().await.unwrap();
    let row = history
        .iter()
        .find(|r| r.agent == "Librarian")
        .unwrap_or_else(|| panic!("checkpoint row for the plan task: {history:?}"));
    assert_eq!(
        row.model.as_deref(),
        Some(HINT),
        "the checkpoint stores the REAL model, not a label: {history:?}"
    );
    let cost = row.cost_usd.expect("usage + pricing ⇒ cost is metered");
    assert!(
        (cost - EXPECTED_COST_USD).abs() < 1e-9,
        "cost from the real usage echo + configured pricing: {cost}"
    );
    clear_env();
}

#[tokio::test]
async fn row13_forecast_key_matches_checkpoint_key() {
    let _home = isolate();
    let echo = spawn_capturing_echo();
    let state_db = _home.path().join("state.db");
    let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
    let repo = zen_repo::WorkflowRepo::new(&client);
    repo.create_plan("seed", None, "{}", 1_000).await.unwrap();
    // Seed a row under the RESOLVED default model name (what the checkpoint
    // now writes for a hintless task). The pre-A-02 lookup key ("") would
    // miss it entirely.
    repo.checkpoint_task(zen_repo::TaskCheckpoint {
        plan_id: "seed",
        task_id: "h1",
        agent: "Librarian",
        status: "completed",
        response: Some("done"),
        error: None,
        now: 1_100,
        model: Some(CONFIGURED_DEFAULT),
        duration_ms: Some(120_000),
        cost_usd: None,
    })
    .await
    .unwrap();
    drop(client);

    let tool = plan_tool(router_for(echo.port), state_db);
    let out = tool
        .invoke(serde_json::json!({
            "name": "a02-forecast",
            "tasks": [{ "id": "a", "agent": "Librarian", "prompt": "x" }]
        }))
        .await
        .unwrap();
    assert!(out["error"].is_null(), "{out}");
    let progress = out["progress"].as_array().expect("progress blob");
    assert_eq!(
        progress[0],
        serde_json::json!("plan 1 tasks · ~2-2 min (estimate)"),
        "the forecast must key on the same resolved model the checkpoint stores: {progress:?}"
    );
    clear_env();
}

/// D3 (eng-review 2026-11-20, outside #6): a hint is admission-validated
/// against the SENSITIVITY-ROUTED provider's models, not the global union.
/// Phase 1 — `hint-x` sits in the global catalog (provider `openai`) but
/// the routed provider (`echo-test`) does not serve it: the hard gate
/// rejects BEFORE the wire (zero echo requests, zero budget spent). Phase
/// 2 — after `echo-test` gains the model, the same invoke succeeds and
/// the echo observes the switched model name.
#[tokio::test]
async fn a02_hint_outside_routed_provider_rejected_before_wire() {
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var(ECHO_KEY_ENV, "test-key");
        std::env::set_var("ZEN_HOME", dir.path());
        std::env::set_var("ZEN_WORKSPACE", dir.path());
    }
    let write_config = |extra: &str| {
        std::fs::write(
            dir.path().join("config.toml"),
            format!(
                "[providers.openai.models.{HINT}]\nmodel = \"{HINT}\"\n\
                 [providers.echo-test.models.test-model]\nmodel = \"test-model\"\n{extra}"
            ),
        )
        .unwrap();
        zen_core::config::invalidate_config_cache();
    };
    write_config("");

    let echo = spawn_capturing_echo();
    let tool = delegate_tool(router_for(echo.port), Sensitivity::Public);

    let rejected = tool
        .invoke(serde_json::json!({
            "agent": "Librarian",
            "prompt": "organize the shelf",
            "model": HINT
        }))
        .await
        .unwrap();
    let message = rejected["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("not served by the sensitivity-routed provider \"echo-test\""),
        "admission must name the routed provider: {rejected}"
    );
    assert!(
        !message.contains("not in the configured provider catalog"),
        "the GLOBAL check must pass; only the routed check may fire: {rejected}"
    );
    assert_eq!(
        echo.request_count(),
        0,
        "rejection happens before the wire — no budget spent, no request sent"
    );

    write_config(&format!(
        "[providers.echo-test.models.{HINT}]\nmodel = \"{HINT}\"\n"
    ));
    let accepted = tool
        .invoke(serde_json::json!({
            "agent": "Librarian",
            "prompt": "organize the shelf",
            "model": HINT
        }))
        .await
        .unwrap();
    assert!(accepted["error"].is_null(), "{accepted}");
    assert_eq!(accepted["model_source"], "hint", "{accepted}");
    let models = echo.models();
    assert_eq!(
        models.last(),
        Some(&HINT.to_string()),
        "the runtime must switch the model name within the routed provider: {models:?}"
    );
    clear_env();
}
