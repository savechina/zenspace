use serde::Deserialize;
use std::fmt;

// ---------------------------------------------------------------------------
// SecretRef — FR-061c: TOML inline-table secret reference (serde-only)
// ---------------------------------------------------------------------------
//
// This type is defined in zen-core (not zen-auth) to avoid circular dependencies.
// zen-core → zen-auth is valid, but zen-auth → zen-core → zen-auth would be circular.
//
// Resolution logic (Keychain::retrieve) lives in zen-auth/src/resolver.rs.
// zen-provider imports SecretRef from zen-core and resolution from zen-auth.

/// Represents a secret reference in configuration.
///
/// Deserializes from TOML inline-table formats:
/// - `{ keychain: "zen-openai-api-key" }` — look up in macOS Keychain
/// - `{ env: "ZEN_OPENAI_API_KEY" }` — look up in environment variables
///
/// Per FR-061c: Resolution chain logged in audit trail.
///
/// # Example TOML
///
/// ```toml
/// [providers.openai]
/// type = "openai"
/// base_url = "https://api.openai.com"
/// api_key = { keychain: "zen-openai-api-key" }
/// # or fallback:
/// # api_key = { env: "ZEN_OPENAI_API_KEY" }
/// ```
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SecretRef {
    /// Look up in macOS Keychain (service name like "zen-openai-api-key").
    Keychain { keychain: String },
    /// Look up in an environment variable.
    Env { env: String },
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretRef::Keychain { keychain } => write!(f, "keychain:{keychain}"),
            SecretRef::Env { env } => write!(f, "env:{env}"),
        }
    }
}

impl SecretRef {
    /// Create a Keychain reference with the standard naming convention.
    ///
    /// Per FR-061b: `zen-{provider}-{credential-type}` format.
    pub fn keychain_for_provider(provider: &str, credential_type: &str) -> Self {
        SecretRef::Keychain {
            keychain: format!("zen-{provider}-{credential_type}"),
        }
    }

    /// Create an environment variable reference with standard naming.
    ///
    /// Per FR-061c: `ZEN_{PROVIDER}_API_KEY` convention.
    pub fn env_for_provider(provider: &str) -> Self {
        SecretRef::Env {
            env: format!("ZEN_{}_API_KEY", provider.to_uppercase()),
        }
    }

    /// Get the default env var name for a provider (legacy fallback).
    ///
    /// Returns `"{PROVIDER}_API_KEY"` format used by existing code.
    pub fn legacy_env_var(provider: &str) -> String {
        format!("{}_API_KEY", provider.to_uppercase())
    }
}
