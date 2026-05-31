use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Retryable,
    UserAction,
    SystemError,
    SafetyBlock,
}

impl std::fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorCategory::Retryable => write!(f, "retryable"),
            ErrorCategory::UserAction => write!(f, "user-action"),
            ErrorCategory::SystemError => write!(f, "system-error"),
            ErrorCategory::SafetyBlock => write!(f, "safety-block"),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing configuration file: {path}")]
    MissingFile { path: String },

    #[error("failed to parse configuration at {path}: {reason}")]
    ParseError { path: String, reason: String },

    #[error("invalid configuration value for key '{key}': {reason}")]
    ValidationError { key: String, reason: String },

    #[error("required environment variable not set: {variable}")]
    MissingEnvVar { variable: String },
}

#[derive(Debug, Error)]
pub enum PathError {
    #[error("path does not exist: {path}")]
    NotFound { path: String },

    #[error("path is not a directory: {path}")]
    NotADirectory { path: String },

    #[error("path is not a file: {path}")]
    NotAFile { path: String },

    #[error("insufficient permissions for path: {path}")]
    PermissionDenied { path: String },

    #[error("could not resolve home directory")]
    HomeDirNotFound,
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("failed to parse JSON: {reason}")]
    JsonError { reason: String },

    #[error("failed to parse TOML at {location}: {reason}")]
    TomlError { location: String, reason: String },

    #[error("failed to parse integer '{input}': {reason}")]
    IntError { input: String, reason: String },
}

#[derive(Debug, Error)]
pub enum AgenticError {
    #[error("LLM provider unavailable: {provider} — {reason}")]
    LlmProviderUnavailable { provider: String, reason: String },

    #[error("LLM routing failed: {provider} — {reason}")]
    LlmRoutingFailed { provider: String, reason: String },

    #[error("LLM response invalid: expected {expected}, got {actual}")]
    LlmResponseInvalid { expected: String, actual: String },

    #[error("LLM rate limited: {provider}, retry after {retry_after_secs}s")]
    LlmRateLimited {
        provider: String,
        retry_after_secs: u64,
    },

    #[error("LLM context overflow: {tokens_used} tokens used, limit {limit}")]
    LlmContextOverflow { tokens_used: u64, limit: u64 },

    #[error("knowledge base empty: {path}")]
    KnowledgeBaseEmpty { path: String },

    #[error("knowledge search failed: {reason}")]
    KnowledgeSearchFailed { reason: String },

    #[error("consolidation failed: error = {error}")]
    KnowledgeConsolidateFailed { error: String },

    #[error("note parse failed: file = {file}, error = {error}")]
    KnowledgeNoteFailed { file: String, error: String },

    #[error("QQ Bot connection failed: {reason}")]
    QqBotConnectionFailed { reason: String },

    #[error("QQ Bot authentication failed: {reason}")]
    QqBotAuthFailed { reason: String },

    #[error("QQ Bot rate limited, retry after {retry_after_secs}s")]
    QqBotRateLimited { retry_after_secs: u64 },

    #[error("FTS5 search failed: {reason}")]
    SearchFts5Failed { reason: String },

    #[error("vector search failed: {reason}")]
    SearchVectorFailed { reason: String },

    #[error("graph search failed: {reason}")]
    SearchGraphFailed { reason: String },

    #[error("keychain access denied: service = {service}")]
    MacosKeychainDenied { service: String },

    #[error("accessibility permission required: {feature}")]
    MacosAccessibilityDenied { feature: String },

    #[error("plugin load failed: {plugin_id} — {error}")]
    PluginLoadFailed { plugin_id: String, error: String },

    #[error("plugin sandbox violation: {plugin_id} tried {operation}")]
    PluginSandboxViolation {
        plugin_id: String,
        operation: String,
    },

    #[error("plugin permission denied: {plugin_id} needs {permission}")]
    PluginPermissionDenied {
        plugin_id: String,
        permission: String,
    },
}

impl AgenticError {
    pub fn category(&self) -> ErrorCategory {
        match self {
            AgenticError::LlmRateLimited { .. }
            | AgenticError::LlmProviderUnavailable { .. }
            | AgenticError::LlmRoutingFailed { .. } => ErrorCategory::Retryable,

            AgenticError::LlmResponseInvalid { .. } | AgenticError::LlmContextOverflow { .. } => {
                ErrorCategory::UserAction
            }

            AgenticError::KnowledgeBaseEmpty { .. }
            | AgenticError::KnowledgeSearchFailed { .. }
            | AgenticError::KnowledgeConsolidateFailed { .. }
            | AgenticError::KnowledgeNoteFailed { .. } => ErrorCategory::SystemError,

            AgenticError::QqBotConnectionFailed { .. } | AgenticError::QqBotAuthFailed { .. } => {
                ErrorCategory::UserAction
            }
            AgenticError::QqBotRateLimited { .. } => ErrorCategory::Retryable,

            AgenticError::SearchFts5Failed { .. }
            | AgenticError::SearchVectorFailed { .. }
            | AgenticError::SearchGraphFailed { .. } => ErrorCategory::SystemError,

            AgenticError::MacosKeychainDenied { .. }
            | AgenticError::MacosAccessibilityDenied { .. } => ErrorCategory::UserAction,

            AgenticError::PluginSandboxViolation { .. } => ErrorCategory::SafetyBlock,
            AgenticError::PluginLoadFailed { .. } | AgenticError::PluginPermissionDenied { .. } => {
                ErrorCategory::UserAction
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("message: {0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum ZenError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("path error: {0}")]
    Path(#[from] PathError),

    #[error("agentic error: {0} [category: {1}]")]
    Agentic(AgenticError, ErrorCategory),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(#[from] ParseError),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("service error: {0}")]
    Service(String),

    #[error("message: {0}")]
    Message(String),
}

impl ZenError {
    pub fn category(&self) -> Option<ErrorCategory> {
        match self {
            ZenError::Agentic(_, cat) => Some(*cat),
            ZenError::Config(_) => Some(ErrorCategory::UserAction),
            ZenError::Path(_) => Some(ErrorCategory::UserAction),
            ZenError::Io(e) => {
                use std::io::ErrorKind;
                match e.kind() {
                    ErrorKind::PermissionDenied | ErrorKind::NotFound => {
                        Some(ErrorCategory::UserAction)
                    }
                    ErrorKind::TimedOut | ErrorKind::Interrupted => Some(ErrorCategory::Retryable),
                    _ => Some(ErrorCategory::SystemError),
                }
            }
            ZenError::Parse(_) => Some(ErrorCategory::UserAction),
            ZenError::Serialization(_) => Some(ErrorCategory::SystemError),
            ZenError::Service(_) => Some(ErrorCategory::SystemError),
            ZenError::Message(_) => Some(ErrorCategory::UserAction),
        }
    }
}

impl From<AgenticError> for ZenError {
    fn from(err: AgenticError) -> Self {
        let cat = err.category();
        ZenError::Agentic(err, cat)
    }
}

impl From<ServiceError> for ZenError {
    fn from(err: ServiceError) -> Self {
        match err {
            ServiceError::Io(e) => ZenError::Io(e),
            ServiceError::Message(m) => ZenError::Service(m),
        }
    }
}
