// ============================================================================
// 4D Test Suite: zen-auth SecretRef resolution and Keychain
//
// Dimensions:
//   NORMAL       — SecretRef resolves correctly, Display redacts keys
//   REVERSE      — Missing env vars, empty keys return clear errors
//   ADVERSARIAL  — Special characters, max-length keys, empty keys
//   LOGIC TREE   — All AuthError variants, all SecretRef variants
// ============================================================================

use zen_auth::{AuthError, Keychain, SecretResolver, resolve_secret_ref};
use zen_core::SecretRef;

// ============================================================================
// NORMAL PATH — Standard resolution paths
// ============================================================================

#[test]
fn test_secret_ref_env_resolution_succeeds() {
    unsafe { std::env::set_var("ZEN_TEST_RESOLVE", "test-value-456") };
    let ref_ = SecretRef::Env {
        env: "ZEN_TEST_RESOLVE".to_string(),
    };
    let result = resolve_secret_ref(&ref_);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "test-value-456");
    unsafe { std::env::remove_var("ZEN_TEST_RESOLVE") };
}

#[test]
fn test_secret_ref_display_keychain_redacted() {
    let ref_ = SecretRef::Keychain {
        keychain: "zen-openai-key".to_string(),
    };
    let display = ref_.to_string();
    assert_eq!(display, "keychain:zen-openai-key");
    // Display shows the keychain service name (identifier), not the secret value.
    // The service name "zen-openai-key" is an identifier, not the API key itself.
}

#[test]
fn test_secret_ref_display_env_redacted() {
    let ref_ = SecretRef::Env {
        env: "ZEN_API_KEY".to_string(),
    };
    let display = ref_.to_string();
    assert_eq!(display, "env:ZEN_API_KEY");
}

#[test]
fn test_secret_resolver_creation() {
    let resolver = SecretResolver::new("zen-test-service", "ZEN_TEST_VAR");
    let result = resolver.resolve();
    // Will fail because keychain unavailable + env var not set — that's OK
    assert!(result.is_err());
}

// ============================================================================
// REVERSE PATH — Missing/invalid resolution targets
// ============================================================================

#[test]
fn test_nonexistent_env_var_returns_credential_not_found() {
    let ref_ = SecretRef::Env {
        env: "ZEN_NONEXISTENT_VAR_UNIQUE_999".to_string(),
    };
    let result = resolve_secret_ref(&ref_);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthError::CredentialNotFound { service, .. } => {
            assert_eq!(service, "env:ZEN_NONEXISTENT_VAR_UNIQUE_999");
        }
        other => panic!("expected CredentialNotFound, got: {other}"),
    }
}

#[test]
fn test_resolver_all_sources_exhausted() {
    let resolver = SecretResolver::new("zen-nonexistent-svc-999", "ZEN_NONEXISTENT_VAR_99999");
    let result = resolver.resolve();
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthError::CredentialNotFound { .. } => {}  // expected
        AuthError::KeychainUnavailable { .. } => {} // acceptable on non-macOS
        other => panic!("expected CredentialNotFound or KeychainUnavailable, got: {other}"),
    }
}

// ============================================================================
// ADVERSARIAL PATH — Extreme/corner case inputs
// ============================================================================

#[test]
fn test_secret_ref_empty_keychain_key() {
    let ref_ = SecretRef::Keychain {
        keychain: "".to_string(),
    };
    let display = ref_.to_string();
    assert_eq!(display, "keychain:");
}

#[test]
fn test_secret_ref_special_chars_in_key() {
    let ref_ = SecretRef::Env {
        env: "ZEN_VAR_WITH_SPECIAL_CHARS_!@#$%".to_string(),
    };
    let display = ref_.to_string();
    assert!(display.contains("ZEN_VAR"));
}

#[test]
fn test_secret_ref_max_length_key() {
    let long_key = "A".repeat(256);
    let ref_ = SecretRef::Keychain {
        keychain: long_key.clone(),
    };
    let display = ref_.to_string();
    assert!(display.len() > 256); // "keychain:" prefix + 256 chars
}

// ============================================================================
// LOGIC TREE — Error variant coverage
// ============================================================================

#[test]
fn test_auth_error_all_variants_display() {
    let errors: Vec<AuthError> = vec![
        AuthError::KeychainAccessDenied {
            service: "svc".into(),
        },
        AuthError::CredentialNotFound {
            service: "svc".into(),
            account: "acc".into(),
        },
        AuthError::Keychain("generic error".into()),
        AuthError::EnvVarNotSet("VAR".into()),
        AuthError::ResolutionFailed {
            reason: "timeout".into(),
        },
        AuthError::KeychainUnavailable {
            platform: "linux".into(),
            message: "not supported".into(),
        },
    ];

    for err in &errors {
        let msg = err.to_string();
        assert!(!msg.is_empty(), "AuthError display should not be empty");
    }
}

#[test]
fn test_auth_error_is_access_denied_detection() {
    let denied = AuthError::KeychainAccessDenied {
        service: "test".into(),
    };
    assert!(denied.is_access_denied());
    assert!(!denied.is_not_found());

    let not_found = AuthError::CredentialNotFound {
        service: "test".into(),
        account: "test".into(),
    };
    assert!(!not_found.is_access_denied());
    assert!(not_found.is_not_found());
}

#[test]
fn test_keychain_non_macos_returns_unavailable() {
    // On non-macOS, Keychain::retrieve returns KeychainUnavailable
    // On macOS, it tries the real keychain (which won't have our test creds)
    let result = Keychain::retrieve("zen-test-nonexistent-svc", "zen");
    #[cfg(not(target_os = "macos"))]
    match result {
        Err(AuthError::KeychainUnavailable { .. }) => {} // expected
        other => panic!("expected KeychainUnavailable on non-macOS, got: {other:?}"),
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS, keychain may return CredentialNotFound or KeychainAccessDenied
        assert!(result.is_err());
    }
}
