use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("operation not supported by this backend")]
    NotSupported,

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("im error: {0}")]
    Im(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn agent(msg: impl Into<String>) -> Self {
        Self::Agent(msg.into())
    }
    pub fn im(msg: impl Into<String>) -> Self {
        Self::Im(msg.into())
    }
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}
