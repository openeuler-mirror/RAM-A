//! 本文件把模板前缀树布局、SVG 坐标和节点字段转换为展示模型。

use std::collections::{BTreeSet, HashMap};

use anyhow::{bail, Result};

use actrail_kv_artifacts::{
    AnalysisResult, PrefixNode, PrefixNodeKind, RequestTemplate, TemplatePrefixView,
};

use super::super::format::excerpt_label;
use crate::locale::{namespace, LocaleSnapshot};

mod chart;

use chart::{build_decay, DecayView};

pub(crate) struct PrefixTreeView {
    pub(crate) title: String,
    pub(crate) stable_legend: String,
    pub(crate) dynamic_legend: String,
    pub(crate) defect_legend: String,
    pub(crate) support_legend: String,
    pub(crate) overview: String,
    pub(crate) show_toc: bool,
    pub(crate) toc: Vec<TemplateSummary>,
    pub(crate) templates: Vec<TemplateView>,
    pub(crate) template_header: String,
    pub(crate) members_header: String,
    pub(crate) stable_bytes_header: String,
    pub(crate) dynamic_nodes_header: String,
    pub(crate) defect_nodes_header: String,
    pub(crate) full_id_label: String,
    pub(crate) distribution_title: String,
    pub(crate) stable_region_label: String,
    pub(crate) dynamic_region_label: String,
    pub(crate) decay_title: String,
    pub(crate) decay_note: String,
    pub(crate) structure_title: String,
    pub(crate) warning_badge: String,
    pub(crate) source_label: String,
    pub(crate) role_label: String,
    pub(crate) position_label: String,
    pub(crate) unit_label: String,
    pub(crate) support_label: String,
    pub(crate) summary_label: String,
    pub(crate) variants_label: String,
    pub(crate) associated_defects_label: String,
    pub(crate) content_sample_label: String,
    pub(crate) member_label: String,
    pub(crate) alive_label: String,
    pub(crate) lost_label: String,
    pub(crate) member_separator: String,
}

pub(crate) struct TemplateSummary {
    pub(crate) id: String,
    pub(crate) short_id: String,
    pub(crate) members: usize,
    pub(crate) stable_bytes: String,
    pub(crate) dynamic_nodes: usize,
    pub(crate) defect_nodes: usize,
}

pub(crate) struct TemplateView {
    pub(crate) anchor: String,
    pub(crate) short_id: String,
    pub(crate) id: String,
    pub(crate) members_summary: String,
    pub(crate) stats: Vec<StatView>,
    pub(crate) stable_width: String,
    pub(crate) dynamic_width: String,
    pub(crate) stable_bytes: usize,
    pub(crate) stable_percent: String,
    pub(crate) dynamic_bytes: usize,
    pub(crate) dynamic_percent: String,
    pub(crate) first_prefix: String,
    pub(crate) first_divergence: Option<String>,
    pub(crate) decay: Option<DecayView>,
    pub(crate) nodes: Vec<NodeView>,
    pub(crate) truncated: Option<String>,
}

pub(crate) struct StatView {
    pub(crate) label: String,
    pub(crate) value: String,
}

pub(crate) struct NodeView {
    pub(crate) class: &'static str,
    pub(crate) badge_class: &'static str,
    pub(crate) kind_label: &'static str,
    pub(crate) index: usize,
    pub(crate) open: bool,
    pub(crate) excerpt: String,
    pub(crate) bytes: usize,
    pub(crate) percent: String,
    pub(crate) support: usize,
    pub(crate) members: usize,
    pub(crate) warning: bool,
    pub(crate) source_path: String,
    pub(crate) role: Option<String>,
    pub(crate) position: String,
    pub(crate) unit: Option<usize>,
    pub(crate) support_value: String,
    pub(crate) summary: String,
    pub(crate) variants: Vec<VariantView>,
    pub(crate) defect_ids: Vec<String>,
    pub(crate) content_sample: Option<String>,
    pub(crate) show_members: bool,
    pub(crate) alive_count: usize,
    pub(crate) all_count: usize,
    pub(crate) alive_members: Vec<String>,
    pub(crate) alive_truncated: bool,
    pub(crate) lost_members: Vec<String>,
    pub(crate) lost_truncated: bool,
}

