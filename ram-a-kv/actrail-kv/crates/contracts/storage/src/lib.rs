//! 存储配置契约、类别选择和运行时参数解析。
mod backends;
mod defaults;
mod resolved;

pub use backends::{
    NdjsonConfig, NdjsonConfigOverrides, NdjsonRetentionConfig, NdjsonRetentionConfigOverrides,
    SqliteConfig, SqliteConfigOverrides, SqliteRetentionConfig, SqliteRetentionConfigOverrides,
};
use config_contract::{Config, RootConfig};
pub use resolved::{ResolvedStorageConfig, RetentionConfig};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct StorageConfig {
    /// 存储后端类别，可选 sqlite、ndjson
    pub backend: StorageBackendKind,
    /// 共享保留期
    pub retention: Option<StorageRetentionConfig>,
    /// 各存储后端的参数
    pub backends: StorageBackendConfigs,
}
impl RootConfig for StorageConfig {}
impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackendKind::default(),
            retention: Some(StorageRetentionConfig::default()),
            backends: StorageBackendConfigs::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackendKind {
    #[default]
    Sqlite,
    Ndjson,
}

impl config_contract::ConfigField for StorageBackendKind {
    type Overrides = Self;
}

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct StorageRetentionConfig {
    /// 共享保留秒数，默认 7 天，0 关闭过期清理
    pub max_age_seconds: u32,
    /// 采集数据容量上限，默认 256 MiB，超限清理最旧数据
    pub max_size_bytes: std::num::NonZeroU64,
}

impl Default for StorageRetentionConfig {
    fn default() -> Self {
        Self {
            max_age_seconds: defaults::DEFAULT_MAX_AGE_SECONDS,
            max_size_bytes: defaults::default_max_size_bytes(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct StorageBackendConfigs {
    /// SQLite 存储设置
    pub sqlite: Option<SqliteConfig>,
    /// NDJSON 存储设置
    pub ndjson: Option<NdjsonConfig>,
}
impl Default for StorageBackendConfigs {
    fn default() -> Self {
        Self {
            sqlite: Some(SqliteConfig::default()),
            ndjson: Some(NdjsonConfig::default()),
        }
    }
}

impl StorageConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, StorageConfigError> {
        let path = path.as_ref();
        let config = Self::load(path, &["storage"])
            .map_err(|error| StorageConfigError::InvalidConfig(error.to_string()))?;
        config.resolve(path.parent().unwrap_or_else(|| Path::new(".")))
    }

    pub fn resolve(mut self, directory: &Path) -> Result<Self, StorageConfigError> {
        match self.backend {
            StorageBackendKind::Sqlite => {
                if let Some(config) = &mut self.backends.sqlite {
                    resolved::resolve_path(&mut config.path, directory)?;
                }
            }
            StorageBackendKind::Ndjson => {
                if let Some(config) = &mut self.backends.ndjson {
                    resolved::resolve_path(&mut config.directory, directory)?;
                }
            }
        }
        Ok(self)
    }

    pub fn validate_output(
        &self,
        config_file: &Path,
        output: &Path,
    ) -> Result<(), StorageConfigError> {
        self.clone()
            .into_backend()?
            .validate_output(Some(config_file), output)
    }

    /// 校验内存配置来源的导出目标，不假定存在配置文件。
    pub fn validate_output_path(&self, output: &Path) -> Result<(), StorageConfigError> {
        self.clone().into_backend()?.validate_output(None, output)
    }

    pub fn into_backend(self) -> Result<ResolvedStorageConfig, StorageConfigError> {
        let shared = self.retention.unwrap_or_default();
        let resolved = match self.backend {
            StorageBackendKind::Sqlite => ResolvedStorageConfig::Sqlite(
                self.backends
                    .sqlite
                    .ok_or_else(|| {
                        StorageConfigError::InvalidConfig("missing storage.backends.sqlite".into())
                    })?
                    .resolve(shared.max_age_seconds, shared.max_size_bytes),
            ),
            StorageBackendKind::Ndjson => ResolvedStorageConfig::Ndjson(
                self.backends
                    .ndjson
                    .ok_or_else(|| {
                        StorageConfigError::InvalidConfig("missing storage.backends.ndjson".into())
                    })?
                    .resolve(shared.max_age_seconds, shared.max_size_bytes),
            ),
        };
        Ok(resolved)
    }
}

/// 配置解析、路径保护及保留期检查错误。
#[derive(Debug, thiserror::Error)]
pub enum StorageConfigError {
    #[error("invalid storage configuration: {0}")]
    InvalidConfig(String),
    #[error("record is already outside the configured capture retention period")]
    ExpiredCapture,
    #[error("capture storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
