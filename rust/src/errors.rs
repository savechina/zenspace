use anyhow::Result;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ZenError {
    #[error("Command failed: {0}")]
    Command(String),

    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Message  error: {0}")]
    Message(String),

    #[error("Service failed.{0}")]
    Service(#[from] ServiceError),

    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error), // Catch-all for other errors
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("Application Provider is not available.{0}")]
    Provider(String),

    #[error("Internal framework error: {0}")]
    Internal(String),
    #[error("Message  error: {0}")]
    Message(String),
    // #[error("Join error: {0}")]
    // JoinError(#[from] tokio::task::JoinError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    // #[error("Network error: {0}")]
    // Network(#[from] reqwest::Error),
    #[error("Serialization/Deserialization error: {0}")]
    Serde(#[from] serde_json::Error),
    // #[error("URL parsing error: {0}")]
    // UrlParse(#[from] url::ParseError),
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error), // Catch-all for other errors
}
