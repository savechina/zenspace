use thiserror::Error;

// ---------------------------------------------------------------------------
// AuthError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("keychain access denied for service: {service}")]
    KeychainAccessDenied { service: String },

    #[error("credential not found: service = {service}, account = {account}")]
    CredentialNotFound { service: String, account: String },

    #[error("keychain error: {0}")]
    Keychain(String),

    #[error("environment variable not set: {0}")]
    EnvVarNotSet(String),

    #[error("secret resolution failed: {reason}")]
    ResolutionFailed { reason: String },

    #[error("keychain unavailable on {platform}: {message}")]
    KeychainUnavailable { platform: String, message: String },
}

impl AuthError {
    pub fn is_access_denied(&self) -> bool {
        matches!(self, AuthError::KeychainAccessDenied { .. })
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, AuthError::CredentialNotFound { .. })
    }
}

// ---------------------------------------------------------------------------
// Platform-specific Keychain implementation (FR-061d)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod platform {
    use super::AuthError;
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const ERRSEC_AUTH_FAILED: i32 = -25293;
    const ERRSEC_ITEM_NOT_FOUND: i32 = -25300;
    const ERRSEC_NOT_AVAILABLE: i32 = -25311;
    const ERRSEC_PERMISSION_DENIED: i32 = -25294;

    pub fn store(service: &str, account: &str, password: &str) -> Result<(), AuthError> {
        set_generic_password(service, account, password.as_bytes())
            .map_err(|e| map_err(&e, service))
    }

    pub fn retrieve(service: &str, account: &str) -> Result<String, AuthError> {
        get_generic_password(service, account)
            .map(|bytes| String::from_utf8(bytes).unwrap_or_default())
            .map_err(|e| map_err(&e, service))
    }

    pub fn delete(service: &str, account: &str) -> Result<(), AuthError> {
        delete_generic_password(service, account).map_err(|e| map_err(&e, service))
    }

    fn map_err(err: &security_framework::base::Error, service: &str) -> AuthError {
        let code = err.code();

        if code == ERRSEC_AUTH_FAILED
            || code == ERRSEC_PERMISSION_DENIED
            || code == ERRSEC_NOT_AVAILABLE
        {
            return AuthError::KeychainAccessDenied {
                service: service.to_string(),
            };
        }

        if code == ERRSEC_ITEM_NOT_FOUND {
            return AuthError::CredentialNotFound {
                service: service.to_string(),
                account: String::new(),
            };
        }

        AuthError::Keychain(format!("{err}"))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::AuthError;

    pub fn store(_service: &str, _account: &str, _password: &str) -> Result<(), AuthError> {
        let platform = std::env::consts::OS;
        tracing::warn!(
            "[AUTH] Keychain unavailable on {}, store is a no-op. Use env vars instead.",
            platform
        );
        Ok(())
    }

    pub fn retrieve(_service: &str, _account: &str) -> Result<String, AuthError> {
        let platform = std::env::consts::OS;
        Err(AuthError::KeychainUnavailable {
            platform: platform.to_string(),
            message: "Keychain not supported on this platform. Use SecretRef::Env or api_key_env fallback.".into(),
        })
    }

    pub fn delete(_service: &str, _account: &str) -> Result<(), AuthError> {
        let platform = std::env::consts::OS;
        tracing::warn!(
            "[AUTH] Keychain unavailable on {}, delete is a no-op.",
            platform
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Keychain — public API delegates to platform module
// ---------------------------------------------------------------------------

pub struct Keychain;

impl Keychain {
    pub fn store(service: &str, account: &str, password: &str) -> Result<(), AuthError> {
        platform::store(service, account, password)
    }

    pub fn retrieve(service: &str, account: &str) -> Result<String, AuthError> {
        platform::retrieve(service, account)
    }

    pub fn delete(service: &str, account: &str) -> Result<(), AuthError> {
        platform::delete(service, account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_error_is_access_denied() {
        let e = AuthError::KeychainAccessDenied {
            service: "zen-test".to_string(),
        };
        assert!(e.is_access_denied());
        assert!(!e.is_not_found());
    }

    #[test]
    fn test_auth_error_is_not_found() {
        let e = AuthError::CredentialNotFound {
            service: "zen-test".to_string(),
            account: "test".to_string(),
        };
        assert!(e.is_not_found());
        assert!(!e.is_access_denied());
    }

    #[test]
    fn test_keychain_unavailable_error() {
        let e = AuthError::KeychainUnavailable {
            platform: "linux".to_string(),
            message: "test".into(),
        };
        assert!(!e.is_access_denied());
        assert!(!e.is_not_found());
    }
}
