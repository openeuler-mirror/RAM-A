//! 本文件定义所有算法阈值和资源预算，并集中执行边界校验。

use anyhow::{bail, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisOptions {
    pub top_k: usize,
    pub comparison_window_seconds: u64,
    pub min_template_members: usize,
    pub stable_span_support_ratio: f64,
    pub min_stable_support: usize,
    pub fixed_variant_max: usize,
    pub min_blocked_stable_bytes: usize,
    pub min_exact_anchor_bytes: usize,
    pub text_similarity_threshold: f64,
    pub template_compatibility_threshold: f64,
    pub max_dynamic_coverage_ratio: f64,
    pub max_candidates_per_request: usize,
    pub max_projection_units: usize,
    pub max_text_unit_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_alignment_cells: usize,
    pub max_total_alignment_cells: usize,
    pub max_records: usize,
    /// 模板前缀树视图模型的最大节点数；0 = 关闭 prefix_view 输出。
    pub prefix_view_max_nodes: usize,
    /// prefix_view 节点文本摘录的最大字符数。
    pub prefix_view_excerpt_bytes: usize,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            top_k: 20,
            comparison_window_seconds: 3_600,
            min_template_members: 3,
            stable_span_support_ratio: 0.80,
            min_stable_support: 3,
            fixed_variant_max: 3,
            min_blocked_stable_bytes: 64,
            min_exact_anchor_bytes: 24,
            text_similarity_threshold: 0.80,
            template_compatibility_threshold: 0.68,
            max_dynamic_coverage_ratio: 0.35,
            max_candidates_per_request: 128,
            max_projection_units: 512,
            max_text_unit_bytes: 1_048_576,
            max_payload_bytes: 8_388_608,
            max_alignment_cells: 2_000_000,
            max_total_alignment_cells: 64_000_000,
            max_records: 1_000_000,
            prefix_view_max_nodes: 5_000,
            prefix_view_excerpt_bytes: 96,
        }
    }
}

impl AnalysisOptions {
    pub fn validate(&self) -> Result<()> {
        if self.top_k == 0 {
            bail!("top_k must be greater than zero");
        }
        if self.comparison_window_seconds == 0 {
            bail!("comparison_window_seconds must be greater than zero");
        }
        if self.min_template_members < 2 || self.min_stable_support < 2 {
            bail!("template and stable support minima must be at least two");
        }
        if self.prefix_view_excerpt_bytes == 0 {
            bail!("prefix_view_excerpt_bytes must be greater than zero");
        }
        for (name, value) in [
            ("stable_span_support_ratio", self.stable_span_support_ratio),
            ("text_similarity_threshold", self.text_similarity_threshold),
            (
                "template_compatibility_threshold",
                self.template_compatibility_threshold,
            ),
            (
                "max_dynamic_coverage_ratio",
                self.max_dynamic_coverage_ratio,
            ),
        ] {
            if !(0.0..=1.0).contains(&value) || !value.is_finite() {
                bail!("{name} must be a finite value between zero and one");
            }
        }
        for (name, value) in [
            ("fixed_variant_max", self.fixed_variant_max),
            ("min_blocked_stable_bytes", self.min_blocked_stable_bytes),
            ("min_exact_anchor_bytes", self.min_exact_anchor_bytes),
            (
                "max_candidates_per_request",
                self.max_candidates_per_request,
            ),
            ("max_projection_units", self.max_projection_units),
            ("max_text_unit_bytes", self.max_text_unit_bytes),
            ("max_payload_bytes", self.max_payload_bytes),
            ("max_alignment_cells", self.max_alignment_cells),
            ("max_total_alignment_cells", self.max_total_alignment_cells),
            ("max_records", self.max_records),
        ] {
            if value == 0 {
                bail!("{name} must be greater than zero");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../tests/unit/analyzer/run/options.rs"]
mod tests;
