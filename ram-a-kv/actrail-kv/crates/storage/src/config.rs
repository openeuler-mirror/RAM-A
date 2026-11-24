//! 本文件加载存储后端及其保留期设置，并保护采集位置免受输出覆盖。

use std::{
    ffi::OsString,
    fs,
    num::{NonZeroU32, NonZeroU64},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use time::{Duration, OffsetDateTime};

use crate::StorageError;

#[derive(Clone, Debug, Deserialize)]
#[serde(try_from = "StorageFile")]
pub enum StorageConfig {
    Sqlite(SqliteConfig),
    Ndjson(NdjsonConfig),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageFile {
    storage: StorageSelection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageSelection {
    backend: Backend,
    retention: Option<SharedRetention>,
    backends: BackendConfigs,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedRetention {
    max_age_seconds: NonZeroU32,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Backend {
    Sqlite,
    Ndjson,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendConfigs {
    sqlite: Option<SqliteFile>,
    ndjson: Option<NdjsonFile>,
}

impl TryFrom<StorageFile> for StorageConfig {
    type Error = StorageError;

    fn try_from(file: StorageFile) -> Result<Self, Self::Error> {
        let StorageSelection {
            backend,
            retention,
            backends,
        } = file.storage;
        let default_age = retention.map(|retention| retention.max_age_seconds);
        match backend {
            Backend::Sqlite => backends
                .sqlite
                .map(|file| Self::Sqlite(file.resolve(default_age)))
                .ok_or_else(|| missing_backend("sqlite")),
            Backend::Ndjson => backends
                .ndjson
                .map(|file| Self::Ndjson(file.resolve(default_age)))
                .ok_or_else(|| missing_backend("ndjson")),
        }
    }
}

fn missing_backend(name: &str) -> StorageError {
    StorageError::InvalidConfig(format!(
        "missing storage.backends.{name} for selected backend"
    ))
}

#[derive(Clone, Debug)]
pub struct SqliteConfig {
    pub path: PathBuf,
    pub retention: Option<RetentionConfig>,
    pub delete_batch_size: NonZeroU32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SqliteFile {
    path: PathBuf,
    retention: Option<SqliteRetention>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SqliteRetention {
    max_age_seconds: Option<NonZeroU32>,
    cleanup_interval_seconds: Option<NonZeroU32>,
    delete_batch_size: Option<NonZeroU32>,
}

impl SqliteFile {
    fn resolve(self, default_age: Option<NonZeroU32>) -> SqliteConfig {
        let settings = self.retention.unwrap_or_default();
        SqliteConfig {
            path: self.path,
            retention: RetentionConfig::resolve(
                settings.max_age_seconds.or(default_age),
                settings.cleanup_interval_seconds,
            ),
            delete_batch_size: settings
                .delete_batch_size
                .unwrap_or_else(|| NonZeroU32::new(256).unwrap()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NdjsonConfig {
    pub directory: PathBuf,
    pub segment_seconds: NonZeroU32,
    pub max_segment_bytes: NonZeroU64,
    pub retention: Option<RetentionConfig>,
    pub max_segments_per_cleanup: Option<NonZeroU32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NdjsonFile {
    directory: PathBuf,
    #[serde(default = "default_segment_seconds")]
    segment_seconds: NonZeroU32,
    #[serde(default = "default_segment_bytes")]
    max_segment_bytes: NonZeroU64,
    retention: Option<NdjsonRetention>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NdjsonRetention {
    max_age_seconds: Option<NonZeroU32>,
    cleanup_interval_seconds: Option<NonZeroU32>,
    max_segments_per_cleanup: Option<NonZeroU32>,
}

impl NdjsonFile {
    fn resolve(self, default_age: Option<NonZeroU32>) -> NdjsonConfig {
        let settings = self.retention.unwrap_or_default();
        NdjsonConfig {
            directory: self.directory,
            segment_seconds: self.segment_seconds,
            max_segment_bytes: self.max_segment_bytes,
            retention: RetentionConfig::resolve(
                settings.max_age_seconds.or(default_age),
                settings.cleanup_interval_seconds,
            ),
            max_segments_per_cleanup: settings.max_segments_per_cleanup,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    pub max_age_seconds: NonZeroU32,
    #[serde(default = "default_cleanup_seconds")]
    pub cleanup_interval_seconds: NonZeroU32,
}

impl RetentionConfig {
    fn resolve(max_age: Option<NonZeroU32>, interval: Option<NonZeroU32>) -> Option<Self> {
        max_age.map(|max_age_seconds| Self {
            max_age_seconds,
            cleanup_interval_seconds: interval.unwrap_or_else(default_cleanup_seconds),
        })
    }

    pub(crate) fn cutoff(self) -> OffsetDateTime {
        OffsetDateTime::now_utc() - Duration::seconds(self.max_age_seconds.get().into())
    }

    pub(crate) fn check(self, captured_at: OffsetDateTime) -> Result<(), StorageError> {
        if captured_at < self.cutoff() {
            return Err(StorageError::ExpiredCapture);
        }
        Ok(())
    }
}

impl StorageConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        let raw = fs::read(path)?;
        let mut config: Self = serde_json::from_slice(&raw)?;
        let relative_to = path.parent().unwrap_or_else(|| Path::new("."));
        match &mut config {
            Self::Sqlite(config) => resolve_path(&mut config.path, relative_to)?,
            Self::Ndjson(config) => resolve_path(&mut config.directory, relative_to)?,
        }
        Ok(config)
    }

    pub fn validate_output(&self, config_file: &Path, output: &Path) -> Result<(), StorageError> {
        let output_path = match output.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = output.parent().filter(|path| !path.as_os_str().is_empty());
                let name = output.file_name().ok_or_else(|| {
                    StorageError::InvalidConfig("output needs a file name".into())
                })?;
                parent
                    .unwrap_or_else(|| Path::new("."))
                    .canonicalize()?
                    .join(name)
            }
            Err(error) => return Err(error.into()),
        };
        if same_file_if_exists(config_file, &output_path)? {
            return Err(StorageError::InvalidConfig(
                "output overlaps with the storage configuration".into(),
            ));
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
            return Err(StorageError::InvalidConfig(
                "output overlaps with the capture store".into(),
            ));
        }
        Ok(())
    }
}

fn resolve_path(path: &mut PathBuf, relative_to: &Path) -> Result<(), StorageError> {
    if path.as_os_str().is_empty() {
        return Err(StorageError::InvalidConfig(
            "storage path must not be empty".into(),
        ));
    }
    if path.is_relative() {
        *path = relative_to.join(&*path);
    }
    Ok(())
}

fn same_file_if_exists(left: &Path, right: &Path) -> Result<bool, StorageError> {
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

fn default_segment_seconds() -> NonZeroU32 {
    NonZeroU32::new(3600).unwrap()
}

fn default_segment_bytes() -> NonZeroU64 {
    NonZeroU64::new(64 * 1024 * 1024).unwrap()
}

fn default_cleanup_seconds() -> NonZeroU32 {
    NonZeroU32::new(60).unwrap()
}
