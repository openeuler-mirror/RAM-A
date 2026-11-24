//! 本文件定义 requests.ndjson 中每一行的采集请求格式。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonMetadata {
    pub endpoint_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_deployment_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kv_namespace: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedRequest {
    pub captured_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 可选会话标识，只做会话内时间序分析，不进入 comparison group。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    pub comparison: ComparisonMetadata,
    pub payload: Value,
}

#[cfg(test)]
#[path = "../../../tests/unit/artifacts/captured_request.rs"]
mod tests;
