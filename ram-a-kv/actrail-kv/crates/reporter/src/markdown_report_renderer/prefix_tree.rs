//! 本文件渲染模板前缀树、成员衰减和结构片段明细。

use std::{collections::BTreeSet, fmt::Write};

use actrail_kv_artifacts::{
    AnalysisResult, PrefixNode, PrefixNodeKind, RequestTemplate, TemplatePrefixView,
};
use anyhow::Result;

use crate::locale::{namespace, LocaleModule, LocaleSnapshot};

use super::{
    defects::defect_anchor,
    format::{code, excerpt, fenced, heading_anchor, inline, short_id, table_code},
};

pub(super) fn render(
    output: &mut String,
    result: &AnalysisResult,
    locale: LocaleSnapshot,
) -> Result<()> {
    let l = locale.create(namespace::REPORT_PREFIX_TREE)?;
    let defects_locale = locale.create(namespace::REPORT_DEFECTS)?;
    let templates: Vec<_> = result
        .templates
        .iter()
        .filter_map(|template| template.prefix_view.as_ref().map(|view| (template, view)))
        .collect();
    if templates.is_empty() {
        return Ok(());
    }
    writeln!(output, "## {}\n", l.get("section_title")?)?;
    writeln!(output, "- {}", l.get("legend_stable")?)?;
    writeln!(output, "- {}", l.get("legend_dynamic")?)?;
    writeln!(output, "- {}", l.get("legend_defect")?)?;
    writeln!(output, "- {}", l.get("legend_support")?)?;
    writeln!(output, "- {}\n", l.get("overview")?)?;

    if templates.len() > 1 {
        writeln!(
            output,
            "| {} | {} | {} | {} | {} |\n|---|---|---:|---:|---:|",
            l.get("template_header")?,
            l.get("members_header")?,
            l.get("stable_bytes_header")?,
            l.get("dynamic_nodes_header")?,
            l.get("defect_nodes_header")?
        )?;
        for (template, view) in &templates {
            let stable_bytes: usize = view
                .nodes
                .iter()
                .filter(|node| node.kind == PrefixNodeKind::Stable)
                .map(|node| node.utf8_bytes)
                .sum();
            let dynamic_count = view
                .nodes
                .iter()
                .filter(|node| node.kind == PrefixNodeKind::Dynamic)
                .count();
            let defect_count = view
                .nodes
                .iter()
                .filter(|node| node.kind == PrefixNodeKind::Dynamic && !node.defect_ids.is_empty())
                .count();
            let anchor = template_anchor(&template.id, l)?;
            writeln!(
                output,
                "| [{}](#{}) | {} | {} B | {} | {} |",
                table_code(&excerpt(&template.id, 16)),
                anchor,
                template.member_request_ids.len(),
                stable_bytes,
                dynamic_count,
                defect_count
            )?;
        }
        output.push('\n');
    }

    for (template, view) in templates {
        render_template(output, template, view, l, defects_locale)?;
    }
    Ok(())
}

