use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::cache::JsonCache;
use crate::canonical::stable_hash;
use crate::episode::{build_episodes, EpisodeConfig};
use crate::error::{PipelineError, Result};
use crate::extraction::{component_identity, ExtractionBatch, MemoryExtractor, SCHEMA_VERSION};
use crate::grounding::{GroundingBatch, GroundingVerifier};
use crate::models::{
    AtomicMemory, ConversationEpisode, ExtractionWindow, NormalizedMessage, PipelineIssue,
};
use crate::normalize::{normalize_prepared_memories, NORMALIZER_VERSION};
use crate::validation::{validate_extraction, ValidationConfig};
use crate::window::{build_windows, WindowConfig};
use crate::writer::{aggregate_exact_memories, make_prepared_output};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PipelineConfig {
    pub episode: EpisodeConfig,
    pub window: WindowConfig,
    pub validation: ValidationConfig,
    pub pipeline_version: String,
    pub fail_fast: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            episode: EpisodeConfig::default(),
            window: WindowConfig::default(),
            validation: ValidationConfig::default(),
            pipeline_version: "memory_pipeline_v1".into(),
            fail_fast: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PipelineRun {
    pub prepared: Value,
    pub normalized_messages: Vec<NormalizedMessage>,
    pub episodes: Vec<ConversationEpisode>,
    pub windows: Vec<ExtractionWindow>,
    pub extracted_candidates: Vec<ExtractionBatch>,
    pub accepted_memories: Vec<AtomicMemory>,
    pub rejected: Vec<PipelineIssue>,
    pub quarantined: Vec<PipelineIssue>,
    pub stats: Value,
    pub run_metadata: Value,
}

pub async fn run_memory_pipeline<E: MemoryExtractor + ?Sized, V: GroundingVerifier + ?Sized>(
    prepared: &Value,
    config: &PipelineConfig,
    extractor: &E,
    verifier: &V,
    cache: Option<&JsonCache>,
) -> Result<PipelineRun> {
    if config.validation.max_memory_chars == 0 {
        return Err(PipelineError::InvalidInput(
            "max_memory_chars must be positive".into(),
        ));
    }
    let (messages, normalization_issues) = normalize_prepared_memories(prepared)?;
    let lookup = messages
        .iter()
        .cloned()
        .map(|message| (message.id.clone(), message))
        .collect::<HashMap<_, _>>();
    let episodes = build_episodes(&messages, &config.episode)?;
    let windows = build_windows(&episodes, &lookup, &config.window)?;
    let mut extracted = Vec::new();
    let mut supported = Vec::new();
    let mut rejected = normalization_issues;
    let mut quarantined = Vec::new();
    let mut empty_windows = 0usize;
    let mut extraction_calls = 0usize;
    let mut verification_calls = 0usize;
    let mut extraction_cache_hits = 0usize;
    let mut verification_cache_hits = 0usize;
    let mut extraction_tokens = 0i64;
    let mut verification_tokens = 0i64;
    let mut extraction_latency = 0.0;
    let mut verification_latency = 0.0;
    let mut grounding_counts: HashMap<String, usize> = HashMap::new();

    for window in &windows {
        let extraction_result = extract(window, &lookup, extractor, cache).await;
        let (batch, cached) = match extraction_result {
            Ok(value) => value,
            Err(error) if config.fail_fast => return Err(error),
            Err(error) => {
                rejected.push(runtime_issue("extract", window, &error, ""));
                continue;
            }
        };
        if cached {
            extraction_cache_hits += 1
        } else {
            extraction_calls += 1;
            extraction_tokens += batch.usage.total_tokens;
            extraction_latency += batch.usage.latency_ms;
        }
        extracted.push(batch.clone());
        if batch.raw_memories.is_empty() {
            empty_windows += 1;
            continue;
        }
        let validation =
            validate_extraction(&batch.raw_memories, window, &lookup, &config.validation);
        rejected.extend(validation.rejected);
        quarantined.extend(validation.quarantined);
        if validation.valid.is_empty() {
            continue;
        }
        let grounding_result = verify(window, &validation.valid, &lookup, verifier, cache).await;
        let (grounding, cached) = match grounding_result {
            Ok(value) => value,
            Err(error) if config.fail_fast => return Err(error),
            Err(error) => {
                quarantined.extend(
                    validation
                        .valid
                        .iter()
                        .map(|memory| runtime_issue("grounding", window, &error, &memory.id)),
                );
                continue;
            }
        };
        if cached {
            verification_cache_hits += 1
        } else {
            verification_calls += 1;
            verification_tokens += grounding.usage.total_tokens;
            verification_latency += grounding.usage.latency_ms;
        }
        let results = grounding
            .results
            .into_iter()
            .map(|result| (result.memory_id.clone(), result))
            .collect::<HashMap<_, _>>();
        for memory in validation.valid {
            let result = results.get(&memory.id).ok_or_else(|| {
                PipelineError::Protocol(format!("verifier omitted memory {}", memory.id))
            })?;
            *grounding_counts.entry(result.status.clone()).or_default() += 1;
            if result.status == "SUPPORTED" {
                supported.push(memory)
            } else {
                quarantined.push(PipelineIssue {
                    stage: "grounding".into(),
                    code: format!("grounding_{}", result.status.to_lowercase()),
                    message: if result.reason.is_empty() {
                        format!("grounding status is {}", result.status)
                    } else {
                        result.reason.clone()
                    },
                    source_id: memory.id,
                    scope_id: memory.scope_id,
                    episode_id: memory.source_episode_id,
                    window_id: memory.source_window_id,
                    details: Map::from_iter([("status".into(), json!(result.status))]),
                });
            }
        }
    }
    let accepted = aggregate_exact_memories(&supported);
    let (coverage, duplication) = candidate_span_metrics(&messages, &windows);
    let source_memory_counts = source_counts(&lookup, &accepted, true);
    let source_evidence_counts = source_counts(&lookup, &accepted, false);
    let run_metadata = json!({
        "pipeline_version": config.pipeline_version,
        "dataset": prepared.get("dataset").cloned().unwrap_or_else(|| json!({})),
        "source_hash": stable_hash(std::slice::from_ref(prepared)),
        "normalizer_version": NORMALIZER_VERSION,
        "config": config,
        "extractor": {
            "model": extractor.model(), "prompt_version": extractor.prompt_version(),
            "schema_version": SCHEMA_VERSION, "implementation": extractor.implementation()
        },
        "verifier": {
            "model": verifier.model(), "prompt_version": verifier.prompt_version(),
            "implementation": verifier.implementation()
        },
        "cache_version": cache.map(|value| value.version.clone()),
    });
    let stats = json!({
        "normalized_message_count": messages.len(), "episode_count": episodes.len(),
        "window_count": windows.len(), "empty_extraction_windows": empty_windows,
        "raw_candidate_count": extracted.iter().map(|batch| batch.raw_memories.len()).sum::<usize>(),
        "accepted_memory_count": accepted.len(), "rejected_count": rejected.len(),
        "quarantined_count": quarantined.len(), "candidate_source_coverage": coverage,
        "candidate_source_duplication": duplication, "extraction_total_tokens": extraction_tokens,
        "verification_total_tokens": verification_tokens, "extraction_cache_hits": extraction_cache_hits,
        "verification_cache_hits": verification_cache_hits, "extraction_call_count": extraction_calls,
        "verification_call_count": verification_calls, "extraction_latency_ms": extraction_latency,
        "verification_latency_ms": verification_latency, "grounding_status_counts": grounding_counts,
        "source_turn_memory_counts": source_memory_counts,
        "source_turn_evidence_ref_counts": source_evidence_counts,
    });
    let output = make_prepared_output(prepared, &accepted, &run_metadata)?;
    Ok(PipelineRun {
        prepared: output,
        normalized_messages: messages,
        episodes,
        windows,
        extracted_candidates: extracted,
        accepted_memories: accepted,
        rejected,
        quarantined,
        stats,
        run_metadata,
    })
}

async fn extract<E: MemoryExtractor + ?Sized>(
    window: &ExtractionWindow,
    messages: &HashMap<String, NormalizedMessage>,
    extractor: &E,
    cache: Option<&JsonCache>,
) -> Result<(ExtractionBatch, bool)> {
    let key = vec![
        serde_json::to_value(window)?,
        serde_json::to_value(window_messages(window, messages))?,
        Value::Object(component_identity(extractor)),
        json!(SCHEMA_VERSION),
    ];
    if let Some(cache) = cache {
        if let Some(value) = cache.get("extraction", &key)? {
            return Ok((serde_json::from_value(value)?, true));
        }
    }
    let batch = extractor.extract(window, messages).await?;
    if let Some(cache) = cache {
        cache.put("extraction", &key, &serde_json::to_value(&batch)?)?;
    }
    Ok((batch, false))
}

async fn verify<V: GroundingVerifier + ?Sized>(
    window: &ExtractionWindow,
    memories: &[AtomicMemory],
    messages: &HashMap<String, NormalizedMessage>,
    verifier: &V,
    cache: Option<&JsonCache>,
) -> Result<(GroundingBatch, bool)> {
    let key = vec![
        json!(window.id),
        serde_json::to_value(memories)?,
        serde_json::to_value(evidence_messages(memories, messages))?,
        Value::Object(verifier_identity(verifier)),
    ];
    if let Some(cache) = cache {
        if let Some(value) = cache.get("grounding", &key)? {
            return Ok((serde_json::from_value(value)?, true));
        }
    }
    let batch = verifier.verify(window, memories, messages).await?;
    if let Some(cache) = cache {
        cache.put("grounding", &key, &serde_json::to_value(&batch)?)?;
    }
    Ok((batch, false))
}

fn window_messages<'a>(
    window: &ExtractionWindow,
    messages: &'a HashMap<String, NormalizedMessage>,
) -> Vec<&'a NormalizedMessage> {
    let mut seen = std::collections::HashSet::new();
    window
        .context_before_refs
        .iter()
        .chain(window.candidate_refs.iter())
        .chain(window.context_after_refs.iter())
        .filter_map(|reference| {
            seen.insert(reference.message_id.clone())
                .then(|| messages.get(&reference.message_id))
                .flatten()
        })
        .collect()
}

