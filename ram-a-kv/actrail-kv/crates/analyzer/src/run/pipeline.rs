//! 本文件编排语料加载、上下文投影、模板发现、结构诊断、聚合排名与原子输出。

use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::Path,
};

use actrail_kv_artifacts::{
    AnalysisOptionsSnapshot, AnalysisResult, AnalysisRunSummary, CapturedRequest, ComparisonGroup,
    ContextDefect as ArtifactContextDefect, RequestTemplate as ArtifactTemplate, SkipRecord,
    SourceLocation as ArtifactSource, StableSpan as ArtifactStableSpan,
    TemplateSlot as ArtifactSlot, ANALYSIS_SCHEMA_VERSION,
};
use anyhow::{Context, Result};
use futures_util::stream::Stream;

use crate::{
    cache_metrics::compute_cache_metrics,
    diagnosis::{diagnose_template, DiagnosisOptions},
    discovery::{
        candidate::{CandidateBuilder, CandidateOptions},
        template::{RequestTemplate, TemplateExtractor, TemplateOptions},
    },
    model::{
        corpus::{CorpusLoadLimits, CorpusLoadResult, CorpusLoader, CorpusSkip, ReportSelection},
        projection::{ProjectionLimits, ProjectionSkip, RequestProjector},
    },
    ranking::rank_defects,
    session::{analyze_reports, request_potential_by_request, summarize_prefix_reuse, SessionRow},
};

use super::AnalysisOptions;

pub fn analyze_file(
    input: &Path,
    output: &Path,
    options: AnalysisOptions,
) -> Result<AnalysisResult> {
    analyze_file_with_selection(input, output, options, &ReportSelection::default())
}

pub fn analyze_file_with_selection(
    input: &Path,
    output: &Path,
    options: AnalysisOptions,
    selection: &ReportSelection,
) -> Result<AnalysisResult> {
    ensure_distinct_paths(input, output)?;
    let file = File::open(input)
        .with_context(|| format!("failed to open captured requests {}", input.display()))?;
    let reader = BufReader::new(file);
    let result = if selection.is_all() {
        analyze_reader(reader, options)?
    } else {
        options.validate()?;
        let loaded = corpus_loader(&options)
            .load_selected(reader, selection)
            .context("failed to select captured requests")?;
        analyze_loaded(loaded, options)?
    };
    write_atomic_json(output, &result)?;
    Ok(result)
}

pub fn analyze_reader<R: BufRead>(reader: R, options: AnalysisOptions) -> Result<AnalysisResult> {
    options.validate()?;
    let loaded = corpus_loader(&options)
        .load(reader)
        .context("failed to read captured requests")?;
    analyze_loaded(loaded, options)
}

pub async fn analyze_captures<S, E>(
    records: S,
    output: &Path,
    options: AnalysisOptions,
) -> Result<AnalysisResult>
where
    S: Stream<Item = Result<CapturedRequest, E>>,
    E: std::error::Error + Send + Sync + 'static,
{
    analyze_captures_with_selection(records, output, options, &ReportSelection::default()).await
}

pub async fn analyze_captures_with_selection<S, E>(
    records: S,
    output: &Path,
    options: AnalysisOptions,
    selection: &ReportSelection,
) -> Result<AnalysisResult>
where
    S: Stream<Item = Result<CapturedRequest, E>>,
    E: std::error::Error + Send + Sync + 'static,
{
    options.validate()?;
    let loader = corpus_loader(&options);
    let loaded = if selection.is_all() {
        loader.load_captures(records).await
    } else {
        loader.load_selected_captures(records, selection).await
    }
    .context("failed to scan captured requests")?;
    let result = analyze_loaded(loaded, options)?;
    write_atomic_json(output, &result)?;
    Ok(result)
}

fn corpus_loader(options: &AnalysisOptions) -> CorpusLoader {
    CorpusLoader::new(CorpusLoadLimits {
        max_record_bytes: options.max_payload_bytes,
        max_records: options.max_records,
    })
}

