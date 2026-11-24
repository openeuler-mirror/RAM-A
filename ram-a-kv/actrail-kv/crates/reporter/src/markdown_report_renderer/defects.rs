//! 本文件渲染 Top K 与全部 P1/X/P2 缺陷详情。

use std::{collections::BTreeMap, fmt::Write};

use actrail_kv_artifacts::{AnalysisResult, ContextDefect, DefectFactKind, MismatchPattern};
use anyhow::{anyhow, Result};

use crate::locale::{namespace, LocaleModule, LocaleSnapshot};

use super::format::{code, heading_anchor, inline, table_code};

pub(super) fn render_top_k(
    output: &mut String,
    result: &AnalysisResult,
    locale: LocaleSnapshot,
) -> Result<()> {
    let l = locale.create(namespace::REPORT_TOP_K)?;
    let defects: BTreeMap<_, _> = result
        .defects
        .iter()
        .map(|defect| (defect.id.as_str(), defect))
        .collect();
    writeln!(output, "## {}\n", l.get("section_title")?)?;
    writeln!(output, "{}\n", l.get("intro")?)?;
    if result.top_k.is_empty() {
        writeln!(output, "{}\n", l.get("empty")?)?;
        return Ok(());
    }
    writeln!(
        output,
        "| {} | {} | {} | {} | {} | {} | {} |",
        l.get("rank_header")?,
        l.get("location_header")?,
        l.get("blocked_header")?,
        l.get("affected_header")?,
        l.get("score_header")?,
        l.get("advice_header")?,
        l.get("defect_header")?
    )?;
    output.push_str("|---:|---|---:|---:|---:|---|---|\n");
    let defects_locale = locale.create(namespace::REPORT_DEFECTS)?;
    let common = locale.create(namespace::REPORT_COMMON)?;
    let role = common.get("role")?;
    let unit = common.get("unit")?;
    for (index, id) in result.top_k.iter().enumerate() {
        let defect = defects
            .get(id.as_str())
            .ok_or_else(|| anyhow!("top_k references unknown defect {id}"))?;
        let advice = defect
            .insights
            .first()
            .map(|insight| insight.summary.as_str())
            .unwrap_or("—");
        writeln!(
            output,
            "| {} | {} | {} | {} | {:.2} | {} | [{}](#{}) |",
            index + 1,
            table_code(&defect_location(defect, role, unit)),
            defect.blocked_stable_bytes,
            defect.affected_count,
            defect.score.score,
            inline(advice),
            table_code(id),
            defect_anchor(id, defects_locale)?
        )?;
    }
    output.push('\n');
    Ok(())
}

pub(super) fn render_details(
    output: &mut String,
    result: &AnalysisResult,
    locale: LocaleSnapshot,
) -> Result<()> {
    let l = locale.create(namespace::REPORT_DEFECTS)?;
    writeln!(output, "## {}\n", l.get("section_title")?)?;
    for defect in &result.defects {
        render_defect(output, defect, locale)?;
    }
    Ok(())
}

