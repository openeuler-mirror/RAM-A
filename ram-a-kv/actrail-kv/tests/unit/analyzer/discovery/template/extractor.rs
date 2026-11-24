//! Template extraction tests cover vocabulary independence, support, Unicode, and determinism.

use crate::{
    discovery::{
        candidate::{CandidateBuilder, CandidateOptions},
        template::{CoordinateBindingState, SymbolicAtom},
    },
    model::projection::{
        CacheSequence, CacheUnit, CacheUnitKind, ComparisonDomain, ContextCollectionKind,
        HierarchyLocation, SourceLocation,
    },
};

use super::*;

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

fn multi_sequence(id: &str, values: &[(&str, &str)]) -> CacheSequence {
    let mut sequence = sequence(id, values.first().map_or("", |value| value.1));
    sequence.units = values
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
                role: Some("user".into()),
            },
            tool_identity: None,
            hierarchy: HierarchyLocation {
                collection: ContextCollectionKind::Messages,
                parent_json_path: "$.messages".into(),
                element_index: Some(index),
                content_block_index: None,
            },
        })
        .collect();
    sequence
}

fn extract_explicit(medoid: CacheSequence, members: Vec<CacheSequence>) -> RequestTemplate {
    let cohort = CandidateCohort {
        id: "explicit-cohort".into(),
        domain: medoid.domain.clone(),
        signature: crate::discovery::candidate::StructureSignature::of(&medoid),
        medoid,
        members,
        cohesion: 0.9,
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

fn extract(texts: &[&str]) -> RequestTemplate {
    let sequences: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| sequence(&i.to_string(), text))
        .collect();
    let cohorts = CandidateBuilder::new(CandidateOptions {
        compatibility_threshold: 0.0,
        max_dynamic_coverage_ratio: 1.0,
        min_members: 3,
        max_candidates_per_request: 10,
    })
    .build(sequences)
    .cohorts;
    TemplateExtractor::new(TemplateOptions {
        stable_support_ratio: 0.8,
        min_stable_support: 3,
        min_stable_span_bytes: 2,
        max_alignment_cells: 10_000,
        max_total_alignment_cells: 100_000,
    })
    .extract(cohorts)
    .templates
    .remove(0)
}

#[test]
fn extracts_dynamic_slot_between_stable_unicode_spans() {
    let template = extract(&["规则甲🌍固定尾部", "规则乙🌍固定尾部", "规则丙🌍固定尾部"]);
    assert!(template
        .stable_spans
        .iter()
        .any(|span| span.content.contains("固定尾部")));
    assert!(!template.slots.is_empty());
    for span in &template.stable_spans {
        let text = &template.members[0].units[span.unit_index].content;
        assert!(text.is_char_boundary(span.medoid_utf8_bytes.start));
        assert!(text.is_char_boundary(span.medoid_utf8_bytes.end));
    }
}

#[test]
fn works_with_unrelated_business_vocabulary() {
    let logistics = extract(&[
        "route A then obey the shared safety manual",
        "route B then obey the shared safety manual",
        "route C then obey the shared safety manual",
    ]);
    let astronomy = extract(&[
        "target Mars then apply the common telescope guide",
        "target Moon then apply the common telescope guide",
        "target Io then apply the common telescope guide",
    ]);
    assert!(!logistics.stable_spans.is_empty());
    assert!(!astronomy.stable_spans.is_empty());
}

#[test]
fn stable_support_threshold_is_inclusive() {
    let template = extract(&[
        "AA stable suffix",
        "AA stable suffix",
        "AA stable suffix",
        "BB stable suffix",
    ]);
    assert!(template
        .stable_spans
        .iter()
        .any(|span| span.content.contains("stable suffix")));
}

#[test]
fn missing_units_count_against_stable_support() {
    let medoid = sequence("a", "shared stable text");
    let domain = medoid.domain.clone();
    let empty = |id: &str| CacheSequence {
        request_id: id.into(),
        domain: domain.clone(),
        units: Vec::new(),
        projection_reliability_millis: 1000,
    };
    let cohort = CandidateCohort {
        id: "cohort".into(),
        domain: medoid.domain.clone(),
        signature: crate::discovery::candidate::StructureSignature::of(&medoid),
        medoid: medoid.clone(),
        members: vec![
            medoid,
            sequence("b", "shared stable text"),
            sequence("c", "shared stable text"),
            empty("d"),
            empty("e"),
        ],
        cohesion: 0.8,
    };
    let result = TemplateExtractor::new(TemplateOptions {
        stable_support_ratio: 0.8,
        min_stable_support: 3,
        min_stable_span_bytes: 2,
        max_alignment_cells: 10_000,
        max_total_alignment_cells: 100_000,
    })
    .extract(vec![cohort]);
    assert!(result.templates[0].stable_spans.is_empty());
}

#[test]
fn complete_template_output_is_input_permutation_invariant() {
    let options = CandidateOptions {
        compatibility_threshold: 0.0,
        max_dynamic_coverage_ratio: 1.0,
        min_members: 3,
        max_candidates_per_request: 10,
    };
    let sequences = vec![
        sequence("stable-a", "tenant red then shared operational handbook"),
        sequence("stable-b", "tenant blue then shared operational handbook"),
        sequence("stable-c", "tenant green then shared operational handbook"),
    ];
    let mut reversed = sequences.clone();
    reversed.reverse();
    let extract = |members| {
        let cohorts = CandidateBuilder::new(options.clone())
            .build(members)
            .cohorts;
        TemplateExtractor::new(TemplateOptions {
            stable_support_ratio: 0.8,
            min_stable_support: 3,
            min_stable_span_bytes: 2,
            max_alignment_cells: 10_000,
            max_total_alignment_cells: 100_000,
        })
        .extract(cohorts)
        .templates
        .remove(0)
    };
    let first = extract(sequences);
    let second = extract(reversed);
    assert_eq!(first.id, second.id);
    let spans = |template: &RequestTemplate| {
        template
            .stable_spans
            .iter()
            .map(|span| {
                (
                    span.unit_index,
                    span.medoid_utf8_bytes.clone(),
                    span.content.clone(),
                    span.support_count,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(spans(&first), spans(&second));
}

#[test]
fn total_text_alignment_budget_is_explicit_and_exact_text_uses_linear_fast_path() {
    let long = "稳定内容".repeat(20_000);
    let sequences = vec![
        sequence("a", &long),
        sequence("b", &long),
        sequence("c", &long),
    ];
    let cohorts = CandidateBuilder::new(CandidateOptions {
        compatibility_threshold: 0.0,
        max_dynamic_coverage_ratio: 1.0,
        min_members: 3,
        max_candidates_per_request: 10,
    })
    .build(sequences)
    .cohorts;
    let exact = TemplateExtractor::new(TemplateOptions {
        stable_support_ratio: 0.8,
        min_stable_support: 3,
        min_stable_span_bytes: 2,
        max_alignment_cells: 10,
        max_total_alignment_cells: 300_000,
    })
    .extract(cohorts);
    assert_eq!(exact.templates.len(), 1);

    let differing = vec![
        sequence("x", &format!("{}x", "a".repeat(500))),
        sequence("y", &format!("{}y", "a".repeat(500))),
        sequence("z", &format!("{}z", "a".repeat(500))),
    ];
    let cohorts = CandidateBuilder::new(CandidateOptions {
        compatibility_threshold: 0.0,
        max_dynamic_coverage_ratio: 1.0,
        min_members: 3,
        max_candidates_per_request: 10,
    })
    .build(differing)
    .cohorts;
    let exhausted = TemplateExtractor::new(TemplateOptions {
        stable_support_ratio: 0.8,
        min_stable_support: 3,
        min_stable_span_bytes: 2,
        max_alignment_cells: 1_000_000,
        max_total_alignment_cells: 1_000,
    })
    .extract(cohorts);
    assert!(exhausted.templates.is_empty());
    assert!(exhausted.skipped[0]
        .reason
        .contains("total alignment cell budget"));
}

#[test]
fn slots_keep_true_slices_and_at_most_sixteen_examples() {
    let sequences = (0..20)
        .map(|index| {
            sequence(
                &format!("r{index:02}"),
                &format!("stable-prefix::{index:02}::stable-suffix"),
            )
        })
        .collect();
    let cohorts = CandidateBuilder::new(CandidateOptions {
        compatibility_threshold: 0.0,
        max_dynamic_coverage_ratio: 1.0,
        min_members: 3,
        max_candidates_per_request: 32,
    })
    .build(sequences)
    .cohorts;
    let template = TemplateExtractor::new(TemplateOptions {
        stable_support_ratio: 0.8,
        min_stable_support: 3,
        min_stable_span_bytes: 4,
        max_alignment_cells: 100_000,
        max_total_alignment_cells: 2_000_000,
    })
    .extract(cohorts)
    .templates
    .remove(0);
    let slot = template
        .slots
        .iter()
        .find(|slot| slot.distinct_variant_count >= 20)
        .expect("dynamic slot");
    assert!(slot.observed_values.len() <= 16);
    assert!(slot.observed_values.iter().all(|value| value.len() <= 2));
}

#[test]
fn symbolic_maps_make_deletions_explicit_gaps() {
    let medoid = multi_sequence("a", &[("a", "alpha"), ("b", "beta"), ("c", "gamma")]);
    let shortened = |id| multi_sequence(id, &[("a", "alpha"), ("c", "gamma")]);
    let template = extract_explicit(medoid.clone(), vec![medoid, shortened("b"), shortened("c")]);
    assert!(template.coordinates.len() >= 3);
    assert!(template
        .member_maps
        .iter()
        .all(|map| { map.bindings.len() == template.coordinates.len() }));
    let deleted = template
        .coordinates
        .iter()
        .find(|coordinate| coordinate.logical_unit.alignment_key == "b")
        .expect("deleted coordinate");
    assert_eq!(deleted.support_count, 1);
    for member_id in ["b", "c"] {
        let map = template
            .member_maps
            .iter()
            .find(|map| map.member_request_id == member_id)
            .unwrap();
        let binding = map
            .bindings
            .iter()
            .find(|binding| binding.coordinate_id == deleted.id)
            .unwrap();
        assert_eq!(binding.state, CoordinateBindingState::Gap);
    }
}

#[test]
fn symbolic_maps_retain_insertions_as_bounded_unmatched_runs() {
    let medoid = multi_sequence("a", &[("a", "alpha"), ("c", "gamma")]);
    let inserted = multi_sequence("b", &[("a", "alpha"), ("b", "inserted"), ("c", "gamma")]);
    let template = extract_explicit(medoid.clone(), vec![medoid.clone(), inserted, medoid]);
    let map = template
        .member_maps
        .iter()
        .find(|map| map.member_request_id == "b")
        .unwrap();
    assert_eq!(map.unmatched_runs.len(), 1);
    let run = &map.unmatched_runs[0];
    assert!(run.left_coordinate_id.is_some());
    assert!(run.right_coordinate_id.is_some());
    assert_eq!(run.fragments.len(), 1);
    assert_eq!(run.fragments[0].observable_bytes, "inserted".len());
}

#[test]
fn symbolic_slot_ranges_are_utf8_safe_and_slice_true_member_values() {
    let template = extract(&["前缀甲🌍固定尾部", "前缀乙🌍固定尾部", "前缀丙🌍固定尾部"]);
    for symbolic in &template.symbolic_members {
        let member = template
            .members
            .iter()
            .find(|member| member.request_id == symbolic.member_request_id)
            .unwrap();
        for atom in &symbolic.atoms {
            let fragment = match atom {
                SymbolicAtom::StableRef { fragment, .. }
                | SymbolicAtom::SlotBinding { fragment, .. } => fragment,
                SymbolicAtom::UnmatchedRun(_) | SymbolicAtom::Gap { .. } => continue,
            };
            let text = &member.units[fragment.unit_index].content;
            assert!(text.is_char_boundary(fragment.utf8_bytes.start));
            assert!(text.is_char_boundary(fragment.utf8_bytes.end));
            assert_eq!(
                fragment.observable_bytes,
                text[fragment.utf8_bytes.clone()].len()
            );
        }
    }
}

#[test]
fn stable_span_requires_the_same_members_to_support_the_whole_contiguous_range() {
    let medoid = sequence("medoid", "AAAABBBB");
    let cohort = CandidateCohort {
        id: "repeated-characters".into(),
        domain: medoid.domain.clone(),
        signature: crate::discovery::candidate::StructureSignature::of(&medoid),
        medoid: medoid.clone(),
        members: vec![
            medoid,
            sequence("a1", "AAAA"),
            sequence("a2", "AAAA"),
            sequence("b1", "BBBB"),
            sequence("b2", "BBBB"),
        ],
        cohesion: 0.7,
    };
    let result = TemplateExtractor::new(TemplateOptions {
        stable_support_ratio: 0.6,
        min_stable_support: 3,
        min_stable_span_bytes: 8,
        max_alignment_cells: 1_000,
        max_total_alignment_cells: 10_000,
    })
    .extract(vec![cohort]);
    assert!(result.templates[0].stable_spans.is_empty());
}
