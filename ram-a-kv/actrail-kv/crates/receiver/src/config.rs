//! 本文件定义接收器的 JSON 配置模型与默认值。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// 默认监听地址，与历史 CLI 默认一致。
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8080";
/// 默认请求体上限，与协议文档一致。
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Receiver 的 JSON 配置快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_max_payload_bytes")]
    pub max_payload_bytes: usize,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            max_payload_bytes: default_max_payload_bytes(),
        }
    }
}

impl ReceiverConfig {
    /// 从 JSON 字符串解析并校验配置。
    pub fn from_json_str(input: &str) -> Result<Self> {
        let config: Self = serde_json::from_str(input)
            .with_context(|| "invalid receiver config JSON".to_owned())?;
        config.validate()?;
        Ok(config)
    }

    /// 输出可重新载入的完整 JSON 配置。
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("serialize receiver config")
    }

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

#[cfg(test)]
#[path = "../../../tests/unit/receiver/config.rs"]
mod tests;
