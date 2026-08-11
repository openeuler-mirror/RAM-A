use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::canonical::{canonical_json, stable_hash};
use crate::episode::parse_time;
use crate::error::{PipelineError, Result};
use crate::models::{AtomicMemory, EvidenceRef, NormalizedMessage};

pub fn attach_source_observations(
    memories: &mut [AtomicMemory],
    messages: &HashMap<String, NormalizedMessage>,
) {
    for memory in memories {
        let mut evidence_indexes = HashMap::new();
        let mut evidence_groups: Vec<(String, Vec<EvidenceRef>)> = Vec::new();
        for evidence in &memory.evidence {
            let index = *evidence_indexes
                .entry(evidence.message_id.clone())
                .or_insert_with(|| {
                    evidence_groups.push((evidence.message_id.clone(), Vec::new()));
                    evidence_groups.len() - 1
                });
            evidence_groups[index].1.push(evidence.clone());
        }
        let mut source_observations = Vec::new();
        for (message_id, evidence_refs) in evidence_groups {
            let Some(message) = messages.get(&message_id) else {
                continue;
            };
            let mut source_observation = observation_with_evidence(memory, &evidence_refs);
            if !message.timestamp.is_empty() {
                source_observation.insert("observed_at".into(), json!(message.timestamp));
            }
            if let Some(speaker) = source_speaker(memory, Some(message)) {
                source_observation.insert("speaker".into(), json!(speaker));
            }
            if !message.session_id.is_empty() {
                source_observation.insert("session_id".into(), json!(message.session_id));
            }
            if let Some(turn_index) = message.turn_index {
                source_observation.insert("turn_index".into(), json!(turn_index));
            }
            source_observations.push(source_observation);
        }
        if source_observations.is_empty() {
            let mut source_observation = observation(memory);
            if let Some(speaker) = source_speaker(memory, None) {
                source_observation.insert("speaker".into(), json!(speaker));
            }
            source_observations.push(source_observation);
        }
        extend_observations(&mut memory.observation_refs, &source_observations);
    }
}

fn source_speaker<'a>(
    memory: &'a AtomicMemory,
    message: Option<&'a NormalizedMessage>,
) -> Option<&'a str> {
    message
        .map(|message| message.speaker.trim())
        .filter(|speaker| !speaker.is_empty())
        .or_else(|| {
            memory
                .subject
                .get("source_speaker")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|speaker| !speaker.is_empty())
        })
}

pub fn aggregate_exact_memories(memories: &[AtomicMemory]) -> Vec<AtomicMemory> {
    let mut output: Vec<AtomicMemory> = Vec::new();
    let mut indexes = HashMap::new();
    for memory in memories {
        let key = canonical_json(&json!({
            "scope_id": memory.scope_id,
            "content": memory.canonical_content(),
        }));
        let index = *indexes.entry(key).or_insert_with(|| {
            let mut value = memory.clone();
            value.evidence.clear();
            value.observation_refs.clear();
            output.push(value);
            output.len() - 1
        });
        extend_evidence(&mut output[index].evidence, &memory.evidence);
        let incoming = if memory.observation_refs.is_empty() {
            vec![observation(memory)]
        } else {
            memory.observation_refs.clone()
        };
        extend_observations(&mut output[index].observation_refs, &incoming);
    }
    for memory in &mut output {
        memory.id = format!(
            "mem-{}",
            stable_hash(&[json!(memory.scope_id), memory.canonical_content()])
        );
        let latest_observation = memory
            .observation_refs
            .iter()
            .max_by(|left, right| {
                compare_observed_at(observation_time(left), observation_time(right))
            })
            .cloned();
        if let Some(observation) = latest_observation {
            memory.observed_at = observation_time(&observation).to_owned();
            memory.source_episode_id = observation_text(&observation, "source_episode_id");
            memory.source_window_id = observation_text(&observation, "source_window_id");
        }
    }
    output
}

fn compare_observed_at(left: &str, right: &str) -> std::cmp::Ordering {
    match (parse_time(left), parse_time(right)) {
        (Some(left_time), Some(right_time)) => {
            left_time.cmp(&right_time).then_with(|| left.cmp(right))
        }
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => left.cmp(right),
    }
}

