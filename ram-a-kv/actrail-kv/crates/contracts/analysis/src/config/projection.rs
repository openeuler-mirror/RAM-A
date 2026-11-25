//! ProjectionConfig 的字段、默认值与业务约束。
use anyhow::{ensure, Result};
use config_contract::Config;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct ProjectionConfig {
    /// 每条请求的投影单元数量上限
    pub max_units: usize,
    /// 单个文本投影单元的字节上限
    pub max_text_unit_bytes: usize,
    /// 比较时间窗的秒数
    pub comparison_window_seconds: u64,
}
impl Default for ProjectionConfig {
    fn default() -> Self {
        Self {
            max_units: 512,
            max_text_unit_bytes: 1024 * 1024,
            comparison_window_seconds: 3_600,
        }
    }
}
impl ProjectionConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.max_units > 0, "max_units must be greater than zero");
        ensure!(
            self.max_text_unit_bytes > 0,
            "max_text_unit_bytes must be greater than zero"
        );
        ensure!(
            self.comparison_window_seconds > 0,
            "comparison_window_seconds must be greater than zero"
        );
        Ok(())
    }
}
