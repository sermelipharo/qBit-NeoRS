/// Error type of every fallible core API call.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error))]
pub enum QbtError {
    #[error("not found: {message}")]
    NotFound { message: String },
    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },
    #[error("conflict: {message}")]
    Conflict { message: String },
    #[error("I/O error: {message}")]
    Io { message: String },
    #[error("network error: {message}")]
    Network { message: String },
    #[error("engine error: {message}")]
    Engine { message: String },
    #[error("unsupported: {message}")]
    Unsupported { message: String },
    #[error("{message}")]
    Other { message: String },
}

impl QbtError {
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::NotFound { message: m.into() }
    }
    pub fn invalid(m: impl Into<String>) -> Self {
        Self::InvalidArgument { message: m.into() }
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        Self::Conflict { message: m.into() }
    }
    pub fn io(m: impl Into<String>) -> Self {
        Self::Io { message: m.into() }
    }
    pub fn network(m: impl Into<String>) -> Self {
        Self::Network { message: m.into() }
    }
    pub fn engine(m: impl Into<String>) -> Self {
        Self::Engine { message: m.into() }
    }
    pub fn unsupported(m: impl Into<String>) -> Self {
        Self::Unsupported { message: m.into() }
    }
    pub fn other(m: impl Into<String>) -> Self {
        Self::Other { message: m.into() }
    }
}

impl From<std::io::Error> for QbtError {
    fn from(e: std::io::Error) -> Self {
        Self::io(e.to_string())
    }
}

impl From<anyhow::Error> for QbtError {
    fn from(e: anyhow::Error) -> Self {
        Self::engine(format!("{e:#}"))
    }
}

impl From<serde_json::Error> for QbtError {
    fn from(e: serde_json::Error) -> Self {
        Self::invalid(e.to_string())
    }
}

pub type QbtResult<T> = Result<T, QbtError>;
