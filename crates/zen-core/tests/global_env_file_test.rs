//! Pins for the global `.env` load in `load_config()` (FIX-1, 2026-11-21):
//! the daemonized binary (launchd cwd=`/`) can never resolve a cwd-relative
//! `.env`, so the global root's `.env` is the daemon's designated surface for
//! search API keys and egress proxy vars. dotenvy never overrides variables
//! already present, so a dev-shell cwd `.env` keeps priority.
//!
//! Runs under nextest (process-per-test): env-var mutation is process-global
//! and the config cache is per-process.

use tempfile::TempDir;

/// Create a temp `ZEN_HOME`, optionally write `<home>/.env`, and return the
/// guard plus home path.
fn setup_global_home(env_body: Option<&str>) -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().expect("temp dir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    if let Some(body) = env_body {
        std::fs::write(home.join(".env"), body).expect("write .env");
    }
    // SAFETY: nextest runs each test in its own process, so env var
    // isolation is guaranteed.
    unsafe {
        std::env::set_var("ZEN_HOME", &home);
    }
    (tmp, home)
}

#[test]
fn global_env_file_values_reach_the_process_env() {
    // SAFETY: process-per-test isolation.
    unsafe {
        std::env::remove_var("ZEN_TEST_GLOBAL_ENV_FROM_FILE");
    }
    let (_guard, _home) = setup_global_home(Some("ZEN_TEST_GLOBAL_ENV_FROM_FILE=global_value\n"));

    let _config = zen_core::config::load_config().expect("load_config with global .env");

    assert_eq!(
        std::env::var("ZEN_TEST_GLOBAL_ENV_FROM_FILE").as_deref(),
        Ok("global_value"),
        "global .env values must be visible after load_config"
    );
}

#[test]
fn global_env_file_never_overrides_pre_set_env() {
    // SAFETY: process-per-test isolation.
    unsafe {
        std::env::set_var("ZEN_TEST_GLOBAL_ENV_PRESET", "shell_value");
    }
    let (_guard, _home) = setup_global_home(Some("ZEN_TEST_GLOBAL_ENV_PRESET=file_value\n"));

    let _config = zen_core::config::load_config().expect("load_config with pre-set env var");

    assert_eq!(
        std::env::var("ZEN_TEST_GLOBAL_ENV_PRESET").as_deref(),
        Ok("shell_value"),
        "dotenvy must not override a variable already present in the environment"
    );
}
