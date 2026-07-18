use std::fmt;

#[derive(Debug)]
pub enum GatewayError {
    NotImplemented,
    Io(std::io::Error),
    Bind(String),
    /// Failed to parse a value (e.g., PID file contained non-numeric text).
    Parse(String),
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GatewayError::NotImplemented => write!(f, "Gateway method not yet implemented"),
            GatewayError::Io(e) => write!(f, "IO error: {e}"),
            GatewayError::Bind(e) => write!(f, "Bind error: {e}"),
            GatewayError::Parse(ctx) => write!(f, "Parse error: {ctx}"),
        }
    }
}

impl std::error::Error for GatewayError {}

impl From<std::io::Error> for GatewayError {
    fn from(e: std::io::Error) -> Self {
        GatewayError::Io(e)
    }
}

/// Result type alias for gateway operations.
#[allow(dead_code)]
pub type GatewayResult<T> = Result<T, GatewayError>;
