//! Symbolic builder maps every template coordinate back to every member without copying payload text.

use std::{collections::BTreeMap, ops::Range};

use crate::{
    discovery::{candidate::CandidateCohort, template::UnitAlignment},
    model::projection::{CacheSequence, CacheUnit},
};

use super::identity::{coordinate_base_id, digest, digest_parts, kind_order, logical_unit_key};
use super::{
    CoordinateBinding, CoordinateBindingState, CoordinateKind, MemberCoordinateMap,
    MemberFragmentRef, SymbolicAtom, SymbolicMemberSequence, TemplateCoordinate,
    TemplateCoordinateId, UnmatchedRun,
};
use crate::discovery::template::{StableSpan, TemplateSlot};

type TextMappings = Vec<Option<Vec<Vec<Option<usize>>>>>;

pub(crate) fn build_symbolic_template(
    cohort: &CandidateCohort,
    alignments: &[UnitAlignment],
    text_mappings: &TextMappings,
    stable_spans: &[StableSpan],
    slots: &[TemplateSlot],
) -> (
    Vec<TemplateCoordinate>,
    Vec<MemberCoordinateMap>,
    Vec<SymbolicMemberSequence>,
) {
    let coordinates = build_coordinates(cohort, alignments, stable_spans, slots);
    let mut pairs: Vec<_> = cohort
        .members
        .iter()
        .zip(alignments)
        .enumerate()
        .map(|(member_position, (member, alignment))| {
            let map = build_member_map(
                cohort,
                member_position,
                member,
                alignment,
                text_mappings,
                &coordinates,
            );
            let sequence = build_member_sequence(member, &coordinates, &map);
            (map, sequence)
        })
        .collect();
    pairs.sort_by(|left, right| left.0.member_request_id.cmp(&right.0.member_request_id));
    let (member_maps, symbolic_members) = pairs.into_iter().unzip();
    (coordinates, member_maps, symbolic_members)
}

fn build_coordinates(
    cohort: &CandidateCohort,
    alignments: &[UnitAlignment],
    stable_spans: &[StableSpan],
    slots: &[TemplateSlot],
) -> Vec<TemplateCoordinate> {
    let mut fragments: Vec<_> = stable_spans
        .iter()
        .map(|span| {
            (
                span.unit_index,
                span.medoid_utf8_bytes.clone(),
                CoordinateKind::Stable,
                span.support_count,
                span.support_ratio,
            )
        })
        .chain(slots.iter().map(|slot| {
            (
                slot.unit_index,
                slot.medoid_utf8_bytes.clone(),
                CoordinateKind::Slot,
                slot.support_count,
                slot.support_count as f64 / cohort.members.len().max(1) as f64,
            )
        }))
        .collect();
    for (unit_index, unit) in cohort.medoid.units.iter().enumerate() {
        let mut covered: Vec<_> = fragments
            .iter()
            .filter(|fragment| fragment.0 == unit_index)
            .map(|fragment| fragment.1.clone())
            .collect();
        covered.sort_by_key(|range| (range.start, range.end));
        if covered.is_empty() {
            let support = exact_unit_support(cohort, alignments, unit_index);
            fragments.push((
                unit_index,
                0..unit.content.len(),
                if support == cohort.members.len() {
                    CoordinateKind::Stable
                } else {
                    CoordinateKind::Slot
                },
                support,
                support as f64 / cohort.members.len().max(1) as f64,
            ));
        }
    }
    fragments
        .sort_by_key(|(unit, range, kind, _, _)| (*unit, range.start, kind_order(kind), range.end));
    let slot_identities: BTreeMap<_, _> = fragments
        .iter()
        .filter(|(_, _, kind, _, _)| matches!(kind, CoordinateKind::Slot))
        .map(|(unit_index, range, _, _, _)| {
            let left = fragments
                .iter()
                .filter(|(unit, candidate, kind, _, _)| {
                    unit == unit_index
                        && matches!(kind, CoordinateKind::Stable)
                        && candidate.end <= range.start
                })
                .max_by_key(|(_, candidate, _, _, _)| candidate.end)
                .map(|(_, candidate, _, _, _)| {
                    digest(cohort.medoid.units[*unit_index].content[candidate.clone()].as_bytes())
                });
            let right = fragments
                .iter()
                .filter(|(unit, candidate, kind, _, _)| {
                    unit == unit_index
                        && matches!(kind, CoordinateKind::Stable)
                        && candidate.start >= range.end
                })
                .min_by_key(|(_, candidate, _, _, _)| candidate.start)
                .map(|(_, candidate, _, _, _)| {
                    digest(cohort.medoid.units[*unit_index].content[candidate.clone()].as_bytes())
                });
            (
                (*unit_index, range.start, range.end),
                digest_parts(&[
                    left.as_deref().unwrap_or("start").as_bytes(),
                    right.as_deref().unwrap_or("end").as_bytes(),
                ]),
            )
        })
        .collect();
    let mut seen_ids = BTreeMap::<String, usize>::new();
    fragments
        .into_iter()
        .enumerate()
        .map(
            |(ordinal, (unit_index, range, kind, support_count, support_ratio))| {
                let unit = &cohort.medoid.units[unit_index];
                let content = &unit.content[range.clone()];
                let digest = digest(content.as_bytes());
                let identity_digest = match kind {
                    CoordinateKind::Stable => &digest,
                    CoordinateKind::Slot => slot_identities
                        .get(&(unit_index, range.start, range.end))
                        .expect("every slot has a boundary identity"),
                };
                let base_id = coordinate_base_id(unit, &kind, identity_digest);
                let occurrence = seen_ids.entry(base_id.clone()).or_default();
                let id = if *occurrence == 0 {
                    base_id
                } else {
                    digest_parts(&[base_id.as_bytes(), occurrence.to_string().as_bytes()])
                };
                *occurrence += 1;
                let mut source = unit.source.clone();
                source.utf8_bytes = Some(range.clone());
                TemplateCoordinate {
                    id: TemplateCoordinateId(id),
                    ordinal,
                    kind,
                    logical_unit: logical_unit_key(unit),
                    medoid_unit_index: unit_index,
                    medoid_utf8_bytes: range,
                    source,
                    content_digest: digest,
                    observable_bytes: content.len(),
                    support_count,
                    support_ratio,
                }
            },
        )
        .collect()
}