fn analyze_loaded(loaded: CorpusLoadResult, options: AnalysisOptions) -> Result<AnalysisResult> {
    let input_records = loaded.corpus.records.len() + loaded.skipped.len();
    let cache_metrics = compute_cache_metrics(&loaded.corpus.records);
    let mut skips: Vec<SkipRecord> = loaded.skipped.iter().map(corpus_skip).collect();
    let projector = RequestProjector::new(
        ProjectionLimits {
            max_units: options.max_projection_units,
            max_text_unit_bytes: options.max_text_unit_bytes,
        },
        options.comparison_window_seconds,
    );
    let mut sequences = Vec::new();
    let mut sequence_records = Vec::new();
    for record in &loaded.corpus.records {
        match projector.project(record) {
            Ok(sequence) => {
                sequence_records.push(record);
                sequences.push(sequence);
            }
            Err(skip) => skips.push(projection_skip(record.input_line, &skip)),
        }
    }
    let analyzed_records = sequences.len();
    if analyzed_records == 0 {
        anyhow::bail!(
            "input contains no analyzable OpenAI-compatible chat requests ({} records rejected)",
            skips.len()
        );
    }
    let session_rows: Vec<SessionRow<'_>> = sequence_records
        .iter()
        .zip(&sequences)
        .filter_map(|(record, sequence)| {
            record.session_key.as_deref().map(|session_key| SessionRow {
                session_key,
                captured_at: record.captured_at.as_deref(),
                input_line: record.input_line,
                sequence,
            })
        })
        .collect();
    let session_reports = analyze_reports(session_rows);
    let candidate_result = CandidateBuilder::new(CandidateOptions {
        compatibility_threshold: options.template_compatibility_threshold,
        max_dynamic_coverage_ratio: options.max_dynamic_coverage_ratio,
        min_members: options.min_template_members,
        max_candidates_per_request: options.max_candidates_per_request,
    })
    .build(sequences);
    let mut limitations =
        vec!["结构代理：未使用 tokenizer、模型内部 chat template 或真实 KV 命中数据".to_owned()];
    limitations.extend(candidate_result.skipped.iter().map(|skip| {
        format!(
            "candidate group {}/{}/{} skipped for {} members: {}",
            skip.domain.endpoint_key,
            skip.domain.model,
            skip.domain.time_window_key,
            skip.member_count,
            skip.reason
        )
    }));
    let extraction = TemplateExtractor::new(TemplateOptions {
        stable_support_ratio: options.stable_span_support_ratio,
        min_stable_support: options.min_stable_support,
        min_stable_span_bytes: options.min_exact_anchor_bytes,
        max_alignment_cells: options.max_alignment_cells,
        max_total_alignment_cells: options.max_total_alignment_cells,
    })
    .extract(candidate_result.cohorts);
    limitations.extend(extraction.skipped.iter().map(|skip| {
        format!(
            "template cohort {} skipped: {}",
            skip.cohort_id, skip.reason
        )
    }));
    let diagnosis_options = DiagnosisOptions {
        min_stable_support: options.min_stable_support,
        stable_support_rate: options.stable_span_support_ratio,
        fixed_variant_max: options.fixed_variant_max,
        min_blocked_bytes: options.min_blocked_stable_bytes,
        min_anchor_bytes: options.min_exact_anchor_bytes,
    };
    let mut candidates = Vec::new();
    for template in &extraction.templates {
        candidates.extend(diagnose_template(template, &diagnosis_options));
    }
    let ranked = rank_defects(candidates, options.top_k);
    let prefix_reuse = summarize_prefix_reuse(
        &session_reports,
        &request_potential_by_request(&ranked.defects),
    );
    let mut defects_by_template: std::collections::HashMap<String, Vec<&ArtifactContextDefect>> =
        std::collections::HashMap::new();
    for defect in &ranked.defects {
        defects_by_template
            .entry(defect.template_id.clone())
            .or_default()
            .push(defect);
    }
    let templates = extraction
        .templates
        .iter()
        .map(|template| {
            template_to_artifact(
                template,
                defects_by_template
                    .get(&template.id)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                &options,
            )
        })
        .collect();
    skips.sort_by(|left, right| {
        left.record_index
            .cmp(&right.record_index)
            .then_with(|| left.reason.cmp(&right.reason))
    });
    Ok(AnalysisResult {
        run: AnalysisRunSummary {
            schema_version: ANALYSIS_SCHEMA_VERSION.to_owned(),
            input_records,
            analyzed_records,
            skipped_records: skips,
            options: options_snapshot(&options),
            limitations,
        },
        templates,
        defects: ranked.defects,
        top_k: ranked.top_k,
        session_reports,
        cache_metrics,
        prefix_reuse,
    })
}

