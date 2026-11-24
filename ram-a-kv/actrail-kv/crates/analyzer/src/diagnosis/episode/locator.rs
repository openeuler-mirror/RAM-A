//! 本文件直接跨符号模板成员定位唯一 P1/X/P2 region，并按成员一次构造变体直方图。

use std::collections::{BTreeMap, BTreeSet};

use actrail_kv_artifacts::{ComparisonGroup, DefectFact, DefectFactKind};

use crate::diagnosis::{facts, identity};
use crate::discovery::template::{
    CoordinateBindingState, CoordinateKind, MemberCoordinateMap, MemberFragmentRef,
    RequestTemplate, TemplateCoordinate, TemplateCoordinateId,
};
use crate::model::projection::{ComparisonDomain, ContextCollectionKind};

use super::variant::{build_variant, episode_variant, member, recovered_evidence, LocatedVariant};
use super::{DefectCandidate, DiagnosisOptions};

#[derive(Clone, Debug)]
pub(in crate::diagnosis) struct Recovery {
    pub start: usize,
    pub end: usize,
    pub supporter_bytes: BTreeMap<String, usize>,
    pub anchor_chain_digest: String,
    pub local_shape: String,
}

pub fn diagnose_template(
    template: &RequestTemplate,
    options: &DiagnosisOptions,
) -> Vec<DefectCandidate> {
    if template.symbolic_members.len() < options.min_stable_support
        || template.coordinates.is_empty()
    {
        return vec![];
    }
    let maps: BTreeMap<_, _> = template
        .member_maps
        .iter()
        .map(|map| (map.member_request_id.as_str(), map))
        .collect();
    let mut candidates = Vec::new();
    let mut cursor = 0usize;
    let coordinate_count = template.coordinates.len();
    while cursor < coordinate_count {
        let Some((x_start, recovery_search)) = first_mismatch_from(template, &maps, cursor) else {
            break;
        };
        let Some(recovery) = find_recovery(template, &maps, recovery_search.max(cursor), options)
        else {
            cursor += 1;
            continue;
        };
        if recovery.start < x_start {
            cursor = x_start.saturating_add(1);
            continue;
        }
        if let Some(candidate) = build_candidate(template, &maps, x_start, &recovery, options) {
            candidates.push(candidate);
        }
        let next = recovery.end.max(x_start.saturating_add(1));
        if next <= cursor {
            cursor += 1;
        } else {
            cursor = next;
        }
    }
    candidates
}

