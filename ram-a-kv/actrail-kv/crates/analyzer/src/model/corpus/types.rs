//! Corpus records preserve complete payloads and make every rejected input explicit.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureComparison {
    pub endpoint_key: String,
    pub agent_key: Option<String>,
    pub model_deployment_key: Option<String>,
    pub kv_namespace: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CorpusRecord {
    pub id: String,
    pub captured_at: Option<String>,
    pub source: Option<String>,
    pub session_key: Option<String>,
    pub comparison: CaptureComparison,
    pub payload: Value,
    /// 采集方上报的响应 usage（prompt/缓存命中/输出 token）。
    pub response_usage: Option<ResponseUsage>,
    pub input_line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseUsage {
    pub prompt_tokens: u32,
    pub cached_tokens: u32,
    pub completion_tokens: u32,
    /// 首 token 时延（毫秒）；未上报时为 `None`（`None` 与真实的 0 必须可区分）。
    pub ttft_ms: Option<u64>,
    /// 总耗时（wall time：请求发出到流结束，毫秒）；未上报时为 `None`。
    pub total_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct AnalysisCorpus {
    pub records: Vec<CorpusRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorpusSkipReason {
    EmptyLine,
    RecordTooLarge { bytes: usize, limit: usize },
    InvalidJson(String),
    RecordNotObject,
    MissingPayload,
    MissingComparison,
    MissingEndpointKey,
    EmptyEndpointKey,
    InvalidComparisonKey { field: String },
    EmptyComparisonKey { field: String },
    InvalidSessionKey,
    EmptySessionKey,
    PayloadNotObject,
    RecordBudgetExceeded { limit: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorpusSkip {
    pub input_line: usize,
    pub reason: CorpusSkipReason,
}

#[derive(Clone, Debug, Default)]
pub struct CorpusLoadResult {
    pub corpus: AnalysisCorpus,
    pub skipped: Vec<CorpusSkip>,
}
