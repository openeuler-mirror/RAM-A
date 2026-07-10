use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("invalid input: {message}")]
    InvalidInput { message: String },
    #[error("embedding error: {message}")]
    Embedding { message: String },
    #[error("rerank error: {message}")]
    Rerank { message: String },
    #[error("store error: {0}")]
    Store(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store backend error: {message}")]
    StoreBackend { message: String },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type MemoryResult<T> = Result<T, MemoryError>;
