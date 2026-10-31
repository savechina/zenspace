// PURPOSE: Spot-assert that config keys documented in the user guide
//          (docs/src/configuration/*.md) are real parseable fields — the
//          2026-09-27 audit found phantom keys (`llm_preferences`,
//          `max_sensitivity`) documented as functional that never parsed in
//          any revision. Because the config has no `deny_unknown_fields`,
//          a phantom key parses silently; the only reliable detector is a
//          sentinel round-trip: set the documented key, parse, and assert
//          the sentinel LANDED on the struct.
// USAGE: Runs under `bin/test`; extend the CASES list when the guide
//        documents new high-traffic keys.
// EXPECTED: PASS — every documented key parses and lands.
// ERRORS: A red test names the key that no longer lands — fix the doc or
//         restore/rename the serde field (mind the 4 rename exceptions:
//         `type`, `env_key`, `wire_api`, `loop`).

use zen_core::config::ZenConfig;

/// Sentinel-parse one documented TOML snippet and assert the value landed.
/// (doc_key, toml snippet, probe) — probe returns Err(key) if the sentinel
/// did NOT land, proving the key is phantom.
fn assert_key_lands(toml_src: &str, doc_key: &str, probe: impl Fn(&ZenConfig) -> bool) {
    let cfg: ZenConfig = toml::from_str(toml_src)
        .unwrap_or_else(|e| panic!("documented key `{doc_key}` broke parse: {e}"));
    assert!(
        probe(&cfg),
        "documented key `{doc_key}` is phantom: sentinel parsed silently but landed nowhere"
    );
}

#[test]
fn documented_config_keys_are_real() {
    // Top-level default provider (docs/src/configuration/overview.md).
    assert_key_lands(
        r#"default_provider = "sentinel-prov""#,
        "default_provider",
        |c| c.default_provider.as_deref() == Some("sentinel-prov"),
    );

    // Provider table incl. the renamed `type`/`env_key` keys and pricing
    // (docs/src/configuration/providers.md).
    assert_key_lands(
        r#"
[providers.demo]
type = "openai-compatible"
base_url = "https://sentinel.example"
env_key = "SENTINEL_KEY"
default_model = "sentinel-model"
input_cost_per_million = 0.25
output_cost_per_million = 1.25
"#,
        "providers.<name>.(type|base_url|env_key|default_model|*_cost_per_million)",
        |c| {
            let p = c.providers.get("demo").expect("providers.demo entry");
            p.provider_type.as_deref() == Some("openai-compatible")
                && p.base_url.as_deref() == Some("https://sentinel.example")
                && p.api_key_env.as_deref() == Some("SENTINEL_KEY")
                && p.default_model.as_deref() == Some("sentinel-model")
                && p.input_cost_per_million == Some(0.25)
                && p.output_cost_per_million == Some(1.25)
        },
    );

    // Per-agent routing incl. sensitivity (docs/src/configuration/agent-routing.md).
    assert_key_lands(
        r#"
[agents.notion_extraction]
provider = "sentinel-prov"
model = "sentinel-model"
sensitivity = "Private"
"#,
        "agents.<task>.(provider|model|sensitivity)",
        |c| {
            let a = c
                .agents
                .get("notion_extraction")
                .expect("agents.notion_extraction entry");
            a.provider.as_deref() == Some("sentinel-prov")
                && a.model.as_deref() == Some("sentinel-model")
                && a.sensitivity.is_some()
        },
    );

    // Worker cost cap (docs/src/configuration/overview.md — real since 2026-09-20).
    assert_key_lands(
        r#"
[cron]
llm_cost_cap_usd = 12.5
"#,
        "cron.llm_cost_cap_usd",
        |c| c.cron.llm_cost_cap_usd == Some(12.5),
    );

    // qqbot drain interval (docs/src — channel config scope logic).
    assert_key_lands(
        r#"
[channels.qqbot]
app_id = "sentinel-app"
client_secret = "sentinel-secret"
outbox_drain_interval_secs = 120
"#,
        "channels.qqbot.outbox_drain_interval_secs",
        |c| {
            c.channels
                .qqbot
                .as_ref()
                .and_then(|q| q.outbox_drain_interval_secs)
                == Some(120)
        },
    );

    // Loop refinement reverify window ([agentic.loop] — renamed Rust-side `loop_cfg`).
    assert_key_lands(
        r#"
[agentic.loop]
reverify_older_than_days = 3
"#,
        "agentic.loop.reverify_older_than_days",
        |c| c.agentic.loop_cfg.reverify_older_than_days == Some(3),
    );

    // Secrets Guard kill-switch ([security.secrets_guard] — FR-042).
    assert_key_lands(
        r#"
[security.secrets_guard]
enabled = false
"#,
        "security.secrets_guard.enabled=false",
        |c| c.security.secrets_guard.enabled == Some(false) && !c.secrets_guard_enabled(),
    );
    assert_key_lands(
        r#"
[security.secrets_guard]
enabled = true
"#,
        "security.secrets_guard.enabled=true",
        |c| c.security.secrets_guard.enabled == Some(true) && c.secrets_guard_enabled(),
    );
    // Absent key = guard enabled by default (FR-042 default-on).
    assert_key_lands("", "security.secrets_guard.enabled(absent)", |c| {
        c.security.secrets_guard.enabled.is_none() && c.secrets_guard_enabled()
    });

    // Background tool execution caps ([agentic.background] — design record
    // background-tool-execution-2026-10-30.md, documented in config.toml).
    assert_key_lands(
        r#"
[agentic.background]
max_wall_secs = 3600
max_concurrent = 8
"#,
        "agentic.background.(max_wall_secs|max_concurrent)",
        |c| {
            c.agentic.background.max_wall_secs == Some(3600)
                && c.agentic.background.max_concurrent == Some(8)
        },
    );

    // Gateway turn budgets ([gateway] — slow-local-model fix 2026-10-31).
    assert_key_lands(
        r#"
[gateway]
turn_watchdog_secs = 1234
local_turn_watchdog_secs = 5678
"#,
        "gateway.(turn_watchdog_secs|local_turn_watchdog_secs)",
        |c| {
            c.gateway.turn_watchdog_secs == Some(1234)
                && c.gateway.local_turn_watchdog_secs == Some(5678)
        },
    );
}
