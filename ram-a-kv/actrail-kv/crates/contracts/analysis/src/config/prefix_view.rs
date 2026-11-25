//! PrefixViewConfig 的字段、默认值与业务约束。
use anyhow::{ensure, Result};
use config_contract::Config;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct PrefixViewConfig {
    /// 前缀树视图节点上限，0 关闭视图
    pub max_nodes: usize,
    /// 前缀树节点文本摘录上限
    pub excerpt_bytes: usize,
}
impl Default for PrefixViewConfig {
    fn default() -> Self {
        Self {
            max_nodes: 5_000,
            excerpt_bytes: 96,
        }
    }
}
impl PrefixViewConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.excerpt_bytes > 0,
            "excerpt_bytes must be greater than zero"
        );
        Ok(())
    }
}
