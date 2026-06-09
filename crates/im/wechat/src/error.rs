use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("http: {0}")]
    Http(String),

    #[error("login: {0}")]
    Login(String),

    #[error("api: ret={ret} {msg}")]
    Api { ret: i32, msg: String },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("base64: {0}")]
    Base64(#[from] base64::DecodeError),

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
        agentline_bridge::Error::im(e.to_string())
    }
}
