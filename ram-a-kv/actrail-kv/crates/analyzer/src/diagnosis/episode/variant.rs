//! 本文件从已定位的 X/P2 边界构造成员变体、来源证据与恢复稳定段证据。

use std::collections::BTreeMap;

use actrail_kv_artifacts::{RecoveredStable, SourceLocation, VariantEvidence};

use crate::diagnosis::identity;
use crate::discovery::template::{
    CoordinateBindingState, MemberCoordinateMap, MemberFragmentRef, RequestTemplate,
    TemplateCoordinate, TemplateCoordinateId,
};
use crate::model::projection::{CacheSequence, ContextCollectionKind};

use super::locator::Recovery;
use super::EpisodeVariant;

#[derive(Clone, Debug)]
pub(in crate::diagnosis) struct LocatedPart {
    pub coordinate_id: Option<String>,
    pub content_digest: String,
    pub content: String,
    pub source: SourceLocation,
    pub observable_bytes: usize,
    pub stable_identity: Option<String>,
    pub kind: &'static str,
}

#[derive(Clone, Debug)]
pub(in crate::diagnosis) struct LocatedVariant {
    pub member_id: String,
    pub fingerprint: String,
    pub shape: String,
    pub parts: Vec<LocatedPart>,
    pub text: String,
    pub has_gap_or_unmatched: bool,
    pub actual_prefix_bytes: usize,
}

