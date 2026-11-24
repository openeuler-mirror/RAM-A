//! Template extractor estimates per-character support and emits UTF-8-safe stable spans and slots.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::{
    discovery::candidate::CandidateCohort,
    model::projection::{CacheUnit, CacheUnitKind},
};

use super::{
    symbolic::build_symbolic_template, RequestTemplate, SequenceAligner, StableSpan,
    TemplateExtractionResult, TemplateSkip, TemplateSlot,
};

#[derive(Clone, Debug)]
pub struct TemplateOptions {
    pub stable_support_ratio: f64,
    pub min_stable_support: usize,
    pub min_stable_span_bytes: usize,
    pub max_alignment_cells: usize,
    pub max_total_alignment_cells: usize,
}

impl Default for TemplateOptions {
    fn default() -> Self {
        Self {
            stable_support_ratio: 0.80,
            min_stable_support: 3,
            min_stable_span_bytes: 24,
            max_alignment_cells: 2_000_000,
            max_total_alignment_cells: 64_000_000,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TemplateExtractor {
    options: TemplateOptions,
}

impl TemplateExtractor {
    pub fn new(options: TemplateOptions) -> Self {
        Self { options }
    }

    pub fn extract(&self, cohorts: Vec<CandidateCohort>) -> TemplateExtractionResult {
        let mut result = TemplateExtractionResult::default();
        for cohort in cohorts {
            match self.extract_one(&cohort) {
                Ok(template) => result.templates.push(template),
                Err(reason) => result.skipped.push(TemplateSkip {
                    cohort_id: cohort.id,
                    reason,
                }),
            }
        }
        result
            .templates
            .sort_by(|left, right| left.id.cmp(&right.id));
        result
    }

    fn extract_one(&self, cohort: &CandidateCohort) -> Result<RequestTemplate, String> {
        let aligner = SequenceAligner::new(self.options.max_alignment_cells);
        let mut remaining_cells = self.options.max_total_alignment_cells;
        let alignments = cohort
            .members
            .iter()
            .map(|member| {
                let required =
                    (cohort.medoid.units.len() + 1).saturating_mul(member.units.len() + 1);
                charge_cells(&mut remaining_cells, required)?;
                aligner
                    .align(&cohort.medoid.units, &member.units)
                    .map_err(|error| format!("unit alignment budget exceeded: {error:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut stable_spans = Vec::new();
        let mut slots = Vec::new();
        let mut text_mappings = vec![None; cohort.medoid.units.len()];
        for (unit_index, medoid_unit) in cohort.medoid.units.iter().enumerate() {
            let aligned_units: Vec<_> = cohort
                .members
                .iter()
                .zip(&alignments)
                .map(|(member, alignment)| {
                    alignment.medoid_to_member[unit_index].map(|index| &member.units[index])
                })
                .collect();
            if medoid_unit.kind == CacheUnitKind::VisibleText {
                text_mappings[unit_index] = Some(self.extract_text_unit(
                    unit_index,
                    medoid_unit,
                    &aligned_units,
                    cohort.members.len(),
                    &mut remaining_cells,
                    &mut stable_spans,
                    &mut slots,
                )?);
            } else {
                self.extract_atomic_unit(
                    unit_index,
                    medoid_unit,
                    &aligned_units,
                    cohort.members.len(),
                    &mut stable_spans,
                    &mut slots,
                );
            }
        }
        let reliability = cohort
            .members
            .iter()
            .map(|member| member.projection_reliability())
            .fold(1.0f64, f64::min);
        let (coordinates, member_maps, symbolic_members) =
            build_symbolic_template(cohort, &alignments, &text_mappings, &stable_spans, &slots);
        Ok(RequestTemplate {
            id: template_id(cohort),
            domain: cohort.domain.clone(),
            medoid_request_id: cohort.medoid.request_id.clone(),
            members: cohort.members.clone(),
            stable_spans,
            slots,
            coordinates,
            member_maps,
            symbolic_members,
            cohesion: cohort.cohesion,
            projection_reliability: reliability,
        })
    }

    fn extract_atomic_unit(
        &self,
        unit_index: usize,
        medoid: &CacheUnit,
        aligned: &[Option<&CacheUnit>],
        total_members: usize,
        stable_spans: &mut Vec<StableSpan>,
        slots: &mut Vec<TemplateSlot>,
    ) {
        let support = aligned
            .iter()
            .flatten()
            .filter(|unit| unit.content == medoid.content)
            .count();
        if self.is_stable(support, total_members)
            && medoid.content.len() >= self.options.min_stable_span_bytes
        {
            stable_spans.push(stable_span(
                unit_index,
                medoid,
                0..medoid.content.len(),
                support,
                total_members,
            ));
        } else if aligned
            .iter()
            .flatten()
            .any(|unit| unit.content != medoid.content)
        {
            slots.push(template_slot(
                unit_index,
                medoid,
                0..medoid.content.len(),
                aligned,
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn extract_text_unit(
        &self,
        unit_index: usize,
        medoid: &CacheUnit,
        aligned: &[Option<&CacheUnit>],
        total_members: usize,
        remaining_cells: &mut usize,
        stable_spans: &mut Vec<StableSpan>,
        slots: &mut Vec<TemplateSlot>,
    ) -> Result<Vec<Vec<Option<usize>>>, String> {
        let medoid_chars: Vec<_> = medoid.content.char_indices().collect();
        let mut mappings = Vec::with_capacity(aligned.len());
        for unit in aligned {
            mappings.push(match unit {
                Some(unit) => lcs_medoid_positions(
                    &medoid.content,
                    &unit.content,
                    self.options.max_alignment_cells,
                    remaining_cells,
                )?,
                None => vec![None; medoid_chars.len()],
            });
        }
        let mut accepted = Vec::new();
        let mut cursor = 0;
        while cursor < medoid_chars.len() {
            let mut supporters: BTreeSet<usize> = mappings
                .iter()
                .enumerate()
                .filter_map(|(member, mapping)| mapping[cursor].map(|_| member))
                .collect();
            if !self.is_stable(supporters.len(), total_members) {
                cursor += 1;
                continue;
            }
            let start = cursor;
            cursor += 1;
            while cursor < medoid_chars.len() {
                let mut next_supporters = supporters.clone();
                next_supporters.retain(|member| {
                    matches!(
                        (mappings[*member][cursor - 1], mappings[*member][cursor]),
                        (Some(previous), Some(current)) if current == previous + 1
                    )
                });
                if !self.is_stable(next_supporters.len(), total_members) {
                    break;
                }
                supporters = next_supporters;
                cursor += 1;
            }
            let byte_start = medoid_chars[start].0;
            let byte_end = if cursor < medoid_chars.len() {
                medoid_chars[cursor].0
            } else {
                medoid.content.len()
            };
            if byte_end - byte_start >= self.options.min_stable_span_bytes {
                accepted.push((start..cursor, byte_start..byte_end, supporters.len()));
            } else if cursor > start + 1 {
                cursor = start + 1;
            }
        }
        for (_, range, span_support) in &accepted {
            stable_spans.push(stable_span(
                unit_index,
                medoid,
                range.clone(),
                *span_support,
                total_members,
            ));
        }
        let mut byte_cursor = 0;
        let mut char_cursor = 0;
        for (char_range, byte_range, _) in &accepted {
            if byte_cursor < byte_range.start {
                slots.push(template_slot_text(
                    unit_index,
                    medoid,
                    byte_cursor..byte_range.start,
                    char_cursor..char_range.start,
                    aligned,
                    &mappings,
                ));
            }
            byte_cursor = byte_range.end;
            char_cursor = char_range.end;
        }
        if byte_cursor < medoid.content.len() {
            slots.push(template_slot_text(
                unit_index,
                medoid,
                byte_cursor..medoid.content.len(),
                char_cursor..medoid_chars.len(),
                aligned,
                &mappings,
            ));
        }
        if accepted.is_empty()
            && !aligned
                .iter()
                .flatten()
                .all(|unit| unit.content == medoid.content)
            && medoid.content.is_empty()
        {
            slots.push(template_slot(unit_index, medoid, 0..0, aligned));
        }
        Ok(mappings)
    }

    fn is_stable(&self, support: usize, total: usize) -> bool {
        support >= self.options.min_stable_support
            && (support as f64 / total.max(1) as f64) + f64::EPSILON
                >= self.options.stable_support_ratio
    }
}

fn stable_span(
    unit_index: usize,
    unit: &CacheUnit,
    range: std::ops::Range<usize>,
    support: usize,
    total: usize,
) -> StableSpan {
    StableSpan {
        unit_index,
        content: unit.content[range.clone()].to_owned(),
        medoid_utf8_bytes: range,
        source: unit.source.clone(),
        support_count: support,
        support_ratio: support as f64 / total.max(1) as f64,
    }
}

fn template_slot(
    unit_index: usize,
    medoid: &CacheUnit,
    range: std::ops::Range<usize>,
    aligned: &[Option<&CacheUnit>],
) -> TemplateSlot {
    let values: Vec<_> = aligned
        .iter()
        .flatten()
        .map(|unit| unit.content.clone())
        .collect();
    let support_count = values.len();
    make_slot(unit_index, medoid, range, values, support_count)
}

fn template_slot_text(
    unit_index: usize,
    medoid: &CacheUnit,
    byte_range: std::ops::Range<usize>,
    char_range: std::ops::Range<usize>,
    aligned: &[Option<&CacheUnit>],
    mappings: &[Vec<Option<usize>>],
) -> TemplateSlot {
    let values: Vec<_> = aligned
        .iter()
        .zip(mappings)
        .filter_map(|(unit, mapping)| {
            let unit = (*unit)?;
            let member_bytes: Vec<_> = unit
                .content
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(unit.content.len()))
                .collect();
            let start_char = if char_range.start == 0 {
                0
            } else {
                mapping[char_range.start - 1]?.saturating_add(1)
            };
            let end_char = if char_range.end == mapping.len() {
                member_bytes.len().saturating_sub(1)
            } else {
                mapping[char_range.end]?
            };
            (start_char <= end_char && end_char < member_bytes.len())
                .then(|| unit.content[member_bytes[start_char]..member_bytes[end_char]].to_owned())
        })
        .collect();
    let support_count = values.len();
    make_slot(unit_index, medoid, byte_range, values, support_count)
}

fn make_slot(
    unit_index: usize,
    medoid: &CacheUnit,
    range: std::ops::Range<usize>,
    values: impl IntoIterator<Item = String>,
    support_count: usize,
) -> TemplateSlot {
    const MAX_EXAMPLES: usize = 16;
    let mut variants = BTreeSet::new();
    let mut examples = Vec::new();
    for value in values {
        let digest = Sha256::digest(value.as_bytes());
        if variants.insert(digest.to_vec()) && examples.len() < MAX_EXAMPLES {
            examples.push(value);
        }
    }
    TemplateSlot {
        unit_index,
        medoid_utf8_bytes: range,
        source: medoid.source.clone(),
        observed_values: examples,
        distinct_variant_count: variants.len(),
        support_count,
    }
}

fn lcs_medoid_positions(
    left: &str,
    right: &str,
    max_cells: usize,
    remaining_cells: &mut usize,
) -> Result<Vec<Option<usize>>, String> {
    if left == right {
        let chars = left.chars().count();
        charge_cells(remaining_cells, chars)?;
        return Ok((0..chars).map(Some).collect());
    }
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let required = (left.len() + 1).saturating_mul(right.len() + 1);
    if required > max_cells {
        return Err(format!(
            "text alignment cells {required} exceed {max_cells}"
        ));
    }
    charge_cells(remaining_cells, required)?;
    let mut matched = vec![None; left.len()];
    for (left_index, right_index) in hirschberg_lcs(&left, &right) {
        matched[left_index] = Some(right_index);
    }
    Ok(matched)
}

/// Hirschberg 线性空间 LCS：返回 (left_idx, right_idx) 单调配对，内存 O(n+m)。
fn hirschberg_lcs(left: &[char], right: &[char]) -> Vec<(usize, usize)> {
    if left.is_empty() || right.is_empty() {
        return Vec::new();
    }
    if left.len() == 1 {
        return right
            .iter()
            .position(|candidate| candidate == &left[0])
            .map(|index| vec![(0, index)])
            .unwrap_or_default();
    }
    let mid = left.len() / 2;
    let forward = lcs_lengths(&left[..mid], right);
    let backward = lcs_lengths_rev(&left[mid..], right);
    let mut split = 0usize;
    let mut best = 0usize;
    for index in 0..=right.len() {
        let score = forward[index] + backward[right.len() - index];
        if score > best {
            best = score;
            split = index;
        }
    }
    let mut pairs = hirschberg_lcs(&left[..mid], &right[..split]);
    pairs.extend(
        hirschberg_lcs(&left[mid..], &right[split..])
            .into_iter()
            .map(|(left_index, right_index)| (left_index + mid, right_index + split)),
    );
    pairs
}

fn lcs_lengths(left: &[char], right: &[char]) -> Vec<usize> {
    let mut previous = vec![0usize; right.len() + 1];
    let mut current = vec![0usize; right.len() + 1];
    for left_char in left {
        for (index, right_char) in right.iter().enumerate() {
            current[index + 1] = if left_char == right_char {
                previous[index] + 1
            } else {
                previous[index + 1].max(current[index])
            };
        }
        std::mem::swap(&mut previous, &mut current);
        current[0] = 0;
    }
    previous
}

fn lcs_lengths_rev(left: &[char], right: &[char]) -> Vec<usize> {
    let left_rev: Vec<_> = left.iter().rev().copied().collect();
    let right_rev: Vec<_> = right.iter().rev().copied().collect();
    lcs_lengths(&left_rev, &right_rev)
}

fn charge_cells(remaining: &mut usize, required: usize) -> Result<(), String> {
    if required > *remaining {
        return Err(format!(
            "total alignment cell budget exhausted: required {required}, remaining {remaining}"
        ));
    }
    *remaining -= required;
    Ok(())
}

fn template_id(cohort: &CandidateCohort) -> String {
    let mut digest = Sha256::new();
    digest.update(cohort.id.as_bytes());
    digest.update([0]);
    digest.update(cohort.medoid.request_id.as_bytes());
    hex::encode(digest.finalize())
}

#[cfg(test)]
#[path = "../../../../../tests/unit/analyzer/discovery/template/extractor.rs"]
mod tests;