fn build_member_map(
    cohort: &CandidateCohort,
    member_position: usize,
    member: &CacheSequence,
    alignment: &UnitAlignment,
    text_mappings: &TextMappings,
    coordinates: &[TemplateCoordinate],
) -> MemberCoordinateMap {
    let bindings = coordinates
        .iter()
        .map(|coordinate| CoordinateBinding {
            coordinate_id: coordinate.id.clone(),
            state: fragment_for_coordinate(
                &cohort.medoid,
                member_position,
                member,
                alignment,
                text_mappings,
                coordinate,
            )
            .map_or(CoordinateBindingState::Gap, CoordinateBindingState::Present),
        })
        .collect();
    let unmatched_runs = unmatched_runs(cohort, member, alignment, coordinates);
    MemberCoordinateMap {
        member_request_id: member.request_id.clone(),
        bindings,
        unmatched_runs,
    }
}

fn fragment_for_coordinate(
    medoid: &CacheSequence,
    member_position: usize,
    member: &CacheSequence,
    alignment: &UnitAlignment,
    text_mappings: &TextMappings,
    coordinate: &TemplateCoordinate,
) -> Option<MemberFragmentRef> {
    let member_unit_index = alignment.medoid_to_member[coordinate.medoid_unit_index]?;
    let unit = &member.units[member_unit_index];
    let range = match text_mappings.get(coordinate.medoid_unit_index)? {
        Some(member_mappings) => map_text_range(
            &coordinate.medoid_utf8_bytes,
            &member_mappings[member_position],
            &medoid.units[coordinate.medoid_unit_index].content,
            &unit.content,
            &coordinate.kind,
        )?,
        None => 0..unit.content.len(),
    };
    Some(fragment_ref(member_unit_index, unit, range))
}

fn map_text_range(
    medoid_bytes: &Range<usize>,
    mapping: &[Option<usize>],
    medoid_text: &str,
    member_text: &str,
    kind: &CoordinateKind,
) -> Option<Range<usize>> {
    let medoid_boundaries = char_boundaries(medoid_text);
    let member_boundaries = char_boundaries(member_text);
    let medoid_char_start = medoid_boundaries.binary_search(&medoid_bytes.start).ok()?;
    let medoid_char_end = medoid_boundaries.binary_search(&medoid_bytes.end).ok()?;
    let (start_char, end_char) = match kind {
        CoordinateKind::Stable if medoid_char_start < medoid_char_end => {
            let start = *mapping.get(medoid_char_start)?.as_ref()?;
            let end = mapping
                .get(medoid_char_end - 1)?
                .as_ref()?
                .saturating_add(1);
            let contiguous = (medoid_char_start..medoid_char_end)
                .all(|index| mapping[index] == Some(start + index - medoid_char_start));
            if !contiguous {
                return None;
            }
            (start, end)
        }
        _ => {
            let start = if medoid_char_start == 0 {
                0
            } else {
                mapping
                    .get(medoid_char_start - 1)?
                    .as_ref()?
                    .saturating_add(1)
            };
            let end = if medoid_char_end == mapping.len() {
                member_boundaries.len().saturating_sub(1)
            } else {
                *mapping.get(medoid_char_end)?.as_ref()?
            };
            (start, end)
        }
    };
    (start_char <= end_char && end_char < member_boundaries.len())
        .then(|| member_boundaries[start_char]..member_boundaries[end_char])
}

fn exact_unit_support(
    cohort: &CandidateCohort,
    alignments: &[UnitAlignment],
    medoid_unit_index: usize,
) -> usize {
    alignments
        .iter()
        .zip(&cohort.members)
        .filter(|(alignment, member)| {
            alignment.medoid_to_member[medoid_unit_index].is_some_and(|index| {
                member.units[index].content == cohort.medoid.units[medoid_unit_index].content
            })
        })
        .count()
}

