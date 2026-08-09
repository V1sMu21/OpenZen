use thiserror::Error;

#[derive(Error, Debug)]
pub enum MemoryError {
    #[error("memory not found: {0}")]
    NotFound(String),

    #[error("cache capacity exceeded")]
    CapacityExceeded,

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("WAL write error: {0}")]
    WalWrite(String),

    #[error("WAL read error: {0}")]
    WalRead(String),

    #[error("internal error: {0}")]
    Internal(String),
}

pub type MemoryResult<T> = Result<T, MemoryError>;
