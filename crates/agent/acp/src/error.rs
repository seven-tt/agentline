use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ACP protocol: {0}")]
    Protocol(String),

    #[error("agent not running")]
    NotRunning,

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("permission request not pending: {0}")]
    NoPendingPermission(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
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
