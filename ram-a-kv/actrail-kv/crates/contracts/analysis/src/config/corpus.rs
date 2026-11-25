//! CorpusConfig 的字段、默认值与业务约束。
use anyhow::{ensure, Result};
use config_contract::Config;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct CorpusConfig {
    /// 输入记录的字节上限
    pub max_record_bytes: usize,
    /// 载入记录数量上限
    pub max_records: usize,
}
impl Default for CorpusConfig {
    fn default() -> Self {
        Self {
            max_record_bytes: 8 * 1024 * 1024,
            max_records: 1_000_000,
        }
    }
}
impl CorpusConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_record_bytes > 0,
            "max_record_bytes must be greater than zero"
        );
        ensure!(
            self.max_records > 0,
            "max_records must be greater than zero"
        );
        Ok(())
    }
}
