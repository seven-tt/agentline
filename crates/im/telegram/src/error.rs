use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("telegram http: {0}")]
    Http(String),

    #[error("telegram api: {0}")]
    Api(String),

    #[error("telegram parse: {0}")]
    Parse(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn http(msg: impl Into<String>) -> Self {
        Self::Http(msg.into())
    }
    pub fn api(msg: impl Into<String>) -> Self {
        Self::Api(msg.into())
    }
}

impl From<Error> for agentline_bridge::Error {
    fn from(e: Error) -> Self {
        agentline_bridge::Error::im(e.to_string())
    }
}
