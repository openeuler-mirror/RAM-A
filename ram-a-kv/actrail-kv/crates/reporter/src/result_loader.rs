//! 本文件严格加载并校验统一 P1/X/P2 analysis.json 的引用与数值契约。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::BufReader,
    path::Path,
};

use actrail_kv_artifacts::{
    AnalysisResult, ContextDefect, SourceLocation, ANALYSIS_SCHEMA_VERSION,
};
use anyhow::{bail, Context, Result};

pub fn load_result(path: &Path) -> Result<AnalysisResult> {
    let file = File::open(path)
        .with_context(|| format!("failed to open analysis result {}", path.display()))?;
    let result: AnalysisResult = serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("invalid analysis result {}", path.display()))?;
    validate(&result)?;
    Ok(result)
}

fn validate(result: &AnalysisResult) -> Result<()> {
    if result.run.schema_version != ANALYSIS_SCHEMA_VERSION {
        bail!(
            "unsupported analysis schema version {}",
            result.run.schema_version
        );
    }
    let templates: BTreeMap<_, _> = result
        .templates
        .iter()
        .map(|template| (&template.id, template))
        .collect();
    if templates.len() != result.templates.len() {
        bail!("analysis contains duplicate template IDs");
    }
    for template in &result.templates {
        validate_group(&template.comparison_group)?;
        if !is_probability(template.cohesion) {
            bail!("template cohesion must be a finite probability");
        }
        let members: BTreeSet<_> = template.member_request_ids.iter().collect();
        if members.len() != template.member_request_ids.len() {
            bail!("template contains duplicate member IDs");
        }
        if !members.contains(&template.medoid_request_id) {
            bail!("template medoid is not one of its members");
        }
        if let Some(prefix_view) = &template.prefix_view {
            validate_prefix_view(prefix_view)?;
        }
    }
    let defects: BTreeMap<_, _> = result
        .defects
        .iter()
        .map(|defect| (&defect.id, defect))
        .collect();
    if defects.len() != result.defects.len() {
        bail!("analysis contains duplicate defect IDs");
    }
    for defect in &result.defects {
        let template = templates
            .get(&defect.template_id)
            .ok_or_else(|| anyhow::anyhow!("defect references an unknown template"))?;
        if template.comparison_group != defect.comparison_group {
            bail!("defect and template comparison groups do not match");
        }
        validate_defect(defect, &template.member_request_ids)?;
    }
    if result.top_k.len() > result.run.options.top_k {
        bail!("top_k exceeds the configured limit");
    }
    let mut seen = BTreeSet::new();
    for (index, id) in result.top_k.iter().enumerate() {
        if !seen.insert(id) {
            bail!("top_k contains duplicate defect references");
        }
        if !defects.contains_key(id) {
            bail!("top_k references an unknown defect");
        }
        if result.defects.get(index).map(|defect| &defect.id) != Some(id) {
            bail!("top_k must be a prefix of the deterministically sorted defects");
        }
    }
    validate_sessions(&result.session_reports)?;
    Ok(())
}

fn validate_prefix_view(view: &actrail_kv_artifacts::TemplatePrefixView) -> Result<()> {
    if view.node_count != view.nodes.len() {
        bail!("prefix view node_count does not match nodes");
    }
    let by_id: BTreeMap<_, _> = view
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    if by_id.len() != view.nodes.len() {
        bail!("prefix view contains duplicate node IDs");
    }
    if view.nodes.is_empty() {
        return Ok(());
    }
    let mut current = by_id
        .get("seq-0")
        .copied()
        .ok_or_else(|| anyhow::anyhow!("prefix view is missing the seq-0 root node"))?;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.id.as_str()) {
            bail!("prefix view sequence contains a cycle at {}", current.id);
        }
        let Some(child) = current.sequence_child.as_deref() else {
            break;
        };
        current = by_id
            .get(child)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("prefix view references unknown node {child}"))?;
    }
    if visited.len() != view.nodes.len() {
        bail!("prefix view sequence does not include every node");
    }
    Ok(())
}

fn validate_sessions(reports: &[actrail_kv_artifacts::SessionReport]) -> Result<()> {
    for report in reports {
        if report.request_count != report.events.len().saturating_add(1) {
            bail!("session request_count does not match event count");
        }
        let modern = report
            .events
            .iter()
            .any(|event| event.prefix_reuse_ratio.is_some());
        if modern {
            let refresh_count = report
                .events
                .iter()
                .filter(|event| event.prefix_reuse_outlier)
                .count();
            if report.context_refresh_count != refresh_count {
                bail!("session refresh count does not match refresh events");
            }
            let abandoned: usize = report
                .events
                .iter()
                .map(|event| event.abandoned_prefix_bytes)
                .sum();
            if report.total_abandoned_prefix_bytes != abandoned {
                bail!("session abandoned prefix total is inconsistent");
            }
        }
        for event in &report.events {
            if event.abandoned_prefix_bytes > event.previous_total_bytes {
                bail!("session abandoned prefix exceeds previous context");
            }
            let Some(ratio) = event.prefix_reuse_ratio else {
                continue;
            };
            if event.previous_total_bytes == 0 || !is_probability(ratio) {
                bail!("session prefix reuse ratio must be a finite probability");
            }
            if event.retained_prefix_bytes > event.previous_total_bytes
                || event.retained_prefix_bytes + event.abandoned_prefix_bytes
                    != event.previous_total_bytes
            {
                bail!("session retained and abandoned prefix bytes are inconsistent");
            }
            let retained = event.retained_prefix_bytes;
            let expected = retained as f64 / event.previous_total_bytes as f64;
            let tolerance = f64::EPSILON * expected.abs().max(1.0) * 8.0;
            if (ratio - expected).abs() > tolerance {
                bail!("session prefix reuse ratio is inconsistent with byte evidence");
            }
            if event.prefix_reuse_outlier != (event.abandoned_prefix_bytes > 0) {
                bail!("session refresh flag is inconsistent with abandoned prefix");
            }
        }
    }
    Ok(())
}

