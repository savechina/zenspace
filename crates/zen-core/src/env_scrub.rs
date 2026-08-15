//! Child-process environment scrubbing (FR-037).
//!
//! Strips secret-bearing variables from the environment inherited by
//! subprocess spawns (`shell.exec`, MCP stdio transport) so an agent-driven
//! child can never echo `*_API_KEY`-style credentials back out.

use serde::{Deserialize, Serialize};

const ALWAYS_SCRUB: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "BRAVE_SEARCH_API_KEY",
    "TAVILY_API_KEY",
    "GITHUB_TOKEN",
    "AWS_SECRET_ACCESS_KEY",
];

const SCRUB_SUFFIXES: &[&str] = &["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD", "_CREDENTIAL"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvScrubConfig {
    /// Additional var names to always scrub (joined with pattern rules).
    #[serde(default)]
    pub extra_scrub: Vec<String>,
    /// Names exempt from scrubbing (exact match wins over pattern match).
    #[serde(default)]
    pub allowlist: Vec<String>,
}

/// Should this environment variable name be scrubbed?
pub fn is_secret_var(name: &str, config: &EnvScrubConfig) -> bool {
    if config.allowlist.iter().any(|a| a == name) {
        return false;
    }
    if ALWAYS_SCRUB.contains(&name) {
        return true;
    }
    if config.extra_scrub.iter().any(|s| s == name) {
        return true;
    }
    let upper = name.to_uppercase();
    SCRUB_SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
}

/// Produce the child environment: the parent env minus every secret-bearing
/// var, plus the caller's explicit `inject` entries (the only sanctioned way
/// to pass variables into a child). Inject entries are held to the same
/// standard (SC-017: child env contains ZERO secret-bearing vars) — a caller
/// cannot smuggle `*_API_KEY`-style names or loader-injection vars back in.
pub fn scrubbed_env(
    inject: &std::collections::HashMap<String, String>,
    config: &EnvScrubConfig,
) -> std::collections::HashMap<String, String> {
    let mut out = std::env::vars()
        .filter(|(name, _)| !is_secret_var(name, config))
        .filter(|(name, _)| !is_loader_injection_var(name))
        .collect::<std::collections::HashMap<_, _>>();
    for (k, v) in inject {
        if !is_secret_var(k, config) && !is_loader_injection_var(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Loader-injection vars stripped from both parent env and inject entries —
/// mirrors the startup hardening in `process_hardening` so a child cannot
/// re-arm `LD_PRELOAD`-style hooks the host already removed.
fn is_loader_injection_var(name: &str) -> bool {
    matches!(
        name,
        "LD_PRELOAD" | "LD_LIBRARY_PATH" | "DYLD_INSERT_LIBRARIES" | "DYLD_LIBRARY_PATH"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg() -> EnvScrubConfig {
        EnvScrubConfig::default()
    }

    #[test]
    fn pattern_suffixes_matched_case_insensitively() {
        assert!(is_secret_var("MY_SERVICE_API_KEY", &cfg()));
        assert!(is_secret_var("db_password", &cfg()));
        assert!(is_secret_var("SESSION_TOKEN", &cfg()));
        assert!(is_secret_var("CLIENT_SECRET", &cfg()));
        assert!(is_secret_var("AUTH_CREDENTIAL", &cfg()));
    }

    #[test]
    fn known_secrets_always_scrubbed() {
        for name in ALWAYS_SCRUB {
            assert!(is_secret_var(name, &cfg()), "{name}");
        }
    }

    #[test]
    fn benign_vars_kept() {
        for name in ["PATH", "HOME", "LANG", "TERM_PROGRAM", "CARGO_TARGET_DIR"] {
            assert!(!is_secret_var(name, &cfg()), "{name}");
        }
    }

    #[test]
    fn allowlist_exempts_pattern_match() {
        let config = EnvScrubConfig {
            extra_scrub: vec![],
            allowlist: vec!["PUBLIC_TOKEN".to_string()],
        };
        assert!(!is_secret_var("PUBLIC_TOKEN", &config));
    }

    #[test]
    fn extra_scrub_extends_list() {
        let config = EnvScrubConfig {
            extra_scrub: vec!["MY_CUSTOM_VAR".to_string()],
            allowlist: vec![],
        };
        assert!(is_secret_var("MY_CUSTOM_VAR", &config));
    }

    #[test]
    fn scrubbed_env_strips_real_parent_secrets() {
        // SAFETY: single-threaded test; std::env::set_var is process-global.
        unsafe {
            std::env::set_var("ZEN_TEST_SCRUB_API_KEY", "leak-me");
        }
        let env = scrubbed_env(&HashMap::new(), &cfg());
        assert!(!env.contains_key("ZEN_TEST_SCRUB_API_KEY"));
        assert!(env.contains_key("PATH"), "PATH must survive scrubbing");
        unsafe {
            std::env::remove_var("ZEN_TEST_SCRUB_API_KEY");
        }
    }

    #[test]
    fn inject_is_the_only_addition_path() {
        let mut inject = HashMap::new();
        inject.insert("CUSTOM_FLAG".to_string(), "1".to_string());
        let env = scrubbed_env(&inject, &cfg());
        assert_eq!(env.get("CUSTOM_FLAG").map(String::as_str), Some("1"));
    }

    #[test]
    fn inject_cannot_reintroduce_scrubbed_or_loader_vars() {
        // SC-017: the child env must contain ZERO secret-bearing vars — the
        // inject map is model-controlled, so it cannot smuggle *_API_KEY
        // names back in, nor re-arm LD_PRELOAD-style loader injection that
        // process_hardening strips at startup.
        let mut inject = HashMap::new();
        inject.insert("OPENAI_API_KEY".to_string(), "attacker-value".to_string());
        inject.insert("LD_PRELOAD".to_string(), "/tmp/evil.so".to_string());
        inject.insert(
            "DYLD_INSERT_LIBRARIES".to_string(),
            "/tmp/evil.dylib".to_string(),
        );
        let env = scrubbed_env(&inject, &cfg());
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("LD_PRELOAD"));
        assert!(!env.contains_key("DYLD_INSERT_LIBRARIES"));
    }
}