pub(crate) struct VariantView {
    pub(crate) class: &'static str,
    pub(crate) label: String,
    pub(crate) count: String,
    pub(crate) zero_label: Option<String>,
}

pub(crate) fn build(
    result: &AnalysisResult,
    locale: LocaleSnapshot,
) -> Result<Option<PrefixTreeView>> {
    let templates_with_view = result
        .templates
        .iter()
        .filter_map(|template| template.prefix_view.as_ref().map(|view| (template, view)))
        .collect::<Vec<_>>();
    if templates_with_view.is_empty() {
        return Ok(None);
    }
    let l = locale.create(namespace::REPORT_PREFIX_TREE)?;
    let toc = if templates_with_view.len() > 1 {
        templates_with_view
            .iter()
            .map(|(template, view)| build_summary(template, view))
            .collect()
    } else {
        Vec::new()
    };
    Ok(Some(PrefixTreeView {
        title: l.get("section_title")?.to_string(),
        stable_legend: l.get("legend_stable")?.to_string(),
        dynamic_legend: l.get("legend_dynamic")?.to_string(),
        defect_legend: l.get("legend_defect")?.to_string(),
        support_legend: l.get("legend_support")?.to_string(),
        overview: l.get("overview")?.to_string(),
        show_toc: templates_with_view.len() > 1,
        toc,
        templates: templates_with_view
            .into_iter()
            .map(|(template, view)| build_template(template, view, locale))
            .collect::<Result<Vec<_>>>()?,
        template_header: l.get("template_header")?.to_string(),
        members_header: l.get("members_header")?.to_string(),
        stable_bytes_header: l.get("stable_bytes_header")?.to_string(),
        dynamic_nodes_header: l.get("dynamic_nodes_header")?.to_string(),
        defect_nodes_header: l.get("defect_nodes_header")?.to_string(),
        full_id_label: l.get("full_id")?.to_string(),
        distribution_title: l.get("distribution_title")?.to_string(),
        stable_region_label: l.get("stable_region")?.to_string(),
        dynamic_region_label: l.get("dynamic_region")?.to_string(),
        decay_title: l.get("decay_title")?.to_string(),
        decay_note: l.get("decay_note")?.to_string(),
        structure_title: l.get("structure_title")?.to_string(),
        warning_badge: l.get("warning_badge")?.to_string(),
        source_label: l.get("source")?.to_string(),
        role_label: l.get("role")?.to_string(),
        position_label: l.get("position")?.to_string(),
        unit_label: l.get("unit")?.to_string(),
        support_label: l.get("support")?.to_string(),
        summary_label: l.get("summary")?.to_string(),
        variants_label: l.get("variants")?.to_string(),
        associated_defects_label: l.get("associated_defects")?.to_string(),
        content_sample_label: l.get("content_sample")?.to_string(),
        member_label: l.get("members_header")?.to_string(),
        alive_label: l.get("alive_label")?.to_string(),
        lost_label: l.get("lost_label")?.to_string(),
        member_separator: l.get("list_separator")?.to_string(),
    }))
}

fn build_summary(template: &RequestTemplate, view: &TemplatePrefixView) -> TemplateSummary {
    TemplateSummary {
        id: template.id.clone(),
        short_id: template.id.chars().take(16).collect(),
        members: template.member_request_ids.len(),
        stable_bytes: format!(
            "{} B",
            view.nodes
                .iter()
                .filter(|node| node.kind == PrefixNodeKind::Stable)
                .map(|node| node.utf8_bytes)
                .sum::<usize>()
        ),
        dynamic_nodes: view
            .nodes
            .iter()
            .filter(|node| node.kind == PrefixNodeKind::Dynamic)
            .count(),
        defect_nodes: view
            .nodes
            .iter()
            .filter(|node| node.kind == PrefixNodeKind::Dynamic && !node.defect_ids.is_empty())
            .count(),
    }
}