fn observation_time(observation: &Map<String, Value>) -> &str {
    observation
        .get("observed_at")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn observation_text(observation: &Map<String, Value>, key: &str) -> String {
    observation
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn make_prepared_output(
    source: &Value,
    memories: &[AtomicMemory],
    run_metadata: &Value,
) -> Result<Value> {
    if source.get("schema_version").and_then(Value::as_str) != Some("benchmark-prepared-v1") {
        return Err(PipelineError::InvalidInput(
            "source prepared input must use benchmark-prepared-v1".into(),
        ));
    }
    Ok(json!({
        "schema_version": "benchmark-prepared-v1",
        "dataset": source.get("dataset").cloned().unwrap_or_else(|| json!({})),
        "memory_pipeline": run_metadata,
        "memories": memories.iter().map(memory_record).collect::<Vec<_>>(),
        "queries": source.get("queries").cloned().unwrap_or_else(|| json!([])),
    }))
}

fn observation(memory: &AtomicMemory) -> Map<String, Value> {
    observation_with_evidence(memory, &memory.evidence)
}

fn observation_with_evidence(
    memory: &AtomicMemory,
    evidence: &[EvidenceRef],
) -> Map<String, Value> {
    Map::from_iter([
        ("source_episode_id".into(), json!(memory.source_episode_id)),
        ("source_window_id".into(), json!(memory.source_window_id)),
        ("observed_at".into(), json!(memory.observed_at)),
        (
            "evidence_refs".into(),
            serde_json::to_value(evidence).expect("serializable evidence"),
        ),
    ])
}

fn extend_evidence(target: &mut Vec<EvidenceRef>, incoming: &[EvidenceRef]) {
    let mut seen = target.iter().map(evidence_key).collect::<HashSet<_>>();
    for item in incoming {
        if seen.insert(evidence_key(item)) {
            target.push(item.clone());
        }
    }
}

fn evidence_key(item: &EvidenceRef) -> (String, usize, usize, String) {
    (
        item.message_id.clone(),
        item.start_char,
        item.end_char,
        item.evidence_role.clone(),
    )
}

fn extend_observations(target: &mut Vec<Map<String, Value>>, incoming: &[Map<String, Value>]) {
    let mut seen = target
        .iter()
        .map(|value| canonical_json(&Value::Object(value.clone())))
        .collect::<HashSet<_>>();
    for item in incoming {
        if seen.insert(canonical_json(&Value::Object(item.clone()))) {
            target.push(item.clone());
        }
    }
}

fn memory_record(memory: &AtomicMemory) -> Value {
    let mut metadata = Map::from_iter([
        ("schema_version".into(), json!("atomic_memory_v1")),
        ("memory_kind".into(), json!("extracted_memory")),
        ("memory_type".into(), json!(memory.memory_type)),
        ("scope_id".into(), json!(memory.scope_id)),
        ("subject".into(), Value::Object(memory.subject.clone())),
        ("predicate".into(), json!(memory.predicate)),
        (
            "object".into(),
            memory.object.clone().unwrap_or(Value::Null),
        ),
        ("modality".into(), json!(memory.modality)),
        (
            "event_time".into(),
            memory
                .event_time
                .clone()
                .map(Value::Object)
                .unwrap_or(Value::Null),
        ),
        (
            "attributes".into(),
            Value::Object(memory.attributes.clone()),
        ),
        ("observed_at".into(), json!(memory.observed_at)),
        ("source_episode_id".into(), json!(memory.source_episode_id)),
        ("source_window_id".into(), json!(memory.source_window_id)),
        (
            "evidence_refs".into(),
            serde_json::to_value(&memory.evidence).expect("serializable evidence"),
        ),
        (
            "observation_refs".into(),
            serde_json::to_value(&memory.observation_refs).expect("serializable observations"),
        ),
    ]);
    if let Some(confidence) = &memory.model_confidence {
        metadata.insert("model_confidence".into(), confidence.clone());
    }
    if let Some(observation) = memory
        .observation_refs
        .iter()
        .max_by(|left, right| compare_observed_at(observation_time(left), observation_time(right)))
    {
        for key in ["speaker", "session_id", "turn_index"] {
            if let Some(value) = observation.get(key) {
                metadata.insert(key.into(), value.clone());
            }
        }
    }
    if let Some(observed_at) = parse_time(&memory.observed_at) {
        let timestamp_ms = observed_at.timestamp_millis();
        if timestamp_ms >= 0 {
            metadata.insert("observed_at_ms".into(), json!(timestamp_ms as u64));
        }
    }
    json!({"id": memory.id, "text": memory.text, "metadata": metadata})
}
