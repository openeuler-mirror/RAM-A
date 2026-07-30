use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::canonical::{canonical_json, stable_hash};
use crate::episode::parse_time;
use crate::error::{PipelineError, Result};
use crate::models::{AtomicMemory, EvidenceRef};

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
        memory.observed_at = memory
            .observation_refs
            .iter()
            .filter_map(|value| value.get("observed_at").and_then(Value::as_str))
            .max_by(|left, right| compare_observed_at(left, right))
            .unwrap_or("")
            .to_owned();
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
    Map::from_iter([
        ("source_episode_id".into(), json!(memory.source_episode_id)),
        ("source_window_id".into(), json!(memory.source_window_id)),
        ("observed_at".into(), json!(memory.observed_at)),
        (
            "evidence_refs".into(),
            serde_json::to_value(&memory.evidence).expect("serializable evidence"),
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
    json!({"id": memory.id, "text": memory.text, "metadata": metadata})
}