fn build_candidate(
    template: &RequestTemplate,
    maps: &BTreeMap<&str, &MemberCoordinateMap>,
    x_start: usize,
    recovery: &Recovery,
    options: &DiagnosisOptions,
) -> Option<DefectCandidate> {
    let mut located = Vec::new();
    for member_id in recovery.supporter_bytes.keys() {
        let Some(map) = maps.get(member_id.as_str()) else {
            continue;
        };
        let Some(member) = member(template, member_id) else {
            continue;
        };
        located.push(build_variant(
            template,
            member,
            map,
            x_start,
            recovery.start,
        ));
    }
    located.sort_by(|left, right| left.member_id.cmp(&right.member_id));
    if located.len() < options.min_stable_support {
        return None;
    }

    let mut histogram: BTreeMap<String, Vec<&LocatedVariant>> = BTreeMap::new();
    for variant in &located {
        histogram
            .entry(variant.fingerprint.clone())
            .or_default()
            .push(variant);
    }
    if histogram.len() < 2 {
        return None;
    }
    let largest = histogram
        .iter()
        .max_by(|left, right| {
            left.1
                .len()
                .cmp(&right.1.len())
                .then_with(|| right.0.cmp(left.0))
        })
        .map(|(fingerprint, _)| fingerprint.clone())
        .expect("nonempty histogram");
    let affected_count = located.len() - histogram[&largest].len();
    if affected_count == 0 {
        return None;
    }
    let representative = located
        .iter()
        .filter(|variant| variant.fingerprint != largest)
        .min_by(|left, right| {
            left.actual_prefix_bytes
                .cmp(&right.actual_prefix_bytes)
                .then_with(|| left.member_id.cmp(&right.member_id))
        })
        .expect("an affected variant exists");
    let blocked = located
        .iter()
        .filter(|variant| variant.fingerprint != largest)
        .filter_map(|variant| recovery.supporter_bytes.get(&variant.member_id))
        .copied()
        .min()
        .unwrap_or(0);
    if blocked < options.min_blocked_bytes {
        return None;
    }

    let fact_analysis = facts::analyze(&located, options.fixed_variant_max);
    let logical_shape = logical_shape(template, x_start, recovery.start);
    let id = identity::defect_id(&template.domain, &logical_shape, recovery);
    let mut facts = fact_analysis.facts;
    facts.push(DefectFact {
        kind: DefectFactKind::Scope,
        detail: Some(logical_shape.clone()),
    });
    facts.sort_by(|left, right| format!("{:?}", left.kind).cmp(&format!("{:?}", right.kind)));
    let variants = histogram
        .into_iter()
        .map(|(fingerprint, members)| episode_variant(fingerprint, members))
        .collect();
    let recovered_stable = recovered_evidence(template, recovery, &representative.member_id);
    let confidence = (template.cohesion
        * template.projection_reliability
        * (located.len() as f64 / template.symbolic_members.len() as f64)
        * fact_analysis.certainty)
        .clamp(0.0, 1.0);
    Some(DefectCandidate {
        id,
        comparison_group: comparison_group(&template.domain),
        template_id: template.id.clone(),
        pattern: fact_analysis.pattern,
        variants,
        facts,
        recovered_stable,
        representative_member_id: representative.member_id.clone(),
        actual_prefix_bytes: representative.actual_prefix_bytes,
        potential_prefix_bytes: representative.actual_prefix_bytes + blocked,
        blocked_stable_bytes: blocked,
        comparable_count: located.len(),
        affected_count,
        confidence,
        insights: fact_analysis.insights,
    })
}

fn first_mismatch_from(
    template: &RequestTemplate,
    maps: &BTreeMap<&str, &MemberCoordinateMap>,
    start: usize,
) -> Option<(usize, usize)> {
    let indexes = coordinate_indexes(template);
    for index in start..template.coordinates.len() {
        if maps.values().any(|map| {
            map.unmatched_runs.iter().any(|run| {
                boundary_index(run.right_coordinate_id.as_ref(), &indexes) == Some(index)
            })
        }) {
            return Some((index, index));
        }
        let signatures: BTreeSet<_> = maps
            .values()
            .map(|map| binding_signature(map, index))
            .collect();
        if signatures.len() > 1 || template.coordinates[index].kind == CoordinateKind::Slot {
            return Some((index, index + 1));
        }
    }
    None
}

fn find_recovery(
    template: &RequestTemplate,
    maps: &BTreeMap<&str, &MemberCoordinateMap>,
    search: usize,
    options: &DiagnosisOptions,
) -> Option<Recovery> {
    for start in search..template.coordinates.len() {
        if template.coordinates[start].kind != CoordinateKind::Stable {
            continue;
        }
        let mut supporters: BTreeMap<String, usize> = maps
            .iter()
            .filter_map(|(member, map)| {
                exact_fragment(map, start, &template.coordinates[start])
                    .map(|fragment| ((*member).to_owned(), fragment.observable_bytes))
            })
            .collect();
        for end in start + 1..=template.coordinates.len() {
            if end > start + 1 {
                let coordinate = &template.coordinates[end - 1];
                if coordinate.kind != CoordinateKind::Stable {
                    break;
                }
                supporters.retain(|member, bytes| {
                    let Some(map) = maps.get(member.as_str()) else {
                        return false;
                    };
                    if has_run_between(map, &template.coordinates[end - 2].id, &coordinate.id) {
                        return false;
                    }
                    exact_fragment(map, end - 1, coordinate).is_some_and(|fragment| {
                        *bytes += fragment.observable_bytes;
                        true
                    })
                });
            }
            let min_bytes = supporters.values().copied().min().unwrap_or(0);
            if supported(supporters.len(), template.symbolic_members.len(), options)
                && min_bytes >= options.min_blocked_bytes
                && min_bytes >= options.min_anchor_bytes
            {
                let anchor_chain_digest = identity::digest(
                    template.coordinates[start..]
                        .iter()
                        .filter(|coordinate| coordinate.kind == CoordinateKind::Stable)
                        .map(|coordinate| coordinate.content_digest.as_bytes()),
                );
                let local_shape = template.coordinates[start..end]
                    .iter()
                    .map(coordinate_shape)
                    .collect::<Vec<_>>()
                    .join("/");
                return Some(Recovery {
                    start,
                    end,
                    supporter_bytes: supporters,
                    anchor_chain_digest,
                    local_shape,
                });
            }
        }
    }
    None
}

