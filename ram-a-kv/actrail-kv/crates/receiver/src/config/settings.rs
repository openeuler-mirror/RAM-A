//! Receiver 的文件配置入口与 HTTP 配置组合。
use super::ReceiverConfig;
use config_contract::{Config, RootConfig};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Config)]
#[serde(default)]
pub struct ReceiverSettings {
    #[serde(flatten)]
    pub http: ReceiverConfig,
    /// 单文件 NDJSON 输出位置，未设置时使用 storage
    pub output: Option<PathBuf>,
}
impl RootConfig for ReceiverSettings {}
