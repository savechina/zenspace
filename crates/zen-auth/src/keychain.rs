use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use thiserror::Error;

// ---------------------------------------------------------------------------
// AuthError
// ---------------------------------------------------------------------------

/// Auth-specific errors (keychain operations, credential resolution).
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
}

impl AuthError {
    /// Returns `true` if this is an access/permission-related error.
    pub fn is_access_denied(&self) -> bool {
        matches!(self, AuthError::KeychainAccessDenied { .. })
    }

    /// Returns `true` if the credential was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(self, AuthError::CredentialNotFound { .. })
    }
}

// ---------------------------------------------------------------------------
// Keychain — T033: macOS Keychain access via security-framework
// ---------------------------------------------------------------------------

/// macOS credential storage backed by the Keychain.
///
/// Service naming convention: `zen-{provider}-{type}`
/// Example: `zen-openai-api-key`, `zen-jento-bot-token`
pub struct Keychain;

// macOS Keychain OSStatus codes (from Security framework)
const ERRSEC_AUTH_FAILED: i32 = -25293;
const ERRSEC_ITEM_NOT_FOUND: i32 = -25300;
const ERRSEC_NOT_AVAILABLE: i32 = -25311;
const ERRSEC_PERMISSION_DENIED: i32 = -25294;

impl Keychain {
    /// Store a password for the given service (e.g. "zen-openai-api-key").
    pub fn store(service: &str, account: &str, password: &str) -> Result<(), AuthError> {
        set_generic_password(service, account, password.as_bytes())
            .map_err(|e| map_err(&e, service))
    }

    /// Retrieve a password for the given service.
    pub fn retrieve(service: &str, account: &str) -> Result<String, AuthError> {
        get_generic_password(service, account)
            .map(|bytes| String::from_utf8(bytes).unwrap_or_default())
            .map_err(|e| map_err(&e, service))
    }

    /// Delete a stored password for the given service.
    pub fn delete(service: &str, account: &str) -> Result<(), AuthError> {
        delete_generic_password(service, account).map_err(|e| map_err(&e, service))
    }
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
}