fn validate_defect(defect: &ContextDefect, template_members: &[String]) -> Result<()> {
    validate_group(&defect.comparison_group)?;
    if defect.mismatch.variants.len() < 2 {
        bail!("defect mismatch must contain at least two variants");
    }
    let mut variant_ids = BTreeSet::new();
    let mut members = BTreeSet::new();
    let mut largest_variant = 0usize;
    for variant in &defect.mismatch.variants {
        if variant.member_request_ids.is_empty() {
            bail!("defect variant must contain at least one member");
        }
        if !variant_ids.insert(&variant.fingerprint) {
            bail!("defect contains duplicate variant fingerprints");
        }
        if !variant
            .member_request_ids
            .iter()
            .any(|id| id == &variant.representative.request_id)
        {
            bail!("variant representative is not a member of its variant");
        }
        for member in &variant.member_request_ids {
            if !members.insert(member) {
                bail!("a comparable member appears in more than one variant");
            }
        }
        largest_variant = largest_variant.max(variant.member_request_ids.len());
        for source in &variant.representative.sources {
            validate_source(source)?;
        }
        if variant.representative.sources.is_empty()
            && (variant.representative.utf8_bytes != 0 || variant.representative.excerpt.is_some())
        {
            bail!("a source-less missing variant must have zero bytes and no excerpt");
        }
    }
    let template_members: BTreeSet<_> = template_members.iter().collect();
    if !members.is_subset(&template_members) {
        bail!("defect variant contains a request outside its template");
    }
    if members.len() != defect.comparable_count {
        bail!("variant members do not match comparable_count");
    }
    if defect.recovered_stable.support_count != defect.comparable_count {
        bail!("recovered stable support does not match comparable_count");
    }
    if defect.recovered_stable.sources.is_empty() {
        bail!("recovered stable evidence must contain a source");
    }
    if defect.affected_count != defect.comparable_count.saturating_sub(largest_variant) {
        bail!("affected_count does not equal comparable count minus largest variant");
    }
    if defect.affected_count == 0 || defect.insights.is_empty() {
        bail!("a defect must affect requests and provide an optimization insight");
    }
    if defect.potential_prefix_bytes < defect.actual_prefix_bytes {
        bail!("potential prefix is shorter than actual prefix");
    }
    if defect.blocked_stable_bytes > defect.recovered_stable.utf8_bytes {
        bail!("blocked bytes exceed recovered stable bytes");
    }
    if !is_probability(defect.confidence)
        || !is_probability(defect.score.confidence)
        || defect.score.confidence != defect.confidence
    {
        bail!("invalid or inconsistent defect confidence");
    }
    if defect.score.blocked_stable_bytes != defect.blocked_stable_bytes
        || defect.score.affected_count != defect.affected_count
    {
        bail!("score factors do not match defect metrics");
    }
    let expected =
        defect.blocked_stable_bytes as f64 * defect.affected_count as f64 * defect.confidence;
    let tolerance = f64::EPSILON * expected.abs().max(1.0) * 8.0;
    if !defect.score.score.is_finite() || (defect.score.score - expected).abs() > tolerance {
        bail!("score does not match its declared factors");
    }
    for source in &defect.recovered_stable.sources {
        validate_source(source)?;
    }
    Ok(())
}

fn validate_source(source: &SourceLocation) -> Result<()> {
    if source.json_path.is_empty() {
        bail!("source JSON path must not be empty");
    }
    match (source.byte_start, source.byte_end) {
        (Some(start), Some(end)) if start <= end => Ok(()),
        (None, None) => Ok(()),
        _ => bail!("source byte offsets must be a valid start/end pair"),
    }
}

fn validate_group(group: &actrail_kv_artifacts::ComparisonGroup) -> Result<()> {
    if group.time_window_key.is_empty()
        || group.endpoint_key.is_empty()
        || group.model.is_empty()
        || group.context_schema_key.is_empty()
    {
        bail!("comparison group required keys must not be empty");
    }
    Ok(())
}

fn is_probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

#[cfg(test)]
#[path = "../../../tests/unit/reporter/result_loader.rs"]
pub(crate) mod tests;
