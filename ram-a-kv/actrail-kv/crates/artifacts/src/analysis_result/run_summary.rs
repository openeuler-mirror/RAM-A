//! 本文件定义分析运行摘要、实际生效参数与跳过记录。

use analysis_contract::AnalyzeConfig;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisRunSummary {
    pub schema_version: String,
    pub input_records: usize,
    pub analyzed_records: usize,
    pub skipped_records: Vec<SkipRecord>,
    pub options: AnalyzeConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkipRecord {
    pub record_index: usize,
    pub reason: String,
}
