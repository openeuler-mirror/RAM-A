//! StabilityConfig 的字段、默认值与业务约束。
use anyhow::{ensure, Result};
use config_contract::Config;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct StabilityConfig {
    /// 稳定片段最少支持记录数
    pub min_stable_support: usize,
    /// 稳定片段支持率
    pub stable_support_ratio: f64,
    /// 精确匹配锚点最少字节数
    pub min_anchor_bytes: usize,
}
impl Default for StabilityConfig {
    fn default() -> Self {
        Self {
            min_stable_support: 3,
            stable_support_ratio: 0.80,
            min_anchor_bytes: 24,
        }
    }
}
impl StabilityConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.min_stable_support >= 2,
            "min_stable_support must be at least two"
        );
        ensure!(
            self.stable_support_ratio.is_finite()
                && (0.0..=1.0).contains(&self.stable_support_ratio),
            "stable_support_ratio must be a finite value between zero and one"
        );
        ensure!(
            self.min_anchor_bytes > 0,
            "min_anchor_bytes must be greater than zero"
        );
        Ok(())
    }
}
