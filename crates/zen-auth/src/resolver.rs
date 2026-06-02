use crate::keychain::{AuthError, Keychain};

// Re-export SecretRef from zen-core for convenience
pub use zen_core::secrets::SecretRef;

pub fn resolve_secret_ref(ref_: &SecretRef) -> Result<String, AuthError> {
    match ref_ {
        SecretRef::Keychain { keychain } => Keychain::retrieve(keychain, "zen"),
        SecretRef::Env { env } => std::env::var(env).map_err(|_| AuthError::CredentialNotFound {
            service: format!("env:{env}"),
            account: "zen".to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// SecretResolver — T034: Resolution chain with fallback
// ---------------------------------------------------------------------------

/// Resolver that tries multiple sources for a single secret.
///
/// Resolution order:
/// 1. Keychain (via `Keychain::retrieve`)
/// 2. Environment variable fallback
/// 3. [`AuthError::CredentialNotFound`]
pub struct SecretResolver {
    /// Keychain service name (e.g. "zen-openai-api-key").
    keychain_service: String,
    /// Environment variable name fallback (e.g. "ZEN_OPENAI_API_KEY").
    env_var: String,
}

impl SecretResolver {
    pub fn new(keychain_service: &str, env_var: &str) -> Self {
        Self {
            keychain_service: keychain_service.to_string(),
            env_var: env_var.to_string(),
        }
    }

    /// Try Keychain first, then fall back to env var, then fail.
    pub fn resolve(&self) -> Result<String, AuthError> {
        // 1) Keychain
        match Keychain::retrieve(&self.keychain_service, "zen") {
            Ok(val) => return Ok(val),
            Err(AuthError::KeychainAccessDenied { .. })
            | Err(AuthError::CredentialNotFound { .. })
            | Err(AuthError::KeychainUnavailable { .. }) => {
                tracing::debug!(
                    "keychain unavailable for '{}', falling back to env var",
                    self.keychain_service
                );
            }
            Err(e) => return Err(e),
        }

        // 2) Environment variable
        match std::env::var(&self.env_var) {
            Ok(val) => return Ok(val),
            Err(_) => {
                tracing::debug!(
                    "env var '{}' not set, secret resolution failed",
                    self.env_var
                );
            }
        }

        // 3) All sources exhausted
        Err(AuthError::CredentialNotFound {
            service: self.keychain_service.clone(),
            account: format!("env:{}", self.env_var),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_ref_display_keychain() {
        let ref_ = SecretRef::Keychain {
            keychain: "zen-openai-api-key".to_string(),
        };
        assert_eq!(ref_.to_string(), "keychain:zen-openai-api-key");
    }

    #[test]
    fn test_secret_ref_display_env() {
        let ref_ = SecretRef::Env {
            env: "ZEN_OPENAI_API_KEY".to_string(),
        };
        assert_eq!(ref_.to_string(), "env:ZEN_OPENAI_API_KEY");
    }

    #[test]
    fn test_secret_ref_env_resolution() {
        // Set a known env var, resolve via SecretRef.
        // Safety: single-threaded test; no concurrent env mutation.
        unsafe { std::env::set_var("ZEN_TEST_SECRET", "test-value-123") };
        let ref_ = SecretRef::Env {
            env: "ZEN_TEST_SECRET".to_string(),
        };

        let result = resolve_secret_ref(&ref_);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test-value-123");

        // Safety: same reasoning.
        unsafe { std::env::remove_var("ZEN_TEST_SECRET") };
    }

    #[test]
    fn test_secret_ref_env_not_set() {
        let ref_ = SecretRef::Env {
            env: "ZEN_NONEXISTENT_VAR_12345".to_string(),
        };
        let result = resolve_secret_ref(&ref_);
        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::CredentialNotFound { service, .. } => {
                assert_eq!(service, "env:ZEN_NONEXISTENT_VAR_12345");
            }
            other => panic!("expected CredentialNotFound, got: {other}"),
        }
    }

    #[test]
    fn test_secret_resolver_env_fallback() {
        // Env var exists, keychain will fail — should resolve via env.
        // Safety: single-threaded test.
        unsafe { std::env::set_var("ZEN_TEST_FALLBACK", "fallback-value") };
        let resolver = SecretResolver::new("zen-test-fallback-service", "ZEN_TEST_FALLBACK");

        let result = resolver.resolve();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "fallback-value");

        // Safety: same reasoning.
        unsafe { std::env::remove_var("ZEN_TEST_FALLBACK") };
    }

    #[test]
    fn test_secret_resolver_all_fail() {
        let resolver =
            SecretResolver::new("zen-nonexistent-service-123", "ZEN_NONEXISTENT_VAR_99999");
        let result = resolver.resolve();
        assert!(result.is_err());
    }
}
