//! 本文件定义接收器的 TOML 配置模型与默认值。

use anyhow::{bail, Result};
use config_contract::{Config, RootConfig};
use serde::Serialize;

/// 默认监听地址，与历史 CLI 默认一致。
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8080";
/// 默认请求体上限，与协议文档一致。
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Receiver 的 HTTP 配置。
#[derive(Debug, Clone, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct ReceiverConfig {
    #[serde(default = "default_listen")]
    /// HTTP 监听地址
    pub listen: String,
    #[serde(default = "default_max_payload_bytes")]
    /// 单次请求体字节上限
    pub max_payload_bytes: usize,
}

impl RootConfig for ReceiverConfig {}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            max_payload_bytes: default_max_payload_bytes(),
        }
    }
}

impl ReceiverConfig {
    /// 校验监听地址与请求体上限的基本约束。
    pub fn validate(&self) -> Result<()> {
        if self.listen.trim().is_empty() {
            bail!("receiver config listen must not be empty");
        }
        if self.max_payload_bytes == 0 {
            bail!("receiver config max_payload_bytes must be greater than zero");
        }
        Ok(())
    }
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_owned()
}

fn default_max_payload_bytes() -> usize {
    DEFAULT_MAX_PAYLOAD_BYTES
}

pub mod settings;
