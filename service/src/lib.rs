pub mod api;
pub mod config;
mod mirror;
pub mod model;
pub mod recall;
pub mod store;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Invalid(String),
    /// 409. `kind` is the response `type`, such as `precondition_failed`.
    Conflict {
        kind: &'static str,
        message: String,
        paths: Vec<String>,
    },
    NotFound,
    Internal(anyhow::Error),
}
impl Error {
    pub fn conflict(kind: &'static str, message: &str, paths: Vec<String>) -> Self {
        Self::Conflict {
            kind,
            message: message.into(),
            paths,
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) => f.write_str(s),
            Self::Conflict { kind, message, .. } => write!(f, "{kind}: {message}"),
            Self::NotFound => f.write_str("Not found"),
            Self::Internal(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(e.into())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(e.into())
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(e.into())
    }
}
