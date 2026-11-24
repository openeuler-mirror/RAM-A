//! 本文件把会话事件汇总为结构复用率，并关联缺陷证据给出的可恢复上界。

use std::collections::HashMap;

use actrail_kv_artifacts::{
    ContextDefect, PrefixReuseComposition, PrefixReuseRequest, PrefixReuseSession,
    PrefixReuseSummary, SessionReport,
};

/// `requests[]` 的体积上限：超出时只保留增益最大的前若干条。
const MAX_PREFIX_REUSE_REQUESTS: usize = 500;

/// 单个请求的前缀上限与限制它的缺陷。
#[derive(Clone, Debug)]
pub(crate) struct RequestPotential {
    /// 限制该请求前缀的缺陷中，被阻断稳定字节的最大值。
    pub blocked_stable_bytes: usize,
    pub defect_ids: Vec<String>,
}

/// 从缺陷证据反推每个受影响请求被阻断的稳定字节。
///
/// 同一请求被多个缺陷限制时取最大值、不做相加；该量是结构代理，不代表真实命中收益。
pub(crate) fn request_potential_by_request(
    defects: &[ContextDefect],
) -> HashMap<String, RequestPotential> {
    let mut potentials: HashMap<String, RequestPotential> = HashMap::new();
    for defect in defects {
        for variant in &defect.mismatch.variants {
            for request_id in &variant.member_request_ids {
                let entry =
                    potentials
                        .entry(request_id.clone())
                        .or_insert_with(|| RequestPotential {
                            blocked_stable_bytes: 0,
                            defect_ids: Vec::new(),
                        });
                entry.blocked_stable_bytes =
                    entry.blocked_stable_bytes.max(defect.blocked_stable_bytes);
                entry.defect_ids.push(defect.id.clone());
            }
        }
    }
    for entry in potentials.values_mut() {
        entry.defect_ids.sort();
        entry.defect_ids.dedup();
    }
    potentials
}

/// 汇总结构复用率：Σ 会话内相邻请求对的公共前缀 / Σ 上下文总字节。
pub(crate) fn summarize_prefix_reuse(
    reports: &[SessionReport],
    potentials: &HashMap<String, RequestPotential>,
) -> Option<PrefixReuseSummary> {
    if reports.is_empty() {
        return None;
    }
    let mut by_session = Vec::with_capacity(reports.len());
    let mut requests: Vec<PrefixReuseRequest> = Vec::new();
    let mut request_pairs = 0usize;
    let mut lcp_bytes = 0usize;
    let mut potential_lcp_bytes = 0usize;
    let mut context_bytes = 0usize;
    let mut recoverable_stable_bytes = 0usize;
    let mut sessions_with_prefix_cut = 0usize;
    for report in reports {
        let mut session_pairs = 0usize;
        let mut session_lcp = 0usize;
        let mut session_potential = 0usize;
        let mut session_context = 0usize;
        for event in &report.events {
            let reusable = event.lcp_bytes;
            let potential = potentials
                .get(&event.next_request_id)
                .map(|entry| {
                    reusable
                        .saturating_add(entry.blocked_stable_bytes)
                        .min(event.next_total_bytes)
                        .max(reusable)
                })
                .unwrap_or(reusable);
            session_pairs += 1;
            session_lcp += reusable;
            session_potential += potential;
            session_context += event.next_total_bytes;
            if potential > reusable {
                requests.push(PrefixReuseRequest {
                    request_id: event.next_request_id.clone(),
                    session_key: report.session_key.clone(),
                    reusable_prefix_bytes: reusable,
                    potential_prefix_bytes: potential,
                    gain_bytes: potential - reusable,
                    blocking_defect_ids: potentials
                        .get(&event.next_request_id)
                        .map(|entry| entry.defect_ids.clone())
                        .unwrap_or_default(),
                });
            }
        }
        if report.total_prefix_cut_bytes > 0 {
            sessions_with_prefix_cut += 1;
        }
        recoverable_stable_bytes += report.total_stable_after_switch_bytes;
        request_pairs += session_pairs;
        lcp_bytes += session_lcp;
        potential_lcp_bytes += session_potential;
        context_bytes += session_context;
        by_session.push(PrefixReuseSession {
            session_key: report.session_key.clone(),
            request_pairs: session_pairs,
            lcp_bytes: session_lcp,
            context_bytes: session_context,
            ratio: reuse_ratio(session_lcp, session_context),
            potential_lcp_bytes: session_potential,
            gain_bytes: session_potential.saturating_sub(session_lcp),
        });
    }
    if request_pairs == 0 {
        return None;
    }
    requests.sort_by(|left, right| {
        right
            .gain_bytes
            .cmp(&left.gain_bytes)
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    requests.truncate(MAX_PREFIX_REUSE_REQUESTS);
    let gain_bytes = potential_lcp_bytes.saturating_sub(lcp_bytes);
    Some(PrefixReuseSummary {
        sessions: reports.len(),
        request_pairs,
        lcp_bytes,
        context_bytes,
        ratio: reuse_ratio(lcp_bytes, context_bytes),
        potential_lcp_bytes,
        potential_ratio: reuse_ratio(potential_lcp_bytes, context_bytes),
        gain_bytes,
        recoverable_stable_bytes,
        sessions_with_prefix_cut,
        by_session,
        requests,
        composition: PrefixReuseComposition {
            context_bytes,
            reusable_bytes: lcp_bytes,
            recoverable_bytes: gain_bytes,
            new_content_bytes: context_bytes.saturating_sub(potential_lcp_bytes),
        },
    })
}

fn reuse_ratio(lcp_bytes: usize, context_bytes: usize) -> f64 {
    if context_bytes == 0 {
        0.0
    } else {
        lcp_bytes as f64 / context_bytes as f64
    }
}
