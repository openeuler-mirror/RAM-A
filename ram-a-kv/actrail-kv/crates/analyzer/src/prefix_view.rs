//! 模板前缀树视图模型构建器。
//!
//! 复用模板对齐产出的 coordinates（按 ordinal 完整覆盖 stable/slot 片段），
//! 生成语义级 P/X 节点链；字节级内容只作为稳定节点的 content_sample 提供给
//! report 详情面板，不参与树节点展开。

use std::collections::HashMap;

use actrail_kv_artifacts::{
    ContextDefect as ArtifactContextDefect, PrefixNode as ArtifactPrefixNode,
    PrefixNodeKind as ArtifactKind, PrefixVariant, SourceLocation as ArtifactSource,
    TemplatePrefixView as ArtifactPrefixView,
};

use crate::discovery::template::{CoordinateKind, RequestTemplate};

const CONTENT_SAMPLE_CHARS: usize = 512;

pub(crate) fn build_prefix_view(
    template: &RequestTemplate,
    defects: &[&ArtifactContextDefect],
    max_nodes: usize,
    excerpt_chars: usize,
) -> Option<ArtifactPrefixView> {
    if max_nodes == 0 {
        return None;
    }
    let medoid = template
        .members
        .iter()
        .find(|member| member.request_id == template.medoid_request_id)?;
    let mut nodes: Vec<ArtifactPrefixNode> = Vec::new();
    let mut truncated = false;
    let mut seq_index: Vec<Option<usize>> = Vec::with_capacity(template.coordinates.len());

    for coordinate in &template.coordinates {
        if nodes.len() >= max_nodes {
            truncated = true;
            seq_index.push(None);
            continue;
        }
        let unit = medoid.units.get(coordinate.medoid_unit_index);
        let unit_content = unit.map(|u| u.content.as_str()).unwrap_or_default();
        let content = unit_content
            .get(coordinate.medoid_utf8_bytes.clone())
            .unwrap_or_default();
        let is_dynamic = matches!(coordinate.kind, CoordinateKind::Slot);
        let node_id = format!("seq-{}", coordinate.ordinal);
        let supporting_members = supporting_member_ids(template, &coordinate.id);
        let node = ArtifactPrefixNode {
            id: node_id.clone(),
            kind: if is_dynamic {
                ArtifactKind::Dynamic
            } else {
                ArtifactKind::Stable
            },
            source: artifact_source(
                &coordinate.source,
                coordinate.medoid_unit_index,
                coordinate.medoid_utf8_bytes.clone(),
            ),
            utf8_bytes: content.len(),
            support_count: coordinate.support_count,
            support_ratio: coordinate.support_ratio,
            excerpt: abbreviate(content, excerpt_chars),
            member_request_ids: supporting_members,
            content_sample: if is_dynamic {
                None
            } else {
                Some(abbreviate(content, CONTENT_SAMPLE_CHARS))
            },
            sequence_child: None,
            internal_children: Vec::new(),
            variants: None,
            defect_ids: Vec::new(),
        };
        let seq_index_value = nodes.len();
        nodes.push(node);
        seq_index.push(Some(seq_index_value));

        if is_dynamic {
            if let Some(slot) = find_slot(
                template,
                coordinate.medoid_unit_index,
                &coordinate.medoid_utf8_bytes,
            ) {
                let values: Vec<String> = slot.observed_values.clone();
                let (counts, count_known) = variant_counts(
                    template,
                    coordinate.medoid_unit_index,
                    coordinate.medoid_utf8_bytes.clone(),
                );
                let mut variants: Vec<PrefixVariant> = values
                    .iter()
                    .map(|value| PrefixVariant {
                        excerpt: abbreviate(value, excerpt_chars),
                        count: counts.get(value).copied().unwrap_or(0),
                        count_known,
                    })
                    .collect();
                let missing = template
                    .members
                    .len()
                    .saturating_sub(coordinate.support_count);
                if missing > 0 {
                    variants.push(PrefixVariant {
                        excerpt: "（此位置缺失/无此内容）".to_string(),
                        count: missing,
                        count_known: true,
                    });
                }
                nodes[seq_index_value].variants = Some(variants);
            }
        }
    }

    // 对齐序列层：把片段串成 P → X → P2 链
    for (index, coordinate) in template.coordinates.iter().enumerate() {
        let Some(Some(current)) = seq_index.get(index).copied() else {
            continue;
        };
        if let Some(Some(next)) = seq_index.get(index + 1).copied() {
            nodes[current].sequence_child = Some(nodes[next].id.clone());
        }
        if matches!(coordinate.kind, CoordinateKind::Slot) {
            let range = coordinate.medoid_utf8_bytes.clone();
            let attached = overlapping_defect_ids(defects, &coordinate.source.json_path, &range);
            if !attached.is_empty() {
                nodes[current].defect_ids = attached;
            }
        }
    }

    Some(ArtifactPrefixView {
        node_count: nodes.len(),
        truncated,
        nodes,
    })
}

