//! CandidateConfig 的字段、默认值与业务约束。
use anyhow::{ensure, Result};
use config_contract::Config;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct CandidateConfig {
    /// 模板兼容度阈值
    pub compatibility_threshold: f64,
    /// 动态内容覆盖比例上限
    pub max_dynamic_coverage_ratio: f64,
    /// 模板最少成员数
    pub min_members: usize,
    /// 每条请求的候选数量上限
    pub max_candidates_per_request: usize,
}
impl Default for CandidateConfig {
    fn default() -> Self {
        Self {
            compatibility_threshold: 0.68,
            max_dynamic_coverage_ratio: 0.35,
            min_members: 3,
            max_candidates_per_request: 128,
        }
    }
}
impl CandidateConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.compatibility_threshold.is_finite()
                && (0.0..=1.0).contains(&self.compatibility_threshold),
            "compatibility_threshold must be a finite value between zero and one"
        );
        ensure!(
            self.max_dynamic_coverage_ratio.is_finite()
                && (0.0..=1.0).contains(&self.max_dynamic_coverage_ratio),
            "max_dynamic_coverage_ratio must be a finite value between zero and one"
        );
        ensure!(self.min_members >= 2, "min_members must be at least two");
        ensure!(
            self.max_candidates_per_request > 0,
            "max_candidates_per_request must be greater than zero"
        );
        Ok(())
    }
}
