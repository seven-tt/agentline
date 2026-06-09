use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("codex sdk: {0}")]
    Sdk(String),

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("agent not running")]
    NotRunning,

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn sdk(msg: impl Into<String>) -> Self {
        Self::Sdk(msg.into())
    }
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

impl From<Error> for agentline_bridge::Error {
    fn from(e: Error) -> Self {
        agentline_bridge::Error::agent(e.to_string())
    }
}
