//! 本模块只对已唯一化的 region 候选做保守评分、确定性全排序和 Top K 前缀选择。

use std::collections::BTreeMap;

use actrail_kv_artifacts::{ContextDefect, MismatchRegion, MismatchVariant, ScoreBreakdown};

use crate::diagnosis::DefectCandidate;

#[derive(Clone, Debug, PartialEq)]
pub struct RankedDefects {
    pub defects: Vec<ContextDefect>,
    pub top_k: Vec<String>,
}

pub fn rank_defects(candidates: Vec<DefectCandidate>, top_k: usize) -> RankedDefects {
    let mut unique = BTreeMap::<String, DefectCandidate>::new();
    for candidate in candidates {
        unique
            .entry(candidate.id.clone())
            .and_modify(|existing| {
                if candidate_order(&candidate, existing).is_lt() {
                    *existing = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    let mut defects: Vec<_> = unique.into_values().map(into_artifact).collect();
    defects.sort_by(|left, right| {
        right
            .score
            .score
            .total_cmp(&left.score.score)
            .then_with(|| right.affected_count.cmp(&left.affected_count))
            .then_with(|| right.blocked_stable_bytes.cmp(&left.blocked_stable_bytes))
            .then_with(|| left.id.cmp(&right.id))
    });
    let top_k = defects
        .iter()
        .take(top_k)
        .map(|defect| defect.id.clone())
        .collect();
    RankedDefects { defects, top_k }
}

fn into_artifact(candidate: DefectCandidate) -> ContextDefect {
    debug_assert_eq!(
        candidate.potential_prefix_bytes,
        candidate.actual_prefix_bytes + candidate.blocked_stable_bytes
    );
    let score = candidate.blocked_stable_bytes as f64
        * candidate.affected_count as f64
        * candidate.confidence;
    ContextDefect {
        id: candidate.id,
        comparison_group: candidate.comparison_group,
        template_id: candidate.template_id,
        mismatch: MismatchRegion {
            pattern: candidate.pattern,
            variants: candidate
                .variants
                .into_iter()
                .map(|variant| MismatchVariant {
                    fingerprint: variant.fingerprint,
                    member_request_ids: variant.member_request_ids,
                    representative: variant.representative,
                })
                .collect(),
            facts: candidate.facts,
        },
        recovered_stable: candidate.recovered_stable,
        actual_prefix_bytes: candidate.actual_prefix_bytes,
        potential_prefix_bytes: candidate.potential_prefix_bytes,
        blocked_stable_bytes: candidate.blocked_stable_bytes,
        comparable_count: candidate.comparable_count,
        affected_count: candidate.affected_count,
        confidence: candidate.confidence,
        score: ScoreBreakdown {
            blocked_stable_bytes: candidate.blocked_stable_bytes,
            affected_count: candidate.affected_count,
            confidence: candidate.confidence,
            score,
        },
        insights: candidate.insights,
    }
}

fn candidate_order(left: &DefectCandidate, right: &DefectCandidate) -> std::cmp::Ordering {
    left.blocked_stable_bytes
        .cmp(&right.blocked_stable_bytes)
        .then_with(|| left.affected_count.cmp(&right.affected_count))
        .then_with(|| left.confidence.total_cmp(&right.confidence))
        .then_with(|| {
            left.representative_member_id
                .cmp(&right.representative_member_id)
        })
}

#[cfg(test)]
#[path = "../../../../tests/unit/analyzer/ranking/tests.rs"]
mod tests;