fn supporting_member_ids(
    template: &RequestTemplate,
    coordinate_id: &crate::discovery::template::TemplateCoordinateId,
) -> Vec<String> {
    let mut ids: Vec<String> = template
        .member_maps
        .iter()
        .filter_map(|member_map| {
            member_map
                .bindings
                .iter()
                .find(|binding| binding.coordinate_id == *coordinate_id)
                .and_then(|binding| match binding.state {
                    crate::discovery::template::CoordinateBindingState::Present(_) => {
                        Some(member_map.member_request_id.clone())
                    }
                    crate::discovery::template::CoordinateBindingState::Gap => None,
                })
        })
        .collect();
    ids.sort();
    ids
}

fn find_slot<'a>(
    template: &'a RequestTemplate,
    unit_index: usize,
    range: &std::ops::Range<usize>,
) -> Option<&'a crate::discovery::template::TemplateSlot> {
    template
        .slots
        .iter()
        .find(|slot| slot.unit_index == unit_index && slot.medoid_utf8_bytes == *range)
        .or_else(|| {
            template.slots.iter().find(|slot| {
                slot.unit_index == unit_index && slot.medoid_utf8_bytes.start == range.start
            })
        })
}

fn variant_counts(
    template: &RequestTemplate,
    unit_index: usize,
    range: std::ops::Range<usize>,
) -> (HashMap<String, usize>, bool) {
    let mut counts = HashMap::new();
    let mut known = true;
    for member in &template.members {
        let Some(unit) = member.units.get(unit_index) else {
            known = false;
            continue;
        };
        let Some(slice) = unit.content.get(range.clone()) else {
            known = false;
            continue;
        };
        *counts.entry(slice.to_string()).or_insert(0) += 1;
    }
    (counts, known)
}

fn overlapping_defect_ids(
    defects: &[&ArtifactContextDefect],
    json_path: &str,
    range: &std::ops::Range<usize>,
) -> Vec<String> {
    let mut ids = Vec::new();
    for defect in defects {
        let hit = defect.mismatch.variants.iter().any(|variant| {
            variant.representative.sources.iter().any(|source| {
                source.json_path == json_path
                    && ranges_overlap(source.byte_start, source.byte_end, range.start, range.end)
            })
        });
        if hit {
            ids.push(defect.id.clone());
        }
    }
    ids
}

fn ranges_overlap(
    left_start: Option<usize>,
    left_end: Option<usize>,
    right_start: usize,
    right_end: usize,
) -> bool {
    match (left_start, left_end) {
        (Some(start), Some(end)) => start < right_end && right_start < end,
        _ => false,
    }
}

fn artifact_source(
    source: &crate::model::projection::SourceLocation,
    unit_index: usize,
    byte_range: std::ops::Range<usize>,
) -> ArtifactSource {
    let role = source.role.clone();
    ArtifactSource {
        json_path: source.json_path.clone(),
        logical_scope: role.iter().cloned().collect(),
        unit_index: Some(unit_index),
        byte_start: Some(byte_range.start),
        byte_end: Some(byte_range.end),
        role,
    }
}

fn abbreviate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
