//! 本文件定义会话上下文刷新与 prefix-switch 的磁盘协议。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum SessionEventType {
    Append,
    Fork,
    Reorder,
    Reset,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSwitchEvent {
    pub event_type: SessionEventType,
    /// 前一条请求在 session 内的一基序号。
    #[serde(default)]
    pub previous_sequence: usize,
    /// 当前请求在 session 内的一基序号。
    #[serde(default)]
    pub next_sequence: usize,
    pub prev_request_id: String,
    pub next_request_id: String,
    pub lcp_units: usize,
    pub lcp_bytes: usize,
    /// 相邻前一对的公共前缀字节（首事件为 0）。
    pub previous_pair_lcp_bytes: usize,
    /// 前缀收缩字节：previous_pair_lcp 与当前 lcp 的差（无收缩为 0）。
    pub prefix_cut_bytes: usize,
    /// 上一条请求全部内容性上下文单元字节。
    #[serde(default)]
    pub previous_total_bytes: usize,
    /// 内容性单元连续公共前缀字节。
    #[serde(default)]
    pub retained_prefix_bytes: usize,
    /// 内容性单元连续公共前缀字节 / `previous_total_bytes`；上一请求为空时缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_reuse_ratio: Option<f64>,
    /// 上一条内容性上下文未保留的字节数。
    #[serde(default)]
    pub abandoned_prefix_bytes: usize,
    /// 当前相邻请求对是否构成一次上下文刷新。
    #[serde(default)]
    pub prefix_reuse_outlier: bool,
    /// endpoint、model、deployment、schema、agent 或 namespace 是否变化。
    #[serde(default)]
    pub domain_changed: bool,
    pub next_total_bytes: usize,
    pub recomputed_bytes: usize,
    pub stable_after_switch_bytes: usize,
    /// 下一条请求内容性单位的稳定块 run-length 摘要（如 4 个相同块为 x4）。
    pub next_block_runs: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReport {
    pub session_key: String,
    /// 会话第一条有效投影请求的 endpoint。
    pub endpoint_key: String,
    /// 会话第一条有效投影请求的 model。
    pub model: String,
    pub request_count: usize,
    pub append_count: usize,
    pub fork_count: usize,
    pub reorder_count: usize,
    pub reset_count: usize,
    /// 前缀复用离群事件数。
    #[serde(default)]
    pub context_refresh_count: usize,
    pub total_recomputed_bytes: usize,
    pub total_stable_after_switch_bytes: usize,
    pub total_prefix_cut_bytes: usize,
    pub avg_prefix_cut_bytes: usize,
    /// 所有刷新事件废弃前缀字节之和。
    #[serde(default)]
    pub total_abandoned_prefix_bytes: usize,
    /// 比较域发生变化的相邻请求对数量。
    #[serde(default)]
    pub domain_change_count: usize,
    pub events: Vec<SessionSwitchEvent>,
}

#[cfg(test)]
#[path = "../../../../tests/unit/artifacts/analysis_result/session.rs"]
mod tests;
