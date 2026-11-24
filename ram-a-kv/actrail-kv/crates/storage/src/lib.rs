//! 本 crate 定义采集请求的存储接口并构造本地后端。

mod backends;
mod config;
mod filter;
mod runtime;

use actrail_kv_artifacts::CapturedRequest;
use futures_util::{future::BoxFuture, stream::BoxStream};
use thiserror::Error;

pub use backends::{open_legacy_ndjson, open_store, AppendError, NdjsonAppender, StoreAccess};
pub use config::{NdjsonConfig, RetentionConfig, SqliteConfig, StorageConfig};
pub use filter::CaptureFilter;

pub type CaptureStream = BoxStream<'static, Result<CapturedRequest, StorageError>>;

pub trait CaptureStore: Send + Sync {
    /// Completes once the backend has accepted the whole record.
    fn append(&self, record: CapturedRequest) -> BoxFuture<'_, Result<(), StorageError>>;

    /// Opens a bounded view of committed records in a stable backend order.
    fn scan(&self, filter: CaptureFilter) -> BoxFuture<'_, Result<CaptureStream, StorageError>>;
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("invalid storage configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid capture filter: {0}")]
    InvalidFilter(String),
    #[error("invalid capture time {0:?}; expected an RFC 3339 timestamp")]
    InvalidTime(String),
    #[error("capture store was opened read-only")]
    ReadOnly,
    #[error("record is already outside the configured capture retention period")]
    ExpiredCapture,
    #[error("capture snapshot expired during the scan")]
    SnapshotExpired,
    #[error("capture storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("capture JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("capture storage worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
    #[error("{backend} storage failed: {source}")]
    Backend {
        backend: &'static str,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
