use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("acp: {0}")]
    Acp(#[from] agentline_agent_acp::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

impl From<Error> for agentline_bridge::Error {
    fn from(e: Error) -> Self {
        agentline_bridge::Error::agent(e.to_string())
    }
}
