//! 本文件定义逐请求 KV 缓存命中率指标。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum CacheMetricBasis {
    /// 来自上游响应 usage 的真实计数。
    Reported,
    /// 上游未返回 usage，按与上一次请求的上下文公共前缀估算。
    Estimated,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KvCacheMetric {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// 会话内序号（无 session 时按输入顺序）。
    pub sequence: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miss_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    /// 命中率 = cached / prompt（0..=1）。
    pub hit_rate: f64,
    /// 相对同一会话上一条请求的命中率变化。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_rate_delta: Option<f64>,
    pub basis: CacheMetricBasis,
    /// 估算口径下与上一条请求的公共前缀字节。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_lcp_bytes: Option<usize>,
    pub payload_bytes: usize,
    /// 首 token 时延（毫秒）；未上报时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    /// 总耗时（wall time，毫秒）；未上报时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u64>,
}
