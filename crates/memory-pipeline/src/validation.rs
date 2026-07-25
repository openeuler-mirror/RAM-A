use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::canonical::stable_hash;
use crate::models::{
    AtomicMemory, EvidenceRef, ExtractionWindow, MessageRef, NormalizedMessage, PipelineIssue,
};

const MEMORY_TYPES: [&str; 7] = [
    "fact",
    "preference",
    "relationship",
    "event",
    "state",
    "procedure",
    "other",
];
const MODALITIES: [&str; 6] = [
    "asserted",
    "negated",
    "possible",
    "planned",
    "conditional",
    "reported",
];
const EVIDENCE_ROLES: [&str; 2] = ["primary", "supporting"];

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ValidationConfig {
    pub max_memory_chars: usize,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            max_memory_chars: 500,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValidationBatch {
    pub valid: Vec<AtomicMemory>,
    pub rejected: Vec<PipelineIssue>,
    pub quarantined: Vec<PipelineIssue>,
}

pub fn validate_extraction(
    raw_memories: &[Value],
    window: &ExtractionWindow,
    messages_by_id: &HashMap<String, NormalizedMessage>,
    config: &ValidationConfig,
) -> ValidationBatch {
    let mut batch = ValidationBatch::default();
    for (index, raw) in raw_memories.iter().enumerate() {
        let Some(object) = valid_schema(raw) else {
            batch.rejected.push(issue(
                window,
                index,
                "malformed_schema",
                "memory fields do not match atomic_memory_v1",
            ));
            continue;
        };
        let memory_type = object["memory_type"].as_str().unwrap();
        let modality = object["modality"].as_str().unwrap();
        if !MEMORY_TYPES.contains(&memory_type) || !MODALITIES.contains(&modality) {
            batch.rejected.push(issue(
                window,
                index,
                "unknown_enum",
                "memory_type or modality is not allowed",
            ));
            continue;
        }
        let (evidence, evidence_issue, rejected) = validate_evidence(
            object["evidence"].as_array().unwrap(),
            window,
            messages_by_id,
            index,
        );
        if let Some(problem) = evidence_issue {
            if rejected {
                batch.rejected.push(problem)
            } else {
                batch.quarantined.push(problem)
            }
            continue;
        }
        if !evidence.iter().any(|item| {
            item.evidence_role == "primary" && is_candidate(item, &window.candidate_refs)
        }) {
            batch.rejected.push(issue(
                window,
                index,
                "missing_candidate_evidence",
                "memory requires primary evidence from a candidate span",
            ));
            continue;
        }
        let memory_text = object["text"].as_str().unwrap().trim();
        if memory_text.chars().count() > config.max_memory_chars {
            batch.quarantined.push(issue(
                window,
                index,
                "memory_text_too_long",
                &format!("memory text exceeds {} characters", config.max_memory_chars),
            ));
            continue;
        }
        if suspicious_modality(modality, &evidence) {
            batch.quarantined.push(issue(
                window,
                index,
                "suspicious_modality",
                "memory modality is inconsistent with its evidence",
            ));
            continue;
        }
        let subject = object["subject"].as_object().unwrap().clone();
        let event_time = object
            .get("event_time")
            .and_then(Value::as_object)
            .filter(|value| !value.is_empty())
            .cloned();
        let attributes = object
            .get("attributes")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let canonical = json!({
            "memory_type": memory_type,
            "text": memory_text,
            "subject": subject,
            "predicate": object["predicate"].as_str().unwrap().trim(),
            "object": object.get("object").cloned().unwrap_or(Value::Null),
            "modality": modality,
            "event_time": event_time,
            "attributes": attributes,
        });
        let observed_at = observed_at(&evidence, messages_by_id, window);
        batch.valid.push(AtomicMemory {
            id: format!(
                "candidate-{}",
                stable_hash(&[json!(window.scope_id), canonical.clone()])
            ),
            scope_id: window.scope_id.clone(),
            text: memory_text.into(),
            memory_type: memory_type.into(),
            subject,
            predicate: object["predicate"].as_str().unwrap().trim().into(),
            object: object
                .get("object")
                .filter(|value| !value.is_null())
                .cloned(),
            modality: modality.into(),
            evidence,
            event_time,
            attributes,
            model_confidence: object.get("model_confidence").cloned(),
            observed_at,
            source_episode_id: window.episode_id.clone(),
            source_window_id: window.id.clone(),
            observation_refs: Vec::new(),
        });
    }
    batch
}

fn valid_schema(raw: &Value) -> Option<&Map<String, Value>> {
    let object = raw.as_object()?;
    let text = object.get("text")?.as_str()?;
    let predicate = object.get("predicate")?.as_str()?;
    if text.trim().is_empty() || predicate.trim().is_empty() || !object.get("subject")?.is_object()
    {
        return None;
    }
    let object_value = object.get("object").unwrap_or(&Value::Null);
    if !(object_value.is_null() || object_value.is_string() || object_value.is_object()) {
        return None;
    }
    if object
        .get("evidence")?
        .as_array()
        .is_none_or(|items| items.is_empty())
    {
        return None;
    }
    if !object.get("attributes").unwrap_or(&json!({})).is_object() {
        return None;
    }
    if object
        .get("event_time")
        .is_some_and(|value| !(value.is_null() || value.is_object()))
    {
        return None;
    }
    if let Some(confidence) = object.get("model_confidence") {
        let value = confidence.as_f64()?;
        if !(0.0..=1.0).contains(&value) {
            return None;
        }
    }
    object.get("memory_type")?.as_str()?;
    object.get("modality")?.as_str()?;
    Some(object)
}

fn validate_evidence(
    raw: &[Value],
    window: &ExtractionWindow,
    messages: &HashMap<String, NormalizedMessage>,
    index: usize,
) -> (Vec<EvidenceRef>, Option<PipelineIssue>, bool) {
    let refs = window
        .context_before_refs
        .iter()
        .chain(window.candidate_refs.iter())
        .chain(window.context_after_refs.iter())
        .collect::<Vec<_>>();
    let mut output = Vec::new();
    for value in raw {
        let Some(value) = value.as_object() else {
            return (
                Vec::new(),
                Some(issue(
                    window,
                    index,
                    "malformed_schema",
                    "evidence must be an object",
                )),
                true,
            );
        };
        let (Some(message_id), Some(quote), Some(role)) = (
            value.get("message_id").and_then(Value::as_str),
            value.get("quote").and_then(Value::as_str),
            value.get("evidence_role").and_then(Value::as_str),
        ) else {
            return (
                Vec::new(),
                Some(issue(
                    window,
                    index,
                    "malformed_schema",
                    "evidence fields are invalid",
                )),
                true,
            );
        };
        if quote.is_empty() || !EVIDENCE_ROLES.contains(&role) {
            return (
                Vec::new(),
                Some(issue(
                    window,
                    index,
                    "malformed_schema",
                    "evidence fields are invalid",
                )),
                true,
            );
        }
        let matching_refs = refs
            .iter()
            .filter(|reference| reference.message_id == message_id)
            .collect::<Vec<_>>();
        if matching_refs.is_empty() || !messages.contains_key(message_id) {
            return (
                Vec::new(),
                Some(issue(
                    window,
                    index,
                    "unknown_evidence_message",
                    &format!("evidence message {message_id:?} is not in the window"),
                )),
                true,
            );
        }
        let mut matches = Vec::new();
        for reference in matching_refs {
            let source = reference.text.chars().collect::<Vec<_>>();
            let needle = quote.chars().collect::<Vec<_>>();
            if needle.len() <= source.len() {
                for local_char in 0..=source.len() - needle.len() {
                    if source[local_char..local_char + needle.len()] == needle {
                        matches.push((reference, local_char));
                    }
                }
            }
        }
        if matches.is_empty() {
            return (
                Vec::new(),
                Some(issue(
                    window,
                    index,
                    "evidence_quote_not_found",
                    "evidence quote is not an exact substring of the referenced window span",
                )),
                false,
            );
        }
        if matches.len() != 1 {
            return (
                Vec::new(),
                Some(issue(
                    window,
                    index,
                    "ambiguous_evidence_quote",
                    "evidence quote occurs more than once in the referenced window spans",
                )),
                false,
            );
        }
        let (reference, local_start) = matches[0];
        let start = reference.start_char + local_start;
        output.push(EvidenceRef {
            message_id: message_id.into(),
            quote: quote.into(),
            start_char: start,
            end_char: start + quote.chars().count(),
            evidence_role: role.into(),
        });
    }
    (output, None, false)
}

fn is_candidate(evidence: &EvidenceRef, refs: &[MessageRef]) -> bool {
    refs.iter().any(|reference| {
        reference.message_id == evidence.message_id
            && reference.start_char <= evidence.start_char
            && evidence.end_char <= reference.end_char
    })
}

fn suspicious_modality(modality: &str, evidence: &[EvidenceRef]) -> bool {
    if modality != "asserted" {
        return false;
    }
    let text = format!(
        " {} ",
        evidence
            .iter()
            .map(|item| item.quote.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    );
    [
        " plan ",
        "plan to",
        "intend to",
        "打算",
        "计划",
        "准备",
        "might",
        "maybe",
        "possibly",
        "可能",
        "也许",
        "或许",
        " don't ",
        " not ",
        "never",
        "不",
        "没",
        "从不",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn observed_at(
    evidence: &[EvidenceRef],
    messages: &HashMap<String, NormalizedMessage>,
    window: &ExtractionWindow,
) -> String {
    if let Some(timestamp) = evidence.iter().rev().find_map(|item| {
        let timestamp = &messages.get(&item.message_id)?.timestamp;
        (!timestamp.is_empty()).then(|| timestamp.clone())
    }) {
        return timestamp;
    }
    window
        .candidate_refs
        .iter()
        .rev()
        .find_map(|reference| {
            let timestamp = &messages.get(&reference.message_id)?.timestamp;
            (!timestamp.is_empty()).then(|| timestamp.clone())
        })
        .unwrap_or_default()
}

fn issue(window: &ExtractionWindow, index: usize, code: &str, message: &str) -> PipelineIssue {
    PipelineIssue {
        stage: "validation".into(),
        code: code.into(),
        message: message.into(),
        scope_id: window.scope_id.clone(),
        episode_id: window.episode_id.clone(),
        window_id: window.id.clone(),
        details: Map::from_iter([("memory_index".into(), json!(index))]),
        ..PipelineIssue::default()
    }
}
