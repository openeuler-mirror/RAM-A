//! TemplateConfig 的字段、默认值与业务约束。
use anyhow::{ensure, Result};
use config_contract::Config;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct TemplateConfig {
    /// 单次对齐单元格上限
    pub max_alignment_cells: usize,
    /// 总对齐单元格上限
    pub max_total_alignment_cells: usize,
}
impl Default for TemplateConfig {
    fn default() -> Self {
        Self {
            max_alignment_cells: 2_000_000,
            max_total_alignment_cells: 64_000_000,
        }
    }
}
impl TemplateConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_alignment_cells > 0,
            "max_alignment_cells must be greater than zero"
        );
        ensure!(
            self.max_total_alignment_cells > 0,
            "max_total_alignment_cells must be greater than zero"
        );
        Ok(())
    }
}
