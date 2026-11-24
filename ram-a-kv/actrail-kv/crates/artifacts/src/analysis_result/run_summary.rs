//! 本文件定义分析运行摘要、实际生效参数与跳过记录。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisRunSummary {
    pub schema_version: String,
    pub input_records: usize,
    pub analyzed_records: usize,
    pub skipped_records: Vec<SkipRecord>,
    pub options: AnalysisOptionsSnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisOptionsSnapshot {
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
    /// 模板前缀树视图模型的最大节点数；0 = 关闭 prefix_view。
    #[serde(default = "default_prefix_view_max_nodes")]
    pub prefix_view_max_nodes: usize,
    /// prefix_view 节点文本摘录的最大字符数。
    #[serde(default = "default_prefix_view_excerpt_bytes")]
    pub prefix_view_excerpt_bytes: usize,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkipRecord {
    pub record_index: usize,
    pub reason: String,
}
