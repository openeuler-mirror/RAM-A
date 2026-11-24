//! 本文件定义会话结构复用率及其逐会话、逐请求明细。

use serde::{Deserialize, Serialize};

/// 结构复用率汇总：Σ 公共前缀字节 / Σ 上下文总字节。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefixReuseSummary {
    pub sessions: usize,
    pub request_pairs: usize,
    pub lcp_bytes: usize,
    pub context_bytes: usize,
    pub ratio: f64,
    pub potential_lcp_bytes: usize,
    pub potential_ratio: f64,
    pub gain_bytes: usize,
    pub recoverable_stable_bytes: usize,
    pub sessions_with_prefix_cut: usize,
    pub by_session: Vec<PrefixReuseSession>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<PrefixReuseRequest>,
    pub composition: PrefixReuseComposition,
}

/// 上下文字节构成：`reusable + recoverable + new_content == context_bytes`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefixReuseComposition {
    pub context_bytes: usize,
    pub reusable_bytes: usize,
    pub recoverable_bytes: usize,
    pub new_content_bytes: usize,
}

/// 单个会话的结构复用率。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefixReuseSession {
    pub session_key: String,
    pub request_pairs: usize,
    pub lcp_bytes: usize,
    pub context_bytes: usize,
    pub ratio: f64,
    pub potential_lcp_bytes: usize,
    pub gain_bytes: usize,
}

/// 单个请求的前缀上限：受哪些结构问题限制、修掉后能到多少。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefixReuseRequest {
    pub request_id: String,
    pub session_key: String,
    pub reusable_prefix_bytes: usize,
    pub potential_prefix_bytes: usize,
    pub gain_bytes: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocking_defect_ids: Vec<String>,
}
