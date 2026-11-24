//! 本文件把 Top K 和缺陷证据转换为纯展示模型。

use anyhow::Result;

use actrail_kv_artifacts::{AnalysisResult, ContextDefect, DefectFactKind, MismatchPattern};

use super::super::format::defect_location;
use crate::locale::{namespace, LocaleModule, LocaleSnapshot};

pub(crate) struct TopKView {
    pub(crate) title: String,
    pub(crate) intro_before: String,
    pub(crate) intro_after: String,
    pub(crate) empty: Option<String>,
    pub(crate) rank_header: String,
    pub(crate) location_header: String,
    pub(crate) blocked_header: String,
    pub(crate) affected_header: String,
    pub(crate) score_header: String,
    pub(crate) advice_header: String,
    pub(crate) defect_header: String,
    pub(crate) rows: Vec<TopKRow>,
}

pub(crate) struct TopKRow {
    pub(crate) rank: usize,
    pub(crate) location: String,
    pub(crate) blocked: usize,
    pub(crate) affected: usize,
    pub(crate) score: String,
    pub(crate) advice: String,
    pub(crate) defect_id: String,
}

pub(crate) struct DefectsView {
    pub(crate) title: String,
    pub(crate) labels: DefectLabels,
    pub(crate) items: Vec<DefectView>,
}

pub(crate) struct DefectLabels {
    pub(crate) back_to_top_k: String,
    pub(crate) comparison_group: String,
    pub(crate) variants_title: String,
    pub(crate) facts_title: String,
    pub(crate) p2_title: String,
    pub(crate) insights_title: String,
    pub(crate) missing_variant: String,
    pub(crate) location: String,
}

pub(crate) struct DefectView {
    pub(crate) id: String,
    pub(crate) comparison: ComparisonView,
    pub(crate) extras: Vec<ComparisonExtra>,
    pub(crate) p1_summary: String,
    pub(crate) variants: Vec<VariantView>,
    pub(crate) facts: Vec<FactView>,
    pub(crate) p2_excerpt: String,
    pub(crate) p2_rows: Vec<MetricRow>,
    pub(crate) insights: Vec<InsightView>,
}

pub(crate) struct ComparisonView {
    pub(crate) time_window: String,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) schema: String,
}

pub(crate) struct ComparisonExtra {
    pub(crate) label: String,
    pub(crate) value: String,
}

pub(crate) struct VariantView {
    pub(crate) request_count: String,
    pub(crate) representative_label: String,
    pub(crate) request_id: String,
    pub(crate) bytes: String,
    pub(crate) locations: Vec<String>,
    pub(crate) excerpt: Option<String>,
}

pub(crate) struct FactView {
    pub(crate) label: String,
    pub(crate) detail: Option<String>,
}

pub(crate) struct MetricRow {
    pub(crate) label: String,
    pub(crate) value: String,
}

pub(crate) struct InsightView {
    pub(crate) summary: String,
    pub(crate) detail: Option<String>,
}

pub(crate) fn build_top_k(result: &AnalysisResult, locale: LocaleSnapshot) -> Result<TopKView> {
    let l = locale.create(namespace::REPORT_TOP_K)?;
    let common = locale.create(namespace::REPORT_COMMON)?;
    let intro = l.get("intro")?;
    let (intro_before, intro_after) = intro
        .split_once("`top_k[]`")
        .map(|(before, after)| (before.to_string(), after.to_string()))
        .unwrap_or_else(|| (intro.to_string(), String::new()));
    let defects = result
        .defects
        .iter()
        .map(|defect| (defect.id.as_str(), defect))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut rows = Vec::with_capacity(result.top_k.len());
    for (index, id) in result.top_k.iter().enumerate() {
        let defect = defects
            .get(id.as_str())
            .ok_or_else(|| anyhow::anyhow!("top_k references unknown defect {id}"))?;
        rows.push(TopKRow {
            rank: index + 1,
            location: defect_location(defect, common.get("role")?, common.get("unit")?),
            blocked: defect.blocked_stable_bytes,
            affected: defect.affected_count,
            score: format!("{:.2}", defect.score.score),
            advice: defect
                .insights
                .first()
                .map(|insight| insight.summary.clone())
                .unwrap_or_else(|| "—".to_string()),
            defect_id: defect.id.clone(),
        });
    }
    Ok(TopKView {
        title: l.get("section_title")?.to_string(),
        intro_before,
        intro_after,
        empty: result
            .top_k
            .is_empty()
            .then(|| l.get("empty").map(str::to_string))
            .transpose()?,
        rank_header: l.get("rank_header")?.to_string(),
        location_header: l.get("location_header")?.to_string(),
        blocked_header: l.get("blocked_header")?.to_string(),
        affected_header: l.get("affected_header")?.to_string(),
        score_header: l.get("score_header")?.to_string(),
        advice_header: l.get("advice_header")?.to_string(),
        defect_header: l.get("defect_header")?.to_string(),
        rows,
    })
}

