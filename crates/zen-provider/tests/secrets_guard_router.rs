// PURPOSE: Integration pins for the Secrets Guard router last mile (FR-042
//          T270): a cloud egress receives a REDACTED payload (observed via
//          the mock's prompt_len echo), the reply's placeholders restore,
//          invented tokens stay visible, and the kill-switch yields
//          verbatim passthrough.
// USAGE: cargo test -p zen-provider --test secrets_guard_router
// EXPECTED: PASS — all pins green.
// ERRORS: a red pin means the router last mile regressed to verbatim
//         egress, lost the restore side, or leaked raw values.

use zen_provider::{DefaultRouter, LlmConfig, LlmRouter as _, Provider};

const SECRET: &str = "GHP_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";

/// Serializes tests in THIS binary: the kill-switch pin mutates the
/// process-wide env + config cache, so guard-state reads across sibling
/// pins must not interleave with it (plain `cargo test` runs these on
/// threads in one process; nextest isolation already serializes).
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Pin (i): the cloud provider receives a redacted prompt — the mock
/// echoes prompt_len; 40-char secret becomes the 11-char placeholder.
#[test]
fn cloud_payload_is_redacted_before_provider() {
    let _serial = TEST_LOCK.lock().expect("test lock");
    let router = DefaultRouter::new(LlmConfig::default()).with_mock_response("echo");
    let out = router
        .call(Provider::Mock, &format!("nm {SECRET} tail"))
        .expect("call succeeds");
    let redacted_len = "nm ".len() + "⟦SECRET_1⟧".len() + " tail".len();
    assert!(
        out.contains(&format!("prompt_len={redacted_len}")),
        "redacted payload observed: {out}"
    );
    assert!(!out.contains(SECRET), "no raw secret escapes: {out}");
}

/// Pin (ii): a placeholder in the provider REPLY restores to the true
/// value from this session's mapping.
#[test]
fn reply_placeholder_restores_to_true_value() {
    let _serial = TEST_LOCK.lock().expect("test lock");
    let router = DefaultRouter::new(LlmConfig::default()).with_mock_response("found ⟦SECRET_1⟧ ok");
    let out = router
        .call(Provider::Mock, &format!("tell me about {SECRET}"))
        .expect("call succeeds");
    assert!(out.contains(SECRET), "restored reply: {out}");
    assert!(!out.contains("⟦SECRET_1⟧"));
}

/// Pin (iii): an invented token beyond the mapping stays VISIBLE (never
/// fabricated over).
#[test]
fn invented_placeholder_stays_visible() {
    let _serial = TEST_LOCK.lock().expect("test lock");
    let router =
        DefaultRouter::new(LlmConfig::default()).with_mock_response("invented ⟦SECRET_9⟧ here");
    let out = router
        .call(Provider::Mock, "clean prompt")
        .expect("call succeeds");
    assert!(out.contains("⟦SECRET_9⟧"), "leftover visible: {out}");
    assert!(out.contains("invented"), "text intact: {out}");
}

/// Pin (iv): kill-switch env yields byte-identical passthrough — the
/// mock's prompt_len reflects the RAW (unredacted) prompt.
#[test]
fn kill_switch_env_yields_verbatim_passthrough() {
    let _serial = TEST_LOCK.lock().expect("test lock");
    // SAFETY: single-threaded test env mutation per repo convention.
    unsafe {
        std::env::set_var("ZEN_SECRETS_GUARD", "off");
        zen_core::config::invalidate_config_cache();
    }
    let router = DefaultRouter::new(LlmConfig::default()).with_mock_response("echo");
    let out = router
        .call(Provider::Mock, &format!("nm {SECRET} tail"))
        .expect("call succeeds");
    let raw_len = "nm ".len() + SECRET.len() + " tail".len();
    assert!(
        out.contains(&format!("prompt_len={raw_len}")),
        "verbatim passthrough: {out}"
    );
    // restore the process default for sibling tests
    unsafe {
        std::env::remove_var("ZEN_SECRETS_GUARD");
        zen_core::config::invalidate_config_cache();
    }
}