fn supported(count: usize, total: usize, options: &DiagnosisOptions) -> bool {
    count >= options.min_stable_support
        && count as f64 / total.max(1) as f64 + f64::EPSILON >= options.stable_support_rate
}

fn binding_signature(map: &MemberCoordinateMap, index: usize) -> String {
    match &map.bindings[index].state {
        CoordinateBindingState::Present(fragment) => fragment.content_digest.clone(),
        CoordinateBindingState::Gap => "<gap>".into(),
    }
}

fn exact_fragment<'a>(
    map: &'a MemberCoordinateMap,
    index: usize,
    coordinate: &TemplateCoordinate,
) -> Option<&'a MemberFragmentRef> {
    match &map.bindings.get(index)?.state {
        CoordinateBindingState::Present(fragment)
            if fragment.content_digest == coordinate.content_digest =>
        {
            Some(fragment)
        }
        _ => None,
    }
}

fn has_run_between(
    map: &MemberCoordinateMap,
    left: &TemplateCoordinateId,
    right: &TemplateCoordinateId,
) -> bool {
    map.unmatched_runs.iter().any(|run| {
        run.left_coordinate_id.as_ref() == Some(left)
            && run.right_coordinate_id.as_ref() == Some(right)
    })
}

fn coordinate_indexes(template: &RequestTemplate) -> BTreeMap<&TemplateCoordinateId, usize> {
    template
        .coordinates
        .iter()
        .enumerate()
        .map(|(index, coordinate)| (&coordinate.id, index))
        .collect()
}

fn boundary_index(
    id: Option<&TemplateCoordinateId>,
    indexes: &BTreeMap<&TemplateCoordinateId, usize>,
) -> Option<usize> {
    id.and_then(|id| indexes.get(id).copied())
}

fn collection_name(collection: &ContextCollectionKind) -> &'static str {
    match collection {
        ContextCollectionKind::Request => "request",
        ContextCollectionKind::Tools => "tools",
        ContextCollectionKind::Messages => "messages",
        ContextCollectionKind::ContentBlocks => "content_blocks",
    }
}

fn logical_shape(template: &RequestTemplate, x_start: usize, p2_start: usize) -> String {
    template.coordinates[x_start..p2_start.max(x_start + 1).min(template.coordinates.len())]
        .iter()
        .map(coordinate_shape)
        .collect::<Vec<_>>()
        .join("/")
}

fn coordinate_shape(coordinate: &TemplateCoordinate) -> String {
    format!(
        "{}:{:?}:{}:{}:{}",
        collection_name(&coordinate.logical_unit.collection),
        coordinate.logical_unit.unit_kind,
        coordinate.logical_unit.alignment_key,
        coordinate.logical_unit.role.as_deref().unwrap_or("-"),
        coordinate
            .logical_unit
            .stable_identity
            .as_deref()
            .unwrap_or("-")
    )
}

fn comparison_group(domain: &ComparisonDomain) -> ComparisonGroup {
    ComparisonGroup {
        time_window_key: domain.time_window_key.to_string(),
        endpoint_key: domain.endpoint_key.clone(),
        model: domain.model.clone(),
        context_schema_key: domain.context_schema_key.clone(),
        agent_key: domain.agent_key.clone(),
        model_deployment_key: domain.model_deployment_key.clone(),
        kv_namespace: domain.kv_namespace.clone(),
    }
}
