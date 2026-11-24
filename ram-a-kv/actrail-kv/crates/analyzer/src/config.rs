//! 本文件把分析引擎的可调阈值与资源预算提升为 JSON 配置模型。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::run::AnalysisOptions;

/// 与 `AnalysisOptions` 一一对应的 JSON 配置快照，字段名与 `run.options` 一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyzeConfig {
    pub top_k: usize,
    pub comparison_window_seconds: u64,
    pub min_template_members: usize,
    pub stable_span_support_ratio: f64,
    pub min_stable_support: usize,
    #[serde(default = "default_fixed_variant_max")]
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
    #[serde(default = "default_prefix_view_max_nodes")]
    pub prefix_view_max_nodes: usize,
    #[serde(default = "default_prefix_view_excerpt_bytes")]
    pub prefix_view_excerpt_bytes: usize,
}

impl Default for AnalyzeConfig {
    fn default() -> Self {
        Self::from_options(&AnalysisOptions::default())
    }
}

impl AnalyzeConfig {
    /// 从引擎默认值生成配置，保证无配置文件时行为不变。
    pub fn from_options(options: &AnalysisOptions) -> Self {
        Self {
            top_k: options.top_k,
            comparison_window_seconds: options.comparison_window_seconds,
            min_template_members: options.min_template_members,
            stable_span_support_ratio: options.stable_span_support_ratio,
            min_stable_support: options.min_stable_support,
            fixed_variant_max: options.fixed_variant_max,
            min_blocked_stable_bytes: options.min_blocked_stable_bytes,
            min_exact_anchor_bytes: options.min_exact_anchor_bytes,
            text_similarity_threshold: options.text_similarity_threshold,
            template_compatibility_threshold: options.template_compatibility_threshold,
            max_dynamic_coverage_ratio: options.max_dynamic_coverage_ratio,
            max_candidates_per_request: options.max_candidates_per_request,
            max_projection_units: options.max_projection_units,
            max_text_unit_bytes: options.max_text_unit_bytes,
            max_payload_bytes: options.max_payload_bytes,
            max_alignment_cells: options.max_alignment_cells,
            max_total_alignment_cells: options.max_total_alignment_cells,
            max_records: options.max_records,
            prefix_view_max_nodes: options.prefix_view_max_nodes,
            prefix_view_excerpt_bytes: options.prefix_view_excerpt_bytes,
        }
    }

    /// 把配置转回引擎参数并执行完整校验。
    pub fn into_options(self) -> Result<AnalysisOptions> {
        let options = AnalysisOptions {
            top_k: self.top_k,
            comparison_window_seconds: self.comparison_window_seconds,
            min_template_members: self.min_template_members,
            stable_span_support_ratio: self.stable_span_support_ratio,
            min_stable_support: self.min_stable_support,
            fixed_variant_max: self.fixed_variant_max,
            min_blocked_stable_bytes: self.min_blocked_stable_bytes,
            min_exact_anchor_bytes: self.min_exact_anchor_bytes,
            text_similarity_threshold: self.text_similarity_threshold,
            template_compatibility_threshold: self.template_compatibility_threshold,
            max_dynamic_coverage_ratio: self.max_dynamic_coverage_ratio,
            max_candidates_per_request: self.max_candidates_per_request,
            max_projection_units: self.max_projection_units,
            max_text_unit_bytes: self.max_text_unit_bytes,
            max_payload_bytes: self.max_payload_bytes,
            max_alignment_cells: self.max_alignment_cells,
            max_total_alignment_cells: self.max_total_alignment_cells,
            max_records: self.max_records,
            prefix_view_max_nodes: self.prefix_view_max_nodes,
            prefix_view_excerpt_bytes: self.prefix_view_excerpt_bytes,
        };
        options.validate()?;
        Ok(options)
    }

    /// 从 JSON 字符串解析并校验配置。
    pub fn from_json_str(input: &str) -> Result<Self> {
        let config: Self = serde_json::from_str(input)
            .with_context(|| "invalid analyze config JSON".to_owned())?;
        config.clone().into_options()?;
        Ok(config)
    }

    /// 输出可重新载入的完整 JSON 配置。
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("serialize analyze config")
    }
}

fn default_fixed_variant_max() -> usize {
    3
}

fn default_prefix_view_max_nodes() -> usize {
    5_000
}

fn default_prefix_view_excerpt_bytes() -> usize {
    96
}

#[cfg(test)]
#[path = "../../../tests/unit/analyzer/config.rs"]
mod tests;