fn unmatched_runs(
    cohort: &CandidateCohort,
    member: &CacheSequence,
    alignment: &UnitAlignment,
    coordinates: &[TemplateCoordinate],
) -> Vec<UnmatchedRun> {
    let mut result = Vec::new();
    for indexes in consecutive_runs(&alignment.unmatched_member_units) {
        let first = indexes[0];
        let last = *indexes.last().expect("run is non-empty");
        let left_medoid = alignment
            .medoid_to_member
            .iter()
            .enumerate()
            .filter_map(|(medoid, mapped)| mapped.filter(|mapped| *mapped < first).map(|_| medoid))
            .next_back();
        let right_medoid = alignment
            .medoid_to_member
            .iter()
            .enumerate()
            .find_map(|(medoid, mapped)| mapped.filter(|mapped| *mapped > last).map(|_| medoid));
        let left_coordinate_id = boundary_coordinate(coordinates, left_medoid, false);
        let right_coordinate_id = boundary_coordinate(coordinates, right_medoid, true);
        let fragments: Vec<_> = indexes
            .iter()
            .map(|index| {
                fragment_ref(
                    *index,
                    &member.units[*index],
                    0..member.units[*index].content.len(),
                )
            })
            .collect();
        let run_id = digest_parts(&[
            cohort.id.as_bytes(),
            member.request_id.as_bytes(),
            first.to_string().as_bytes(),
            last.to_string().as_bytes(),
            fragments
                .iter()
                .map(|fragment| fragment.content_digest.as_str())
                .collect::<Vec<_>>()
                .join(":")
                .as_bytes(),
        ]);
        result.push(UnmatchedRun {
            run_id,
            left_coordinate_id,
            right_coordinate_id,
            fragments,
        });
    }
    result
}

fn build_member_sequence(
    member: &CacheSequence,
    coordinates: &[TemplateCoordinate],
    map: &MemberCoordinateMap,
) -> SymbolicMemberSequence {
    let mut atoms = Vec::new();
    for coordinate in coordinates {
        atoms.extend(
            map.unmatched_runs
                .iter()
                .filter(|run| run.right_coordinate_id.as_ref() == Some(&coordinate.id))
                .cloned()
                .map(SymbolicAtom::UnmatchedRun),
        );
        let binding = map
            .bindings
            .iter()
            .find(|binding| binding.coordinate_id == coordinate.id)
            .expect("every coordinate has one binding");
        atoms.push(match &binding.state {
            CoordinateBindingState::Present(fragment) => match coordinate.kind {
                CoordinateKind::Stable => SymbolicAtom::StableRef {
                    coordinate_id: coordinate.id.clone(),
                    fragment: fragment.clone(),
                },
                CoordinateKind::Slot => SymbolicAtom::SlotBinding {
                    coordinate_id: coordinate.id.clone(),
                    variant_id: fragment.content_digest.clone(),
                    fragment: fragment.clone(),
                },
            },
            CoordinateBindingState::Gap => SymbolicAtom::Gap {
                coordinate_id: coordinate.id.clone(),
            },
        });
    }
    atoms.extend(
        map.unmatched_runs
            .iter()
            .filter(|run| run.right_coordinate_id.is_none())
            .cloned()
            .map(SymbolicAtom::UnmatchedRun),
    );
    SymbolicMemberSequence {
        member_request_id: member.request_id.clone(),
        atoms,
        observable_bytes: member.units.iter().map(CacheUnit::observable_bytes).sum(),
    }
}

fn fragment_ref(unit_index: usize, unit: &CacheUnit, range: Range<usize>) -> MemberFragmentRef {
    let mut source = unit.source.clone();
    source.utf8_bytes = Some(range.clone());
    MemberFragmentRef {
        unit_index,
        observable_bytes: range.len(),
        content_digest: digest(unit.content[range.clone()].as_bytes()),
        utf8_bytes: range,
        source,
    }
}

fn boundary_coordinate(
    coordinates: &[TemplateCoordinate],
    medoid_unit: Option<usize>,
    first: bool,
) -> Option<TemplateCoordinateId> {
    let unit = medoid_unit?;
    let matching = coordinates
        .iter()
        .filter(|coordinate| coordinate.medoid_unit_index == unit);
    if first {
        matching.min_by_key(|coordinate| coordinate.ordinal)
    } else {
        matching.max_by_key(|coordinate| coordinate.ordinal)
    }
    .map(|coordinate| coordinate.id.clone())
}

fn consecutive_runs(indexes: &[usize]) -> Vec<Vec<usize>> {
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for index in indexes {
        if let Some(run) = runs.last_mut() {
            if run.last().is_some_and(|previous| *index == previous + 1) {
                run.push(*index);
                continue;
            }
        }
        runs.push(vec![*index]);
    }
    runs
}

fn char_boundaries(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect()
}
