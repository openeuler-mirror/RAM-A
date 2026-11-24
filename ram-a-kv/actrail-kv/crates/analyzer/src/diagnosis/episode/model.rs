//! 本文件定义 diagnosis 与 ranking 之间不含展示 Cause 的统一 region 数据契约。

use actrail_kv_artifacts::{
    ComparisonGroup, DefectFact, MismatchPattern, OptimizationInsight, RecoveredStable,
    VariantEvidence,
};

#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosisOptions {
    pub min_stable_support: usize,
    pub stable_support_rate: f64,
    pub fixed_variant_max: usize,
    pub min_blocked_bytes: usize,
    pub min_anchor_bytes: usize,
}

impl Default for DiagnosisOptions {
    fn default() -> Self {
        Self {
            min_stable_support: 3,
            stable_support_rate: 0.8,
            fixed_variant_max: 3,
            min_blocked_bytes: 64,
            min_anchor_bytes: 24,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpisodeVariant {
    pub fingerprint: String,
    pub member_request_ids: Vec<String>,
    pub representative: VariantEvidence,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DefectCandidate {
    pub id: String,
    pub comparison_group: ComparisonGroup,
    pub template_id: String,
    pub pattern: MismatchPattern,
    pub variants: Vec<EpisodeVariant>,
    pub facts: Vec<DefectFact>,
    pub recovered_stable: RecoveredStable,
    pub representative_member_id: String,
    pub actual_prefix_bytes: usize,
    pub potential_prefix_bytes: usize,
    pub blocked_stable_bytes: usize,
    pub comparable_count: usize,
    pub affected_count: usize,
    pub confidence: f64,
    pub insights: Vec<OptimizationInsight>,
}
