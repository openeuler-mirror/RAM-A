//! 本模块根据配置和读取权限构造采集存储实例。

mod ndjson;
mod sqlite;

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::config::ResolvedStorageConfig;
use crate::{runtime::schedule_retention, CaptureStore, StorageConfig, StorageError};

pub use ndjson::{AppendError, NdjsonAppender};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreAccess {
    ReadOnly,
    ReadWrite,
}

pub async fn open_store(
    config: StorageConfig,
    access: StoreAccess,
) -> Result<Arc<dyn CaptureStore>, StorageError> {
    match config.into_backend()? {
        ResolvedStorageConfig::Sqlite(config) => {
            let retention = config.retention_policy();
            let store = Arc::new(
                tokio::task::spawn_blocking(move || sqlite::SqliteStore::open(config, access))
                    .await??,
            );
            if let (StoreAccess::ReadWrite, Some(config)) = (access, retention) {
                schedule_retention(&store, config, sqlite::SqliteStore::prune);
            }
            Ok(store)
        }
        ResolvedStorageConfig::Ndjson(config) => {
            let retention = config.retention_policy();
            let store = Arc::new(
                tokio::task::spawn_blocking(move || ndjson::NdjsonStore::open(config, access))
                    .await??,
            );
            if let (StoreAccess::ReadWrite, Some(config)) = (access, retention) {
                schedule_retention(&store, config, ndjson::NdjsonStore::prune);
            }
            Ok(store)
        }
    }
}

pub async fn open_legacy_ndjson(path: PathBuf) -> Result<Arc<dyn CaptureStore>, StorageError> {
    let appender = tokio::task::spawn_blocking(move || NdjsonAppender::open(path)).await??;
    Ok(Arc::new(appender))
}

fn open_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}
