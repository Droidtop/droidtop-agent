/// Everything that can go wrong in the core, in words a person can read:
/// the agent prints these and droidtop shows them as the one line a sync
/// reports.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the connection's encryption failed ({0})")]
    Tls(String),
    #[error("a message could not be read ({0})")]
    Json(#[from] serde_json::Error),
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("the other side is not a paired device")]
    NotPaired,
    #[error("the computer said: {0}")]
    Remote(String),
    #[error("{0}")]
    Protocol(String),
    #[error("the key is not valid")]
    BadKey,
}

impl From<rustls::Error> for Error {
    fn from(e: rustls::Error) -> Self {
        Error::Tls(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn protocol(message: impl Into<String>) -> Error {
    Error::Protocol(message.into())
}
