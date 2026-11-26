//! 后端专属配置和保留期继承规则。
use super::{defaults::*, RetentionConfig};
use config_contract::Config;
use serde::Serialize;
use std::{
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
};

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct SqliteConfig {
    /// SQLite 数据库文件路径
    pub path: PathBuf,
    /// SQLite 保留期与清理参数
    pub retention: Option<SqliteRetentionConfig>,
}
impl Default for SqliteConfig {
    fn default() -> Self {
        Self {
            path: "captures.sqlite".into(),
            retention: Some(SqliteRetentionConfig::default()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct SqliteRetentionConfig {
    /// 未设置时继承共享值，0 关闭本后端的过期清理
    pub max_age_seconds: Option<u32>,
    /// 容量上限，未设置时继承共享值
    pub max_size_bytes: Option<NonZeroU64>,
    /// 过期清理间隔秒数
    pub cleanup_interval_seconds: Option<NonZeroU32>,
    /// 单批删除记录数
    pub delete_batch_size: Option<NonZeroU32>,
}
impl Default for SqliteRetentionConfig {
    fn default() -> Self {
        Self {
            max_age_seconds: None,
            max_size_bytes: None,
            cleanup_interval_seconds: Some(default_cleanup_seconds()),
            delete_batch_size: Some(default_delete_batch_size()),
        }
    }
}
impl SqliteConfig {
    pub(super) fn resolve(mut self, shared_age: u32, shared_size: NonZeroU64) -> Self {
        let retention = self.retention.get_or_insert_with(Default::default);
        retention.max_age_seconds.get_or_insert(shared_age);
        retention.max_size_bytes.get_or_insert(shared_size);
        self
    }
    pub fn max_size_bytes(&self) -> NonZeroU64 {
        self.retention
            .as_ref()
            .and_then(|settings| settings.max_size_bytes)
            .unwrap_or_else(default_max_size_bytes)
    }
    pub fn retention_policy(&self) -> Option<RetentionConfig> {
        let settings = self.retention.as_ref()?;
        RetentionConfig::resolve(
            settings.max_age_seconds.and_then(NonZeroU32::new),
            settings.cleanup_interval_seconds,
        )
    }
    pub fn delete_batch_size(&self) -> NonZeroU32 {
        self.retention
            .as_ref()
            .and_then(|settings| settings.delete_batch_size)
            .unwrap_or_else(default_delete_batch_size)
    }
}

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct NdjsonConfig {
    /// NDJSON 分段目录
    pub directory: PathBuf,
    /// 时间分段秒数
    pub segment_seconds: NonZeroU32,
    /// 单个分段文件的字节上限
    pub max_segment_bytes: NonZeroU64,
    /// NDJSON 保留期与清理参数
    pub retention: Option<NdjsonRetentionConfig>,
}
impl Default for NdjsonConfig {
    fn default() -> Self {
        Self {
            directory: "captures".into(),
            segment_seconds: default_segment_seconds(),
            max_segment_bytes: default_segment_bytes(),
            retention: Some(NdjsonRetentionConfig::default()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Config)]
#[serde(default)]
pub struct NdjsonRetentionConfig {
    /// 未设置时继承共享值，0 关闭本后端的过期清理
    pub max_age_seconds: Option<u32>,
    /// 容量上限，未设置时继承共享值
    pub max_size_bytes: Option<NonZeroU64>,
    /// 过期清理间隔秒数
    pub cleanup_interval_seconds: Option<NonZeroU32>,
    /// 单次清理的分段数上限，未设置时不限制
    pub max_segments_per_cleanup: Option<NonZeroU32>,
}
impl Default for NdjsonRetentionConfig {
    fn default() -> Self {
        Self {
            max_age_seconds: None,
            max_size_bytes: None,
            cleanup_interval_seconds: Some(default_cleanup_seconds()),
            max_segments_per_cleanup: None,
        }
    }
}
impl NdjsonConfig {
    pub(super) fn resolve(mut self, shared_age: u32, shared_size: NonZeroU64) -> Self {
        let retention = self.retention.get_or_insert_with(Default::default);
        retention.max_age_seconds.get_or_insert(shared_age);
        retention.max_size_bytes.get_or_insert(shared_size);
        self
    }
    pub fn max_size_bytes(&self) -> NonZeroU64 {
        self.retention
            .as_ref()
            .and_then(|settings| settings.max_size_bytes)
            .unwrap_or_else(default_max_size_bytes)
    }
    pub fn retention_policy(&self) -> Option<RetentionConfig> {
        let settings = self.retention.as_ref()?;
        RetentionConfig::resolve(
            settings.max_age_seconds.and_then(NonZeroU32::new),
            settings.cleanup_interval_seconds,
        )
    }
}