pub(crate) fn build_defects(
    result: &AnalysisResult,
    locale: LocaleSnapshot,
) -> Result<DefectsView> {
    let l = locale.create(namespace::REPORT_DEFECTS)?;
    let common = locale.create(namespace::REPORT_COMMON)?;
    let items = result
        .defects
        .iter()
        .map(|defect| build_defect(defect, l))
        .collect::<Result<Vec<_>>>()?;
    Ok(DefectsView {
        title: l.get("section_title")?.to_string(),
        labels: DefectLabels {
            back_to_top_k: common.get("back_to_top_k")?.to_string(),
            comparison_group: l.get("comparison_group")?.to_string(),
            variants_title: l.get("variants_title")?.to_string(),
            facts_title: l.get("facts_title")?.to_string(),
            p2_title: l.get("p2_title")?.to_string(),
            insights_title: l.get("insights_title")?.to_string(),
            missing_variant: l.get("missing_variant")?.to_string(),
            location: l.get("location")?.to_string(),
        },
        items,
    })
}

fn build_defect(defect: &ContextDefect, l: LocaleModule) -> Result<DefectView> {
    let mut extras = Vec::new();
    if let Some(agent) = &defect.comparison_group.agent_key {
        extras.push(ComparisonExtra {
            label: l.get("agent")?.to_string(),
            value: agent.clone(),
        });
    }
    if let Some(deployment) = &defect.comparison_group.model_deployment_key {
        extras.push(ComparisonExtra {
            label: l.get("deployment")?.to_string(),
            value: deployment.clone(),
        });
    }
    if let Some(namespace) = &defect.comparison_group.kv_namespace {
        extras.push(ComparisonExtra {
            label: l.get("namespace")?.to_string(),
            value: namespace.clone(),
        });
    }
    let variants = defect
        .mismatch
        .variants
        .iter()
        .map(|variant| {
            let evidence = &variant.representative;
            Ok(VariantView {
                request_count: l.format(
                    "request_count",
                    &[("count", variant.member_request_ids.len().to_string())],
                )?,
                representative_label: l.get("representative")?.to_string(),
                request_id: evidence.request_id.clone(),
                bytes: l.format("bytes", &[("bytes", evidence.utf8_bytes.to_string())])?,
                locations: evidence
                    .sources
                    .iter()
                    .map(|source| source.json_path.clone())
                    .collect(),
                excerpt: evidence.excerpt.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let facts = defect
        .mismatch
        .facts
        .iter()
        .map(|fact| {
            Ok(FactView {
                label: fact_label(&fact.kind, l)?.to_string(),
                detail: fact.detail.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let p2_rows = vec![
        MetricRow {
            label: l.get("actual_prefix")?.to_string(),
            value: format!("{} bytes", defect.actual_prefix_bytes),
        },
        MetricRow {
            label: l.get("potential_prefix")?.to_string(),
            value: format!("{} bytes", defect.potential_prefix_bytes),
        },
        MetricRow {
            label: l.get("blocked_bytes")?.to_string(),
            value: format!("{} bytes", defect.blocked_stable_bytes),
        },
        MetricRow {
            label: l.get("comparable_requests")?.to_string(),
            value: defect.comparable_count.to_string(),
        },
        MetricRow {
            label: l.get("affected_requests")?.to_string(),
            value: defect.affected_count.to_string(),
        },
        MetricRow {
            label: l.get("confidence")?.to_string(),
            value: format!("{:.3}", defect.confidence),
        },
    ];
    Ok(DefectView {
        id: defect.id.clone(),
        comparison: ComparisonView {
            time_window: defect.comparison_group.time_window_key.clone(),
            endpoint: defect.comparison_group.endpoint_key.clone(),
            model: defect.comparison_group.model.clone(),
            schema: defect.comparison_group.context_schema_key.clone(),
        },
        extras,
        p1_summary: l.format(
            "p1_summary",
            &[
                ("actual", defect.actual_prefix_bytes.to_string()),
                (
                    "pattern",
                    pattern_label(&defect.mismatch.pattern, l)?.to_string(),
                ),
                ("stable", defect.recovered_stable.utf8_bytes.to_string()),
                ("support", defect.recovered_stable.support_count.to_string()),
            ],
        )?,
        variants,
        facts,
        p2_excerpt: defect.recovered_stable.excerpt.clone(),
        p2_rows,
        insights: defect
            .insights
            .iter()
            .map(|insight| InsightView {
                summary: insight.summary.clone(),
                detail: insight.detail.clone(),
            })
            .collect(),
    })
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