fn build_template(
    template: &RequestTemplate,
    view: &TemplatePrefixView,
    locale: LocaleSnapshot,
) -> Result<TemplateView> {
    let l = locale.create(namespace::REPORT_PREFIX_TREE)?;
    let member_count = template.member_request_ids.len();
    let stable_nodes = view
        .nodes
        .iter()
        .filter(|node| node.kind == PrefixNodeKind::Stable)
        .collect::<Vec<_>>();
    let dynamic_nodes = view
        .nodes
        .iter()
        .filter(|node| node.kind == PrefixNodeKind::Dynamic)
        .collect::<Vec<_>>();
    let stable_bytes = stable_nodes
        .iter()
        .map(|node| node.utf8_bytes)
        .sum::<usize>();
    let dynamic_bytes = dynamic_nodes
        .iter()
        .map(|node| node.utf8_bytes)
        .sum::<usize>();
    let variant_total = dynamic_nodes
        .iter()
        .map(|node| node.variants.as_ref().map(Vec::len).unwrap_or(0))
        .sum::<usize>();
    let missing_total = dynamic_nodes
        .iter()
        .flat_map(|node| node.variants.iter().flatten())
        .filter(|variant| variant.excerpt.starts_with("（此位置缺失"))
        .count();
    let defect_nodes = dynamic_nodes
        .iter()
        .filter(|node| !node.defect_ids.is_empty())
        .count();
    let by_id: HashMap<&str, &PrefixNode> = view
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut chain = Vec::new();
    if let Some(mut current) = by_id.get("seq-0").copied() {
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current.id.as_str()) {
                bail!("prefix view sequence contains a cycle at {}", current.id);
            }
            chain.push(current);
            match current
                .sequence_child
                .as_deref()
                .and_then(|id| by_id.get(id).copied())
            {
                Some(next) => current = next,
                None => break,
            }
        }
    }
    let first_dynamic = chain
        .iter()
        .position(|node| node.kind == PrefixNodeKind::Dynamic);
    let stable_before_first_x = match first_dynamic {
        Some(end) => chain[..end].iter().map(|node| node.utf8_bytes).sum(),
        None => stable_bytes,
    };
    let total_bytes = stable_bytes.saturating_add(dynamic_bytes).max(1);
    let stable_pct = stable_bytes as f64 * 100.0 / total_bytes as f64;
    let dynamic_pct = dynamic_bytes as f64 * 100.0 / total_bytes as f64;
    let first_divergence = first_dynamic
        .map(|first_x| {
            let node = chain[first_x];
            l.format(
                "first_divergence",
                &[
                    ("index", (first_x + 1).to_string()),
                    ("excerpt", excerpt_label(&node.excerpt, 48)),
                    ("support", node.support_count.to_string()),
                    ("members", member_count.to_string()),
                    ("ratio", format!("{:.0}", node.support_ratio * 100.0)),
                ],
            )
        })
        .transpose()?;
    Ok(TemplateView {
        anchor: template.id.clone(),
        short_id: template.id.chars().take(16).collect(),
        id: template.id.clone(),
        members_summary: l.format(
            "members_nodes",
            &[
                ("members", member_count.to_string()),
                ("nodes", view.nodes.len().to_string()),
                ("stable", stable_nodes.len().to_string()),
                ("dynamic", dynamic_nodes.len().to_string()),
            ],
        )?,
        stats: vec![
            StatView {
                label: l.get("stable_bytes")?.to_string(),
                value: format!("{stable_bytes} B"),
            },
            StatView {
                label: l.get("dynamic_bytes")?.to_string(),
                value: format!("{dynamic_bytes} B"),
            },
            StatView {
                label: l.get("variant_total")?.to_string(),
                value: variant_total.to_string(),
            },
            StatView {
                label: l.get("missing_variants")?.to_string(),
                value: missing_total.to_string(),
            },
            StatView {
                label: l.get("defect_nodes")?.to_string(),
                value: defect_nodes.to_string(),
            },
        ],
        stable_width: format!("{stable_pct:.1}"),
        dynamic_width: format!("{dynamic_pct:.1}"),
        stable_bytes,
        stable_percent: format!("{stable_pct:.1}"),
        dynamic_bytes,
        dynamic_percent: format!("{dynamic_pct:.1}"),
        first_prefix: l.format(
            "first_prefix",
            &[("bytes", stable_before_first_x.to_string())],
        )?,
        first_divergence,
        decay: (chain.len() > 1).then(|| build_decay(&chain, member_count)),
        nodes: chain
            .iter()
            .enumerate()
            .map(|(index, node)| build_node(template, node, index, member_count, locale))
            .collect::<Result<Vec<_>>>()?,
        truncated: view
            .truncated
            .then(|| l.get("truncated").map(str::to_string))
            .transpose()?,
    })
}

