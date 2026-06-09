use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("feishu http: {0}")]
    Http(String),

    #[error("feishu api: {0}")]
    Api(String),

    #[error("feishu auth: {0}")]
    Auth(String),

    #[error("feishu parse: {0}")]
    Parse(String),

    #[error("feishu webhook: {0}")]
    Webhook(String),

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
    pub fn auth(msg: impl Into<String>) -> Self {
        Self::Auth(msg.into())
    }
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

impl From<Error> for agentline_bridge::Error {
    fn from(e: Error) -> Self {
        agentline_bridge::Error::im(e.to_string())
    }
}
