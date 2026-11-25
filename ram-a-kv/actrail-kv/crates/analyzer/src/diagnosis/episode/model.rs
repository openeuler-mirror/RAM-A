//! 本文件定义 diagnosis 与 ranking 之间不含展示 Cause 的统一 region 数据契约。

use actrail_kv_artifacts::{
    ComparisonGroup, DefectFact, MismatchPattern, OptimizationInsight, RecoveredStable,
    VariantEvidence,
};

pub use analysis_contract::config::DiagnosisConfig;

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