fn evidence_messages<'a>(
    memories: &[AtomicMemory],
    messages: &'a HashMap<String, NormalizedMessage>,
) -> Vec<&'a NormalizedMessage> {
    let mut seen = std::collections::HashSet::new();
    memories
        .iter()
        .flat_map(|memory| memory.evidence.iter())
        .filter_map(|evidence| {
            seen.insert(evidence.message_id.clone())
                .then(|| messages.get(&evidence.message_id))
                .flatten()
        })
        .collect()
}

fn verifier_identity(verifier: &(impl GroundingVerifier + ?Sized)) -> Map<String, Value> {
    let mut identity = Map::from_iter([
        ("implementation".into(), json!(verifier.implementation())),
        ("model".into(), json!(verifier.model())),
        ("prompt_version".into(), json!(verifier.prompt_version())),
    ]);
    if let Some(tokens) = verifier.max_output_tokens() {
        identity.insert("max_output_tokens".into(), json!(tokens));
    }
    identity
}

pub fn write_pipeline_artifacts(run: &PipelineRun, root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    write_jsonl(
        root.join("normalized_messages.jsonl"),
        &run.normalized_messages,
    )?;
    write_jsonl(root.join("episodes.jsonl"), &run.episodes)?;
    write_jsonl(root.join("extraction_windows.jsonl"), &run.windows)?;
    write_jsonl(
        root.join("extracted_candidates.jsonl"),
        &run.extracted_candidates,
    )?;
    write_jsonl(root.join("accepted_memories.jsonl"), &run.accepted_memories)?;
    write_jsonl(root.join("rejected_extractions.jsonl"), &run.rejected)?;
    write_jsonl(root.join("quarantined_memories.jsonl"), &run.quarantined)?;
    write_json(root.join("extraction_stats.json"), &run.stats)?;
    write_json(root.join("run_metadata.json"), &run.run_metadata)?;
    write_json(root.join("prepared.json"), &run.prepared)?;
    Ok(())
}

