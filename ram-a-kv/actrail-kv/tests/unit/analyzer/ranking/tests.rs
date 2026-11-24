//! 本文件验证 region 去重、保守评分、稳定 tie-break 和 Top K 前缀。

use actrail_kv_artifacts::{ComparisonGroup, MismatchPattern, RecoveredStable, SourceLocation};

use super::*;

fn candidate(id: &str, blocked: usize, affected: usize, confidence: f64) -> DefectCandidate {
    DefectCandidate {
        id: id.into(),
        comparison_group: ComparisonGroup {
            time_window_key: "0".into(),
            endpoint_key: "endpoint".into(),
            model: "model".into(),
            context_schema_key: "schema".into(),
            agent_key: None,
            model_deployment_key: None,
            kv_namespace: None,
        },
        template_id: "template".into(),
        pattern: MismatchPattern::ValueMismatch,
        variants: vec![],
        facts: vec![],
        recovered_stable: RecoveredStable {
            sources: vec![SourceLocation {
                json_path: "$".into(),
                logical_scope: vec![],
                unit_index: None,
                byte_start: None,
                byte_end: None,
                role: None,
            }],
            utf8_bytes: blocked,
            support_count: affected + 1,
            excerpt: "stable".into(),
        },
        representative_member_id: "member".into(),
        actual_prefix_bytes: 10,
        potential_prefix_bytes: 10 + blocked,
        blocked_stable_bytes: blocked,
        comparable_count: affected + 1,
        affected_count: affected,
        confidence,
        insights: vec![],
    }
}

#[test]
fn score_and_top_k_are_a_deterministic_prefix() {
    let input = vec![
        candidate("low", 10, 1, 1.0),
        candidate("high", 30, 2, 1.0),
        candidate("mid", 20, 1, 1.0),
    ];
    let mut reversed = input.clone();
    reversed.reverse();
    let forward = rank_defects(input, 2);
    let backward = rank_defects(reversed, 2);
    assert_eq!(forward, backward);
    assert_eq!(forward.top_k, ["high", "mid"]);
    assert_eq!(forward.defects[0].score.score, 60.0);
}

#[test]
fn duplicate_region_id_is_kept_once_with_conservative_complete_sample() {
    let result = rank_defects(
        vec![
            candidate("same", 100, 2, 1.0),
            candidate("same", 80, 2, 1.0),
        ],
        10,
    );
    assert_eq!(result.defects.len(), 1);
    let defect = &result.defects[0];
    assert_eq!(defect.blocked_stable_bytes, 80);
    assert_eq!(
        defect.potential_prefix_bytes,
        defect.actual_prefix_bytes + defect.blocked_stable_bytes
    );
}