fn render_template(
    output: &mut String,
    template: &RequestTemplate,
    view: &TemplatePrefixView,
    l: LocaleModule,
    defects_locale: LocaleModule,
) -> Result<()> {
    let member_count = template.member_request_ids.len();
    let stable_nodes: Vec<_> = view
        .nodes
        .iter()
        .filter(|node| node.kind == PrefixNodeKind::Stable)
        .collect();
    let dynamic_nodes: Vec<_> = view
        .nodes
        .iter()
        .filter(|node| node.kind == PrefixNodeKind::Dynamic)
        .collect();
    let stable_bytes: usize = stable_nodes.iter().map(|node| node.utf8_bytes).sum();
    let dynamic_bytes: usize = dynamic_nodes.iter().map(|node| node.utf8_bytes).sum();
    let variant_total: usize = dynamic_nodes
        .iter()
        .map(|node| node.variants.as_ref().map(Vec::len).unwrap_or(0))
        .sum();
    let missing_total: usize = dynamic_nodes
        .iter()
        .flat_map(|node| node.variants.iter().flatten())
        .filter(|variant| variant.excerpt.starts_with("（此位置缺失"))
        .count();
    let defect_nodes = dynamic_nodes
        .iter()
        .filter(|node| !node.defect_ids.is_empty())
        .count();
    let by_id: std::collections::HashMap<&str, &PrefixNode> = view
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut chain = Vec::new();
    if let Some(mut current) = by_id.get("seq-0").copied() {
        loop {
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
    let stable_before_first_x = first_dynamic
        .map(|end| chain[..end].iter().map(|node| node.utf8_bytes).sum())
        .unwrap_or(stable_bytes);
    writeln!(
        output,
        "### {} {}\n",
        l.get("template_header")?,
        heading_anchor(&template.id)
    )?;
    writeln!(output, "- {}: {}\n", l.get("full_id")?, code(&template.id))?;
    writeln!(
        output,
        "- {}\n",
        l.format(
            "members_nodes",
            &[
                ("members", member_count.to_string()),
                ("nodes", view.nodes.len().to_string()),
                ("stable", stable_nodes.len().to_string()),
                ("dynamic", dynamic_nodes.len().to_string()),
            ]
        )?
    )?;
    writeln!(
        output,
        "| {} | {} |\n|---|---:|",
        l.get("metric_header")?,
        l.get("value_header")?
    )?;
    writeln!(output, "| {} | {stable_bytes} B |", l.get("stable_bytes")?)?;
    writeln!(
        output,
        "| {} | {dynamic_bytes} B |",
        l.get("dynamic_bytes")?
    )?;
    writeln!(output, "| {} | {variant_total} |", l.get("variant_total")?)?;
    writeln!(
        output,
        "| {} | {missing_total} |",
        l.get("missing_variants")?
    )?;
    writeln!(output, "| {} | {defect_nodes} |\n", l.get("defect_nodes")?)?;

    let total_bytes = stable_bytes.saturating_add(dynamic_bytes).max(1);
    let stable_pct = stable_bytes as f64 * 100.0 / total_bytes as f64;
    let dynamic_pct = dynamic_bytes as f64 * 100.0 / total_bytes as f64;
    writeln!(
        output,
        "#### {}\n\n| {} | {} | {} |\n|---|---:|---:|",
        l.get("distribution_title")?,
        l.get("region_header")?,
        l.get("bytes_header")?,
        l.get("ratio_header")?
    )?;
    writeln!(
        output,
        "| {} | {stable_bytes} B | {stable_pct:.1}% |",
        l.get("stable_region")?
    )?;
    writeln!(
        output,
        "| {} | {dynamic_bytes} B | {dynamic_pct:.1}% |\n",
        l.get("dynamic_region")?
    )?;
    writeln!(
        output,
        "{}\n",
        l.format(
            "first_prefix",
            &[("bytes", stable_before_first_x.to_string())]
        )?
    )?;
    if let Some(first_x) = first_dynamic {
        let node = chain[first_x];
        writeln!(
            output,
            "{}\n",
            l.format(
                "first_divergence",
                &[
                    ("index", (first_x + 1).to_string()),
                    ("excerpt", code(&excerpt(&node.excerpt, 48))),
                    ("support", node.support_count.to_string()),
                    ("members", member_count.to_string()),
                    ("ratio", format!("{:.0}", node.support_ratio * 100.0)),
                ]
            )?
        )?;
    }

    if chain.len() > 1 {
        writeln!(
            output,
            "#### {}\n\n| {} | {} | {} | {} |\n|---:|---|---:|---:|",
            l.get("decay_title")?,
            l.get("decay_index")?,
            l.get("decay_type")?,
            l.get("decay_alive")?,
            l.get("decay_support")?
        )?;
        for (index, node) in chain.iter().enumerate() {
            writeln!(
                output,
                "| {} | {} | {}/{} | {:.1}% |",
                index,
                if node.kind == PrefixNodeKind::Dynamic {
                    "X"
                } else {
                    "P"
                },
                node.support_count,
                member_count,
                node.support_ratio * 100.0
            )?;
        }
        writeln!(output, "\n{}\n", l.get("decay_note")?)?;
    }

    writeln!(output, "#### {}\n", l.get("structure_title")?)?;
    for (index, node) in chain.iter().enumerate() {
        render_node(
            output,
            template,
            node,
            index,
            member_count,
            l,
            defects_locale,
        )?;
    }
    if view.truncated {
        writeln!(output, "{}\n", l.get("truncated")?)?;
    }
    writeln!(
        output,
        "[{}](#{})\n",
        l.get("return_templates")?,
        heading_anchor(l.get("section_title")?)
    )?;
    Ok(())
}

fn render_node(
    output: &mut String,
    template: &RequestTemplate,
    node: &PrefixNode,
    index: usize,
    member_count: usize,
    l: LocaleModule,
    defects_locale: LocaleModule,
) -> Result<()> {
    let kind = if node.kind == PrefixNodeKind::Dynamic {
        "X"
    } else {
        "P"
    };
    writeln!(
        output,
        "##### {} #{index} · {kind} · {}\n",
        l.get("fragment")?,
        inline(&excerpt(&node.excerpt, 120))
    )?;
    writeln!(
        output,
        "- {}: {kind}{}",
        l.get("type_label")?,
        if node.defect_ids.is_empty() {
            ""
        } else {
            l.get("defect_marker")?
        }
    )?;
    write!(
        output,
        "- {}: {}",
        l.get("source")?,
        code(&node.source.json_path)
    )?;
    if let Some(role) = node.source.role.as_deref() {
        write!(output, "; role={}", code(role))?;
    }
    output.push('\n');
    let range = match (node.source.byte_start, node.source.byte_end) {
        (Some(start), Some(end)) => format!("bytes {start}..{end}"),
        _ => "bytes —".to_string(),
    };
    write!(output, "- {}: {}", l.get("position")?, code(&range))?;
    if let Some(unit) = node.source.unit_index {
        write!(output, "; unit #{unit}")?;
    }
    output.push('\n');
    writeln!(
        output,
        "- {}: {} B; {}: {}/{} ({:.0}%)",
        l.get("byte_support")?,
        node.utf8_bytes,
        l.get("support")?,
        node.support_count,
        member_count,
        node.support_ratio * 100.0
    )?;
    writeln!(output, "- {}: {}\n", l.get("summary")?, code(&node.excerpt))?;

    if let Some(variants) = &node.variants {
        if !variants.is_empty() {
            writeln!(
                output,
                "{}:\n\n| {} | {} |\n|---|---:|",
                l.get("variants")?,
                l.get("variants")?,
                l.get("count")?
            )?;
            let mut ordered = variants.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|variant| std::cmp::Reverse(variant.count));
            for variant in ordered {
                let missing = variant.excerpt.starts_with("（此位置缺失");
                let label = if missing {
                    l.get("missing_at_position")?.to_string()
                } else {
                    variant.excerpt.clone()
                };
                let mut count = if variant.count_known {
                    variant.count.to_string()
                } else {
                    format!("{}{}", variant.count, l.get("approx")?)
                };
                if variant.count == 0 {
                    count.push_str(l.get("not_seen")?);
                }
                writeln!(output, "| {} | {} |", table_code(&label), count)?;
            }
            output.push('\n');
        }
    }

    if !node.defect_ids.is_empty() {
        writeln!(output, "{}:\n", l.get("associated_defects")?)?;
        for id in &node.defect_ids {
            writeln!(
                output,
                "- [{}](#{})",
                code(id),
                defect_anchor(id, defects_locale)?
            )?;
        }
        output.push('\n');
    }

    if let Some(sample) = &node.content_sample {
        if !sample.is_empty() {
            writeln!(output, "{}:\n", l.get("content_sample")?)?;
            writeln!(output, "{}\n", fenced(sample))?;
        }
    }

    let all: BTreeSet<&str> = template
        .member_request_ids
        .iter()
        .map(String::as_str)
        .collect();
    let alive: BTreeSet<&str> = node.member_request_ids.iter().map(String::as_str).collect();
    let lost: Vec<&str> = all.difference(&alive).copied().collect();
    if node.kind == PrefixNodeKind::Dynamic || !lost.is_empty() {
        writeln!(
            output,
            "- {}",
            l.format(
                "alive_members",
                &[
                    ("alive", alive.len().to_string()),
                    ("all", all.len().to_string()),
                    ("members", member_list(alive.iter().copied(), l)?),
                ]
            )?
        )?;
        if !lost.is_empty() {
            writeln!(
                output,
                "- {}",
                l.format(
                    "lost_members",
                    &[
                        ("lost", lost.len().to_string()),
                        ("members", member_list(lost.iter().copied(), l)?),
                    ]
                )?
            )?;
        }
        output.push('\n');
    }
    Ok(())
}

fn member_list<'a>(ids: impl Iterator<Item = &'a str>, l: LocaleModule) -> Result<String> {
    let mut values = Vec::new();
    for (index, id) in ids.enumerate() {
        if index == 8 {
            values.push("…".to_string());
            break;
        }
        values.push(code(&short_id(id)));
    }
    if values.is_empty() {
        Ok("—".to_string())
    } else {
        Ok(values.join(
            l.get("list_separator")
                .map_err(|error| anyhow::anyhow!(error))?,
        ))
    }
}

fn template_anchor(id: &str, l: LocaleModule) -> Result<String> {
    Ok(heading_anchor(&format!(
        "{} {}",
        l.get("template_header")?,
        heading_anchor(id)
    )))
}
