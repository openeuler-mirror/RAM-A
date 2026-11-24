//! 会话内 prefix-switch（append/fork/reorder/reset）证据分析。

mod lineage;
mod reuse;

pub(crate) use lineage::{analyze_reports, SessionRow};
pub(crate) use reuse::{request_potential_by_request, summarize_prefix_reuse};
