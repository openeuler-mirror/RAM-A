//! Symbolic API tests verify deterministic coordinate identities under irrelevant sequence growth.

use crate::{
    discovery::{
        candidate::{CandidateCohort, StructureSignature},
        template::{RequestTemplate, TemplateExtractor, TemplateOptions},
    },
    model::projection::{
        CacheSequence, CacheUnit, CacheUnitKind, ComparisonDomain, ContextCollectionKind,
        HierarchyLocation, SourceLocation,
    },
};

fn domain() -> ComparisonDomain {
    ComparisonDomain {
        time_window_key: 0,
        endpoint_key: "chat".into(),
        model: "m".into(),
        model_deployment_key: None,
        context_schema_key: "chat/v1".into(),
        agent_key: None,
        kv_namespace: None,
        dialect: "chat".into(),
        adapter_revision: "1".into(),
    }
}

fn sequence(id: &str, values: &[(&str, &str)]) -> CacheSequence {
    CacheSequence {
        request_id: id.into(),
        domain: domain(),
        units: values
            .iter()
            .enumerate()
            .map(|(index, (key, content))| CacheUnit {
                kind: CacheUnitKind::VisibleText,
                alignment_key: (*key).into(),
                content: (*content).into(),
                source: SourceLocation {
                    json_path: format!("$.messages[{index}].content"),
                    utf8_bytes: Some(0..content.len()),
                    message_index: Some(index),
                    role: Some("system".into()),
                },
                tool_identity: None,
                hierarchy: HierarchyLocation {
                    collection: ContextCollectionKind::Messages,
                    parent_json_path: "$.messages".into(),
                    element_index: Some(index),
                    content_block_index: None,
                },
            })
            .collect(),
        projection_reliability_millis: 1_000,
    }
}

fn extract(medoid: CacheSequence, members: Vec<CacheSequence>) -> RequestTemplate {
    let cohort = CandidateCohort {
        id: "cohort".into(),
        domain: medoid.domain.clone(),
        signature: StructureSignature::of(&medoid),
        medoid,
        members,
        cohesion: 1.0,
    };
    TemplateExtractor::new(TemplateOptions {
        stable_support_ratio: 0.8,
        min_stable_support: 3,
        min_stable_span_bytes: 2,
        max_alignment_cells: 100_000,
        max_total_alignment_cells: 1_000_000,
    })
    .extract(vec![cohort])
    .templates
    .remove(0)
}

#[test]
fn existing_coordinate_identity_survives_a_common_prelude() {
    let bare_members = ["a", "b", "c"].map(|id| sequence(id, &[("core", "shared stable core")]));
    let bare = extract(bare_members[0].clone(), bare_members.to_vec());
    let prefixed_members = ["a", "b", "c"].map(|id| {
        sequence(
            id,
            &[
                ("prelude", "new shared prelude"),
                ("core", "shared stable core"),
            ],
        )
    });
    let prefixed = extract(prefixed_members[0].clone(), prefixed_members.to_vec());
    let bare_core = bare
        .coordinates
        .iter()
        .find(|coordinate| coordinate.logical_unit.alignment_key == "core")
        .unwrap();
    let prefixed_core = prefixed
        .coordinates
        .iter()
        .find(|coordinate| coordinate.logical_unit.alignment_key == "core")
        .unwrap();
    assert_eq!(bare_core.id, prefixed_core.id);
}

#[test]
fn member_maps_and_sequences_are_sorted_by_request_identity() {
    let medoid = sequence("b", &[("core", "stable core")]);
    let template = extract(
        medoid.clone(),
        vec![
            medoid,
            sequence("c", &[("core", "stable core")]),
            sequence("a", &[("core", "stable core")]),
        ],
    );
    let map_ids: Vec<_> = template
        .member_maps
        .iter()
        .map(|map| map.member_request_id.as_str())
        .collect();
    let sequence_ids: Vec<_> = template
        .symbolic_members
        .iter()
        .map(|sequence| sequence.member_request_id.as_str())
        .collect();
    assert_eq!(map_ids, ["a", "b", "c"]);
    assert_eq!(sequence_ids, map_ids);
}