pub(super) fn build_variant(
    template: &RequestTemplate,
    member: &CacheSequence,
    map: &MemberCoordinateMap,
    start: usize,
    end: usize,
) -> LocatedVariant {
    let indexes = coordinate_indexes(template);
    let mut ordered: Vec<(usize, u8, LocatedPart)> = Vec::new();
    for run in &map.unmatched_runs {
        let right = boundary_index(run.right_coordinate_id.as_ref(), &indexes)
            .unwrap_or(template.coordinates.len());
        let left = boundary_index(run.left_coordinate_id.as_ref(), &indexes);
        if right >= start && right <= end && left.is_none_or(|value| value < end) {
            for fragment in &run.fragments {
                ordered.push((right, 0, fragment_part(member, fragment, None, "unmatched")));
            }
        }
    }
    for index in start..end {
        let coordinate = &template.coordinates[index];
        match &map.bindings[index].state {
            CoordinateBindingState::Present(fragment) => ordered.push((
                index,
                1,
                fragment_part(member, fragment, Some(coordinate), "binding"),
            )),
            CoordinateBindingState::Gap => ordered.push((
                index,
                1,
                LocatedPart {
                    coordinate_id: Some(coordinate.id.0.clone()),
                    content_digest: "<gap>".into(),
                    content: String::new(),
                    source: source_from_coordinate(coordinate),
                    observable_bytes: 0,
                    stable_identity: coordinate.logical_unit.stable_identity.clone(),
                    kind: "gap",
                },
            )),
        }
    }
    ordered.sort_by_key(|entry| (entry.0, entry.1));
    let parts: Vec<_> = ordered.into_iter().map(|(_, _, part)| part).collect();
    let fingerprint = identity::digest(parts.iter().flat_map(|part| {
        [
            part.kind.as_bytes(),
            part.coordinate_id.as_deref().unwrap_or("-").as_bytes(),
            part.content_digest.as_bytes(),
        ]
    }));
    let shape = parts
        .iter()
        .map(|part| {
            format!(
                "{}:{}",
                part.kind,
                part.coordinate_id.as_deref().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>()
        .join("/");
    let text = parts.iter().map(|part| part.content.as_str()).collect();
    let actual_prefix_bytes = map.bindings[..start]
        .iter()
        .filter_map(|binding| match &binding.state {
            CoordinateBindingState::Present(fragment) => Some(fragment.observable_bytes),
            CoordinateBindingState::Gap => None,
        })
        .sum();
    LocatedVariant {
        member_id: member.request_id.clone(),
        fingerprint,
        shape,
        has_gap_or_unmatched: parts.iter().any(|part| part.kind != "binding"),
        parts,
        text,
        actual_prefix_bytes,
    }
}

pub(super) fn episode_variant(
    fingerprint: String,
    mut members: Vec<&LocatedVariant>,
) -> EpisodeVariant {
    members.sort_by(|left, right| left.member_id.cmp(&right.member_id));
    let representative = members[0];
    let mut sources: Vec<_> = representative
        .parts
        .iter()
        .filter(|part| part.kind != "gap")
        .map(|part| part.source.clone())
        .collect();
    sources.sort_by(|left, right| {
        left.json_path
            .cmp(&right.json_path)
            .then_with(|| left.byte_start.cmp(&right.byte_start))
    });
    sources.dedup();
    EpisodeVariant {
        fingerprint,
        member_request_ids: members.iter().map(|item| item.member_id.clone()).collect(),
        representative: VariantEvidence {
            request_id: representative.member_id.clone(),
            sources,
            utf8_bytes: representative
                .parts
                .iter()
                .map(|part| part.observable_bytes)
                .sum(),
            excerpt: (!representative.text.is_empty())
                .then(|| representative.text.chars().take(160).collect()),
        },
    }
}

pub(super) fn recovered_evidence(
    template: &RequestTemplate,
    recovery: &Recovery,
    member_id: &str,
) -> RecoveredStable {
    let map = template
        .member_maps
        .iter()
        .find(|map| map.member_request_id == member_id)
        .expect("recovery supporter has coordinate map");
    let member = member(template, member_id).expect("recovery supporter exists");
    let mut sources = Vec::new();
    let mut excerpt = String::new();
    for index in recovery.start..recovery.end {
        if let CoordinateBindingState::Present(fragment) = &map.bindings[index].state {
            sources.push(source_from_fragment(
                member,
                fragment,
                Some(&template.coordinates[index]),
            ));
            excerpt.push_str(fragment_content(member, fragment));
        }
    }
    RecoveredStable {
        sources,
        utf8_bytes: recovery.supporter_bytes[member_id],
        support_count: recovery.supporter_bytes.len(),
        excerpt: excerpt.chars().take(160).collect(),
    }
}

fn fragment_part(
    member: &CacheSequence,
    fragment: &MemberFragmentRef,
    coordinate: Option<&TemplateCoordinate>,
    kind: &'static str,
) -> LocatedPart {
    let unit = &member.units[fragment.unit_index];
    LocatedPart {
        coordinate_id: coordinate.map(|value| value.id.0.clone()),
        content_digest: fragment.content_digest.clone(),
        content: fragment_content(member, fragment).to_owned(),
        source: source_from_fragment(member, fragment, coordinate),
        observable_bytes: fragment.observable_bytes,
        // 成员自身的工具身份表达实际顺序；模板坐标身份只用于没有成员身份的普通单元。
        stable_identity: unit
            .tool_identity
            .clone()
            .or_else(|| coordinate.and_then(|value| value.logical_unit.stable_identity.clone())),
        kind,
    }
}

fn fragment_content<'a>(member: &'a CacheSequence, fragment: &MemberFragmentRef) -> &'a str {
    member.units[fragment.unit_index]
        .content
        .get(fragment.utf8_bytes.clone())
        .expect("symbolic fragment has a valid UTF-8 range")
}

fn source_from_fragment(
    member: &CacheSequence,
    fragment: &MemberFragmentRef,
    coordinate: Option<&TemplateCoordinate>,
) -> SourceLocation {
    let unit = &member.units[fragment.unit_index];
    SourceLocation {
        json_path: fragment.source.json_path.clone(),
        logical_scope: coordinate.map_or_else(
            || {
                vec![
                    collection_name(&unit.hierarchy.collection).into(),
                    unit.alignment_key.clone(),
                ]
            },
            logical_scope,
        ),
        unit_index: Some(fragment.unit_index),
        byte_start: Some(fragment.utf8_bytes.start),
        byte_end: Some(fragment.utf8_bytes.end),
        role: fragment.source.role.clone(),
    }
}

fn source_from_coordinate(coordinate: &TemplateCoordinate) -> SourceLocation {
    SourceLocation {
        json_path: coordinate.source.json_path.clone(),
        logical_scope: logical_scope(coordinate),
        unit_index: None,
        byte_start: Some(coordinate.medoid_utf8_bytes.start),
        byte_end: Some(coordinate.medoid_utf8_bytes.end),
        role: coordinate.source.role.clone(),
    }
}

fn logical_scope(coordinate: &TemplateCoordinate) -> Vec<String> {
    let mut result = vec![
        collection_name(&coordinate.logical_unit.collection).into(),
        coordinate.logical_unit.alignment_key.clone(),
    ];
    if let Some(role) = &coordinate.logical_unit.role {
        result.push(role.clone());
    }
    if let Some(identity) = &coordinate.logical_unit.stable_identity {
        result.push(identity.clone());
    }
    result
}

fn collection_name(collection: &ContextCollectionKind) -> &'static str {
    match collection {
        ContextCollectionKind::Request => "request",
        ContextCollectionKind::Tools => "tools",
        ContextCollectionKind::Messages => "messages",
        ContextCollectionKind::ContentBlocks => "content_blocks",
    }
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

pub(super) fn member<'a>(
    template: &'a RequestTemplate,
    member_id: &str,
) -> Option<&'a CacheSequence> {
    template
        .members
        .iter()
        .find(|member| member.request_id == member_id)
}
