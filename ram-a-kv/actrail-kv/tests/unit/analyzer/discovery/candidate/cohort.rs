//! 本文件验证 discovery/candidate/cohort 模块的行为。

use super::*;
use crate::model::projection::{
    CacheUnit, CacheUnitKind, ContextCollectionKind, HierarchyLocation, SourceLocation,
};

fn sequence(id: &str, text: &str) -> CacheSequence {
    CacheSequence {
        request_id: id.into(),
        domain: ComparisonDomain {
            time_window_key: 0,
            endpoint_key: "chat".into(),
            dialect: "d".into(),
            model: "m".into(),
            model_deployment_key: None,
            context_schema_key: "chat/v1".into(),
            agent_key: None,
            kv_namespace: None,
            adapter_revision: "1".into(),
        },
        units: vec![CacheUnit {
            kind: CacheUnitKind::VisibleText,
            alignment_key: "message:system:content".into(),
            content: text.into(),
            source: SourceLocation {
                json_path: "$.messages[0].content".into(),
                utf8_bytes: Some(0..text.len()),
                message_index: Some(0),
                role: Some("system".into()),
            },
            tool_identity: None,
            hierarchy: HierarchyLocation {
                collection: ContextCollectionKind::Messages,
                parent_json_path: "$.messages".into(),
                element_index: Some(0),
                content_block_index: None,
            },
        }],
        projection_reliability_millis: 1000,
    }
}

#[test]
fn threshold_is_inclusive_and_input_order_does_not_change_ids() {
    let options = CandidateOptions {
        compatibility_threshold: 0.0,
        max_dynamic_coverage_ratio: 1.0,
        min_members: 3,
        max_candidates_per_request: 10,
    };
    let input = vec![
        sequence("c", "gamma"),
        sequence("a", "alpha"),
        sequence("b", "beta"),
    ];
    let mut reversed = input.clone();
    reversed.reverse();
    let first = CandidateBuilder::new(options.clone()).build(input);
    let second = CandidateBuilder::new(options).build(reversed);
    assert_eq!(first.cohorts[0].id, second.cohorts[0].id);
}

#[test]
fn large_homogeneous_domain_is_not_split_by_seed_budget() {
    let result = CandidateBuilder::new(CandidateOptions {
        compatibility_threshold: 0.0,
        max_dynamic_coverage_ratio: 1.0,
        min_members: 2,
        max_candidates_per_request: 1,
    })
    .build(vec![
        sequence("a", "x"),
        sequence("b", "x"),
        sequence("c", "x"),
    ]);
    assert_eq!(result.cohorts.len(), 1);
    assert_eq!(result.cohorts[0].members.len(), 3);
    assert!(result.skipped.is_empty());
}

#[test]
fn separates_genuinely_different_templates_without_business_rules() {
    let sequences = vec![
        sequence("a1", "Translate every passage faithfully into French."),
        sequence("a2", "Translate every passage faithfully into French."),
        sequence("a3", "Translate every passage faithfully into French."),
        sequence(
            "b1",
            "Audit source code for races and memory safety defects.",
        ),
        sequence(
            "b2",
            "Audit source code for races and memory safety defects.",
        ),
        sequence(
            "b3",
            "Audit source code for races and memory safety defects.",
        ),
    ];
    let result = CandidateBuilder::default().build(sequences);
    assert_eq!(result.cohorts.len(), 2);
    assert!(result
        .cohorts
        .iter()
        .all(|cohort| cohort.members.len() == 3));
}

#[test]
fn boundary_validation_rejects_bridge_members() {
    let common = "shared-observable-prefix-".repeat(4);
    let a = format!("{common}{}", "A".repeat(100));
    let bridge = format!("{common}{}{}", "A".repeat(50), "Z".repeat(50));
    let c = format!("{common}{}", "Z".repeat(100));
    let result = CandidateBuilder::new(CandidateOptions {
        compatibility_threshold: 0.78,
        max_dynamic_coverage_ratio: 0.30,
        min_members: 2,
        max_candidates_per_request: 16,
    })
    .build(vec![
        sequence("a1", &a),
        sequence("a2", &a),
        sequence("bridge", &bridge),
        sequence("c1", &c),
        sequence("c2", &c),
    ]);
    for cohort in &result.cohorts {
        let has_a = cohort
            .members
            .iter()
            .any(|member| member.request_id.starts_with('a'));
        let has_c = cohort
            .members
            .iter()
            .any(|member| member.request_id.starts_with('c'));
        assert!(!(has_a && has_c), "bridge merged incompatible boundaries");
    }
}
