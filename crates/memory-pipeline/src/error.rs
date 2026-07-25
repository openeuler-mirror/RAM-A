use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Protocol(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, PipelineError>;