fn build_node(
    template: &RequestTemplate,
    node: &PrefixNode,
    index: usize,
    member_count: usize,
    locale: LocaleSnapshot,
) -> Result<NodeView> {
    let l = locale.create(namespace::REPORT_PREFIX_TREE)?;
    let is_dynamic = node.kind == PrefixNodeKind::Dynamic;
    let is_defect = !node.defect_ids.is_empty();
    let class = match (is_dynamic, is_defect) {
        (true, true) => "pt-node is-x is-defect",
        (true, false) => "pt-node is-x",
        (false, true) => "pt-node is-p is-defect",
        (false, false) => "pt-node is-p",
    };
    let (badge_class, kind_label) = if is_dynamic {
        ("pt-badge pt-badge-x", "X")
    } else {
        ("pt-badge pt-badge-p", "P")
    };
    let source = &node.source;
    let position = match (source.byte_start, source.byte_end) {
        (Some(start), Some(end)) => format!("bytes {start}..{end}"),
        _ => "bytes —".to_string(),
    };
    let variants = node
        .variants
        .as_ref()
        .map(|variants| {
            let mut ordered = variants.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|entry| std::cmp::Reverse(entry.count));
            ordered
                .into_iter()
                .map(|variant| {
                    let missing = variant.excerpt.starts_with("（此位置缺失");
                    let label = if missing {
                        l.get("missing_at_position")?.to_string()
                    } else {
                        variant.excerpt.clone()
                    };
                    Ok(VariantView {
                        class: if missing {
                            "pt-var is-missing"
                        } else {
                            "pt-var"
                        },
                        label,
                        count: if variant.count_known {
                            format!("×{}", variant.count)
                        } else {
                            format!("×{}{}", variant.count, l.get("approx")?)
                        },
                        zero_label: (variant.count_known && variant.count == 0)
                            .then(|| l.get("not_seen").map(str::to_string))
                            .transpose()?,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let all: BTreeSet<&str> = template
        .member_request_ids
        .iter()
        .map(String::as_str)
        .collect();
    let alive: BTreeSet<&str> = node.member_request_ids.iter().map(String::as_str).collect();
    let lost = all.difference(&alive).copied().collect::<Vec<_>>();
    let has_lost = !lost.is_empty();
    let (alive_members, alive_truncated) = member_preview(alive.iter().copied());
    let (lost_members, lost_truncated) = member_preview(lost.into_iter());
    Ok(NodeView {
        class,
        badge_class,
        kind_label,
        index,
        open: is_dynamic,
        excerpt: excerpt_label(&node.excerpt, 120),
        bytes: node.utf8_bytes,
        percent: format!("{:.0}", node.support_ratio * 100.0),
        support: node.support_count,
        members: member_count,
        warning: is_defect,
        source_path: source.json_path.clone(),
        role: source.role.clone(),
        position,
        unit: source.unit_index,
        support_value: l.format(
            "support_value",
            &[
                ("support", node.support_count.to_string()),
                ("members", member_count.to_string()),
                ("ratio", format!("{:.0}", node.support_ratio * 100.0)),
            ],
        )?,
        summary: node.excerpt.clone(),
        variants,
        defect_ids: node.defect_ids.clone(),
        content_sample: node
            .content_sample
            .clone()
            .filter(|sample| !sample.is_empty()),
        show_members: is_dynamic || has_lost,
        alive_count: alive.len(),
        all_count: all.len(),
        alive_members,
        alive_truncated,
        lost_members,
        lost_truncated,
    })
}

fn member_preview<'a>(ids: impl Iterator<Item = &'a str>) -> (Vec<String>, bool) {
    let ids = ids.collect::<Vec<_>>();
    let truncated = ids.len() > 8;
    (
        ids.into_iter()
            .take(8)
            .map(super::super::format::short_id)
            .collect(),
        truncated,
    )
}