fn write_jsonl<T: Serialize>(path: impl AsRef<Path>, values: &[T]) -> Result<()> {
    let mut content = String::new();
    for value in values {
        content.push_str(&serde_json::to_string(value)?);
        content.push('\n');
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn write_json(path: impl AsRef<Path>, value: &Value) -> Result<()> {
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

fn runtime_issue(
    stage: &str,
    window: &ExtractionWindow,
    error: &PipelineError,
    source_id: &str,
) -> PipelineIssue {
    PipelineIssue {
        stage: stage.into(),
        code: format!("{stage}_runtime_error"),
        message: error.to_string(),
        source_id: source_id.into(),
        scope_id: window.scope_id.clone(),
        episode_id: window.episode_id.clone(),
        window_id: window.id.clone(),
        details: Map::from_iter([("error_type".into(), json!("PipelineError"))]),
    }
}

fn candidate_span_metrics(
    messages: &[NormalizedMessage],
    windows: &[ExtractionWindow],
) -> (f64, usize) {
    let mut spans = messages
        .iter()
        .filter(|message| message.candidate_eligible)
        .map(|message| (message.id.clone(), Vec::new()))
        .collect::<HashMap<String, Vec<(usize, usize)>>>();
    let total = messages
        .iter()
        .filter(|message| message.candidate_eligible)
        .map(|message| message.text.chars().count())
        .sum::<usize>();
    let mut raw = 0;
    for window in windows {
        for reference in &window.candidate_refs {
            if let Some(message_spans) = spans.get_mut(&reference.message_id) {
                message_spans.push((reference.start_char, reference.end_char));
                raw += reference.end_char.saturating_sub(reference.start_char);
            }
        }
    }
    let covered = spans
        .values()
        .map(|value| union_length(value))
        .sum::<usize>();
    (
        if total == 0 {
            1.0
        } else {
            covered as f64 / total as f64
        },
        raw.saturating_sub(covered),
    )
}

fn union_length(spans: &[(usize, usize)]) -> usize {
    if spans.is_empty() {
        return 0;
    }
    let mut spans = spans.to_vec();
    spans.sort_unstable();
    let (mut start, mut end) = spans[0];
    let mut total = 0;
    for (next_start, next_end) in spans.into_iter().skip(1) {
        if next_start <= end {
            end = end.max(next_end);
        } else {
            total += end.saturating_sub(start);
            start = next_start;
            end = next_end;
        }
    }
    total + end.saturating_sub(start)
}

fn source_counts(
    messages: &HashMap<String, NormalizedMessage>,
    memories: &[AtomicMemory],
    unique_memory: bool,
) -> HashMap<String, usize> {
    let mut counts = messages
        .keys()
        .map(|id| (id.clone(), 0))
        .collect::<HashMap<_, _>>();
    for memory in memories {
        if unique_memory {
            for id in memory
                .evidence
                .iter()
                .map(|item| &item.message_id)
                .collect::<std::collections::HashSet<_>>()
            {
                *counts.entry(id.clone()).or_default() += 1;
            }
        } else {
            for evidence in &memory.evidence {
                *counts.entry(evidence.message_id.clone()).or_default() += 1;
            }
        }
    }
    counts
}
