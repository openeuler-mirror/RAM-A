//! 存储实例使用的已解析参数及输出路径保护。
use super::{defaults::default_cleanup_seconds, NdjsonConfig, SqliteConfig};
use crate::StorageConfigError;
use std::{
    ffi::OsString,
    fs,
    num::NonZeroU32,
    path::{Path, PathBuf},
};
use time::{Duration, OffsetDateTime};

#[derive(Clone, Debug)]
pub enum ResolvedStorageConfig {
    Sqlite(SqliteConfig),
    Ndjson(NdjsonConfig),
}
#[derive(Clone, Copy, Debug)]
pub struct RetentionConfig {
    pub max_age_seconds: NonZeroU32,
    pub cleanup_interval_seconds: NonZeroU32,
}
impl RetentionConfig {
    pub(super) fn resolve(
        max_age: Option<NonZeroU32>,
        interval: Option<NonZeroU32>,
    ) -> Option<Self> {
        max_age.map(|max_age_seconds| Self {
            max_age_seconds,
            cleanup_interval_seconds: interval.unwrap_or_else(default_cleanup_seconds),
        })
    }

    pub fn cutoff(self) -> OffsetDateTime {
        OffsetDateTime::now_utc() - Duration::seconds(self.max_age_seconds.get().into())
    }

    pub fn check(self, captured_at: OffsetDateTime) -> Result<(), StorageConfigError> {
        if captured_at < self.cutoff() {
            return Err(StorageConfigError::ExpiredCapture);
        }
        Ok(())
    }
}

impl ResolvedStorageConfig {
    pub fn validate_output(
        &self,
        config_file: Option<&Path>,
        output: &Path,
    ) -> Result<(), StorageConfigError> {
        let output_path = match output.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = output.parent().filter(|path| !path.as_os_str().is_empty());
                let name = output.file_name().ok_or_else(|| {
                    StorageConfigError::InvalidConfig("output needs a file name".into())
                })?;
                parent
                    .unwrap_or_else(|| Path::new("."))
                    .canonicalize()?
                    .join(name)
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(config_file) = config_file {
            if same_file_if_exists(config_file, &output_path)? {
                return Err(StorageConfigError::InvalidConfig(
                    "output overlaps with the storage configuration".into(),
                ));
            }
        }
        let overlaps = match self {
            Self::Ndjson(config) => output_path.starts_with(config.directory.canonicalize()?),
            Self::Sqlite(config) => {
                let db_path = config.path.canonicalize()?;
                let mut overlaps = false;
                for suffix in ["", "-wal", "-shm", "-journal"] {
                    let mut path: OsString = db_path.as_os_str().to_owned();
                    path.push(suffix);
                    let path = Path::new(&path);
                    overlaps |= output_path == path || same_file_if_exists(path, &output_path)?;
                }
                overlaps
            }
        };
        if overlaps {
            return Err(StorageConfigError::InvalidConfig(
                "output overlaps with the capture store".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn resolve_path(
    path: &mut PathBuf,
    relative_to: &Path,
) -> Result<(), StorageConfigError> {
    if path.as_os_str().is_empty() {
        return Err(StorageConfigError::InvalidConfig(
            "storage path must not be empty".into(),
        ));
    }
    if path.is_relative() {
        *path = relative_to.join(&*path);
    }
    Ok(())
}

fn same_file_if_exists(left: &Path, right: &Path) -> Result<bool, StorageConfigError> {
    match same_path_identity(left, right) {
        Ok(same) => Ok(same),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn same_path_identity(left: &Path, right: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let left = fs::metadata(left)?;
    let right = fs::metadata(right)?;
    Ok((left.dev(), left.ino()) == (right.dev(), right.ino()))
}

#[cfg(not(unix))]
fn same_path_identity(left: &Path, right: &Path) -> std::io::Result<bool> {
    same_file::is_same_file(left, right)
}
