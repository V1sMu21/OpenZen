use thiserror::Error;

#[derive(Debug, Error)]
pub enum StewardError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("store lock poisoned")]
    Lock,
}

pub type Result<T> = std::result::Result<T, StewardError>;
