use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("dingtalk http: {0}")]
    Http(String),

    #[error("dingtalk websocket: {0}")]
    Ws(String),

    #[error("dingtalk api: {0}")]
    Api(String),

    #[error("dingtalk parse: {0}")]
    Parse(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn http(msg: impl Into<String>) -> Self {
        Self::Http(msg.into())
    }
    pub fn ws(msg: impl Into<String>) -> Self {
        Self::Ws(msg.into())
    }
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

impl From<Error> for agentline_im_core::Error {
    fn from(e: Error) -> Self {
        agentline_im_core::Error::im(e.to_string())
    }
}