fn render_defect(
    output: &mut String,
    defect: &ContextDefect,
    locale: LocaleSnapshot,
) -> Result<()> {
    let l = locale.create(namespace::REPORT_DEFECTS)?;
    let common = locale.create(namespace::REPORT_COMMON)?;
    writeln!(
        output,
        "### {} {}\n",
        l.get("item_title")?,
        defect_label(&defect.id)
    )?;
    writeln!(output, "[{}](#top-k)\n", common.get("back_to_top_k")?)?;
    writeln!(
        output,
        "- {}: {}\n",
        l.get("original_id")?,
        code(&defect.id)
    )?;
    writeln!(
        output,
        "- {}: window={} endpoint={} model={} schema={}",
        l.get("comparison_group")?,
        code(&defect.comparison_group.time_window_key),
        code(&defect.comparison_group.endpoint_key),
        code(&defect.comparison_group.model),
        code(&defect.comparison_group.context_schema_key)
    )?;
    if let Some(agent) = &defect.comparison_group.agent_key {
        writeln!(output, "- {}: {}", l.get("agent")?, code(agent))?;
    }
    if let Some(deployment) = &defect.comparison_group.model_deployment_key {
        writeln!(output, "- {}: {}", l.get("deployment")?, code(deployment))?;
    }
    if let Some(namespace) = &defect.comparison_group.kv_namespace {
        writeln!(output, "- {}: {}", l.get("namespace")?, code(namespace))?;
    }
    writeln!(
        output,
        "- {}\n",
        l.format(
            "p1_summary",
            &[
                ("actual", defect.actual_prefix_bytes.to_string()),
                (
                    "pattern",
                    pattern_label(&defect.mismatch.pattern, l)?.to_owned()
                ),
                ("stable", defect.recovered_stable.utf8_bytes.to_string()),
                ("support", defect.recovered_stable.support_count.to_string()),
            ],
        )?
    )?;

    writeln!(output, "#### {}\n", l.get("variants_title")?)?;
    for variant in &defect.mismatch.variants {
        let evidence = &variant.representative;
        writeln!(
            output,
            "- {}; {}: {}; {}",
            l.format(
                "request_count",
                &[("count", variant.member_request_ids.len().to_string())]
            )?,
            l.get("representative")?,
            code(&evidence.request_id),
            l.format("bytes", &[("bytes", evidence.utf8_bytes.to_string())])?
        )?;
        if !evidence.sources.is_empty() {
            write!(output, "  - {}: ", l.get("location")?)?;
            for (index, source) in evidence.sources.iter().enumerate() {
                if index > 0 {
                    output.push(';');
                }
                write!(output, "{}", code(&source.json_path))?;
            }
            output.push('\n');
        }
        match &evidence.excerpt {
            Some(excerpt) => writeln!(output, "  - {}: {}\n", l.get("content")?, code(excerpt))?,
            None => writeln!(output, "  - {}\n", l.get("missing_variant")?)?,
        }
    }

    if !defect.mismatch.facts.is_empty() {
        writeln!(output, "#### {}\n", l.get("facts_title")?)?;
        for fact in &defect.mismatch.facts {
            write!(output, "- {}", fact_label(&fact.kind, l)?)?;
            if let Some(detail) = &fact.detail {
                write!(output, ": {}", inline(detail))?;
            }
            output.push('\n');
        }
        output.push('\n');
    }

    writeln!(output, "#### {}\n", l.get("p2_title")?)?;
    writeln!(output, "{}\n", code(&defect.recovered_stable.excerpt))?;
    writeln!(
        output,
        "| {} | {} |\n|---|---:|",
        l.get("metric_header")?,
        l.get("value_header")?
    )?;
    writeln!(
        output,
        "| {} | {} bytes |",
        l.get("actual_prefix")?,
        defect.actual_prefix_bytes
    )?;
    writeln!(
        output,
        "| {} | {} bytes |",
        l.get("potential_prefix")?,
        defect.potential_prefix_bytes
    )?;
    writeln!(
        output,
        "| {} | {} bytes |",
        l.get("blocked_bytes")?,
        defect.blocked_stable_bytes
    )?;
    writeln!(
        output,
        "| {} | {} |",
        l.get("comparable_requests")?,
        defect.comparable_count
    )?;
    writeln!(
        output,
        "| {} | {} |",
        l.get("affected_requests")?,
        defect.affected_count
    )?;
    writeln!(
        output,
        "| {} | {:.3} |\n",
        l.get("confidence")?,
        defect.confidence
    )?;

    if !defect.insights.is_empty() {
        writeln!(output, "#### {}\n", l.get("insights_title")?)?;
        for insight in &defect.insights {
            write!(output, "- **{}**", inline(&insight.summary))?;
            if let Some(detail) = &insight.detail {
                write!(output, ": {}", inline(detail))?;
            }
            output.push('\n');
        }
        output.push('\n');
    }
    Ok(())
}

pub(super) fn defect_anchor(id: &str, locale: LocaleModule) -> Result<String> {
    Ok(heading_anchor(&format!(
        "{} {}",
        locale.get("item_title")?,
        defect_label(id)
    )))
}

fn defect_label(id: &str) -> String {
    heading_anchor(id)
}

fn defect_location(defect: &ContextDefect, role_label: &str, unit_label: &str) -> String {
    let source = defect
        .mismatch
        .variants
        .iter()
        .find_map(|variant| variant.representative.sources.first());
    let Some(source) = source else {
        return "—".to_string();
    };
    let mut location = source.json_path.clone();
    if let Some(role) = source.role.as_deref() {
        write!(location, " ({role_label}={role})").expect("writing into String cannot fail");
    }
    if let Some(unit) = source.unit_index {
        write!(location, " · {unit_label} #{unit}").expect("writing into String cannot fail");
    }
    location
}

fn pattern_label(pattern: &MismatchPattern, locale: LocaleModule) -> Result<&'static str> {
    locale.get(match pattern {
        MismatchPattern::ValueMismatch => "pattern_value_mismatch",
        MismatchPattern::InsertionDeletion => "pattern_insertion_deletion",
        MismatchPattern::Reorder => "pattern_reorder",
        MismatchPattern::Mixed => "pattern_mixed",
    })
}

fn fact_label(kind: &DefectFactKind, locale: LocaleModule) -> Result<&'static str> {
    locale.get(match kind {
        DefectFactKind::ContentVariation => "fact_content_variation",
        DefectFactKind::FixedVariants => "fact_fixed_variants",
        DefectFactKind::StructuredDataEquivalent => "fact_structured_equivalence",
        DefectFactKind::InsertionDeletion => "fact_insertion_deletion",
        DefectFactKind::Reorder => "fact_reorder",
        DefectFactKind::Scope => "fact_scope",
    })
}