fn template_to_artifact(
    template: &RequestTemplate,
    template_defects: &[&ArtifactContextDefect],
    options: &AnalysisOptions,
) -> ArtifactTemplate {
    let mut member_request_ids: Vec<_> = template
        .members
        .iter()
        .map(|member| member.request_id.clone())
        .collect();
    member_request_ids.sort();
    ArtifactTemplate {
        id: template.id.clone(),
        comparison_group: comparison_group(&template.domain),
        member_request_ids,
        medoid_request_id: template.medoid_request_id.clone(),
        cohesion: template.cohesion,
        stable_spans: template
            .stable_spans
            .iter()
            .map(|span| ArtifactStableSpan {
                source: source_to_artifact(
                    &span.source.json_path,
                    Some(span.unit_index),
                    Some((span.medoid_utf8_bytes.start, span.medoid_utf8_bytes.end)),
                    span.source.role.clone(),
                ),
                utf8_bytes: span.medoid_utf8_bytes.len(),
                support_count: span.support_count,
                excerpt: Some(abbreviate(&span.content, 160)),
            })
            .collect(),
        slots: template
            .slots
            .iter()
            .map(|slot| ArtifactSlot {
                source: source_to_artifact(
                    &slot.source.json_path,
                    Some(slot.unit_index),
                    Some((slot.medoid_utf8_bytes.start, slot.medoid_utf8_bytes.end)),
                    slot.source.role.clone(),
                ),
                member_count: slot.support_count,
                distinct_variant_count: slot.distinct_variant_count,
                confidence: template.cohesion,
            })
            .collect(),
        prefix_view: crate::prefix_view::build_prefix_view(
            template,
            template_defects,
            options.prefix_view_max_nodes,
            options.prefix_view_excerpt_bytes,
        ),
    }
}

fn source_to_artifact(
    json_path: &str,
    unit_index: Option<usize>,
    byte_range: Option<(usize, usize)>,
    role: Option<String>,
) -> ArtifactSource {
    ArtifactSource {
        json_path: json_path.to_owned(),
        logical_scope: role.iter().cloned().collect(),
        unit_index,
        byte_start: byte_range.map(|range| range.0),
        byte_end: byte_range.map(|range| range.1),
        role,
    }
}

fn comparison_group(domain: &crate::model::projection::ComparisonDomain) -> ComparisonGroup {
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

fn options_snapshot(options: &AnalysisOptions) -> AnalysisOptionsSnapshot {
    AnalysisOptionsSnapshot {
        top_k: options.top_k,
        comparison_window_seconds: options.comparison_window_seconds,
        min_template_members: options.min_template_members,
        stable_span_support_ratio: options.stable_span_support_ratio,
        min_stable_support: options.min_stable_support,
        fixed_variant_max: options.fixed_variant_max,
        min_blocked_stable_bytes: options.min_blocked_stable_bytes,
        min_exact_anchor_bytes: options.min_exact_anchor_bytes,
        text_similarity_threshold: options.text_similarity_threshold,
        template_compatibility_threshold: options.template_compatibility_threshold,
        max_dynamic_coverage_ratio: options.max_dynamic_coverage_ratio,
        max_candidates_per_request: options.max_candidates_per_request,
        max_projection_units: options.max_projection_units,
        max_text_unit_bytes: options.max_text_unit_bytes,
        max_payload_bytes: options.max_payload_bytes,
        max_alignment_cells: options.max_alignment_cells,
        max_total_alignment_cells: options.max_total_alignment_cells,
        max_records: options.max_records,
        prefix_view_max_nodes: options.prefix_view_max_nodes,
        prefix_view_excerpt_bytes: options.prefix_view_excerpt_bytes,
    }
}

fn corpus_skip(skip: &CorpusSkip) -> SkipRecord {
    SkipRecord {
        record_index: skip.input_line,
        reason: format!("corpus: {:?}", skip.reason),
    }
}

fn projection_skip(line: usize, skip: &ProjectionSkip) -> SkipRecord {
    SkipRecord {
        record_index: line,
        reason: format!("projection: {:?}", skip.reason),
    }
}

fn abbreviate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn write_atomic_json(path: &Path, result: &AnalysisResult) -> Result<()> {
    let parent = output_directory(path);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, result)?;
    temporary.write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path)?;
    sync_parent(parent)?;
    Ok(())
}

fn ensure_distinct_paths(input: &Path, output: &Path) -> Result<()> {
    if output.exists() && same_file::is_same_file(input, output).unwrap_or(false) {
        anyhow::bail!("input and output must refer to different files");
    }
    if !output.exists() {
        let input = input.canonicalize()?;
        let parent = output_directory(output).canonicalize()?;
        let destination = match output.file_name() {
            Some(name) => parent.join(name),
            None => parent,
        };
        if input == destination {
            anyhow::bail!("input and output must refer to different files");
        }
    }
    Ok(())
}

fn output_directory(output: &Path) -> &Path {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<()> {
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<()> {
    Ok(())
}
