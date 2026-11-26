//! DiagnosisConfig 的字段、默认值与业务约束。
use anyhow::{ensure, Result};
use config_contract::Config;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct DiagnosisConfig {
    /// 固定变体数量上限
    pub fixed_variant_max: usize,
    /// 纳入阻断分析的稳定内容最少字节数
    pub min_blocked_bytes: usize,
}
impl Default for DiagnosisConfig {
    fn default() -> Self {
        Self {
            fixed_variant_max: 3,
            min_blocked_bytes: 64,
        }
    }
}
impl DiagnosisConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.fixed_variant_max > 0,
            "fixed_variant_max must be greater than zero"
        );
        ensure!(
            self.min_blocked_bytes > 0,
            "min_blocked_bytes must be greater than zero"
        );
        Ok(())
    }
}
