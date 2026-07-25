use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::client::OpenAiCompatibleClient;
use crate::error::{PipelineError, Result};
use crate::models::{ExtractionWindow, NormalizedMessage};
use crate::window::render_window;

pub const SCHEMA_VERSION: &str = "atomic_memory_v1";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ModelUsage {
    pub latency_ms: f64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExtractionBatch {
    pub window_id: String,
    pub schema_version: String,
    pub raw_memories: Vec<Value>,
    pub usage: ModelUsage,
    pub raw_response: String,
}

#[async_trait]
pub trait MemoryExtractor: Send + Sync {
    fn model(&self) -> &str;
    fn prompt_version(&self) -> &str;
    fn implementation(&self) -> &'static str;
    fn max_output_tokens(&self) -> Option<usize> {
        None
    }
    async fn extract(
        &self,
        window: &ExtractionWindow,
        messages_by_id: &HashMap<String, NormalizedMessage>,
    ) -> Result<ExtractionBatch>;
}

pub struct StaticMemoryExtractor {
    responses: HashMap<String, Value>,
}

pub struct LlmMemoryExtractor {
    client: OpenAiCompatibleClient,
    model: String,
    prompt_version: String,
    max_output_tokens: usize,
}

impl LlmMemoryExtractor {
    pub fn new(client: OpenAiCompatibleClient, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            prompt_version: "extract_v2".into(),
            max_output_tokens: 1600,
        }
    }
}

#[async_trait]
impl MemoryExtractor for LlmMemoryExtractor {
    fn model(&self) -> &str {
        &self.model
    }
    fn prompt_version(&self) -> &str {
        &self.prompt_version
    }
    fn implementation(&self) -> &'static str {
        "LLMMemoryExtractor"
    }
    fn max_output_tokens(&self) -> Option<usize> {
        Some(self.max_output_tokens)
    }

    async fn extract(
        &self,
        window: &ExtractionWindow,
        messages: &HashMap<String, NormalizedMessage>,
    ) -> Result<ExtractionBatch> {
        let observed_at = window
            .candidate_refs
            .iter()
            .rev()
            .find_map(|reference| {
                let timestamp = &messages.get(&reference.message_id)?.timestamp;
                (!timestamp.is_empty()).then_some(timestamp.as_str())
            })
            .unwrap_or("");
        let prompt = build_extraction_prompt(window, messages, observed_at)?;
        let result = self.client.chat(&self.model, vec![
            json!({"role": "system", "content": "You are a source-faithful long-term-memory extractor. Output only the requested JSON object. Never invent evidence identifiers."}),
            json!({"role": "user", "content": prompt}),
        ], self.max_output_tokens).await?;
        let payload = parse_extraction_json(&result.content)?;
        let mut batch = batch_from_payload(&window.id, &payload, &result.content)?;
        batch.usage = result.usage;
        Ok(batch)
    }
}

impl StaticMemoryExtractor {
    pub fn new(responses: HashMap<String, Value>) -> Self {
        Self { responses }
    }
}

#[async_trait]
impl MemoryExtractor for StaticMemoryExtractor {
    fn model(&self) -> &str {
        "static"
    }

    fn prompt_version(&self) -> &str {
        "static_v1"
    }

    fn implementation(&self) -> &'static str {
        "StaticMemoryExtractor"
    }

    async fn extract(
        &self,
        window: &ExtractionWindow,
        _messages_by_id: &HashMap<String, NormalizedMessage>,
    ) -> Result<ExtractionBatch> {
        let payload = self.responses.get(&window.id).ok_or_else(|| {
            PipelineError::Protocol(format!(
                "missing static extraction for window {}",
                window.id
            ))
        })?;
        batch_from_payload(&window.id, payload, "")
    }
}

pub fn parse_extraction_json(content: &str) -> Result<Value> {
    let mut text = content.trim();
    if text.starts_with("```json\n") && text.ends_with("\n```") {
        text = &text[8..text.len() - 4];
    } else if text.starts_with("```\n") && text.ends_with("\n```") {
        text = &text[4..text.len() - 4];
    }
    let value: Value = serde_json::from_str(text).map_err(|error| {
        PipelineError::Protocol(format!("extractor did not return valid JSON: {error}"))
    })?;
    if !value.is_object() {
        return Err(PipelineError::Protocol(
            "extractor response must be a JSON object".into(),
        ));
    }
    Ok(value)
}

pub fn batch_from_payload(
    window_id: &str,
    payload: &Value,
    raw_response: &str,
) -> Result<ExtractionBatch> {
    let schema = payload.get("schema_version").and_then(Value::as_str);
    if schema != Some(SCHEMA_VERSION) {
        return Err(PipelineError::Protocol(format!(
            "unexpected extraction schema_version: {schema:?}"
        )));
    }
    let memories = payload
        .get("memories")
        .and_then(Value::as_array)
        .ok_or_else(|| PipelineError::Protocol("extraction memories must be a list".into()))?;
    if memories.iter().any(|memory| !memory.is_object()) {
        return Err(PipelineError::Protocol(
            "each extracted memory must be an object".into(),
        ));
    }
    Ok(ExtractionBatch {
        window_id: window_id.into(),
        schema_version: SCHEMA_VERSION.into(),
        raw_memories: memories.clone(),
        usage: ModelUsage::default(),
        raw_response: raw_response.into(),
    })
}

pub fn build_extraction_prompt(
    window: &ExtractionWindow,
    messages_by_id: &HashMap<String, NormalizedMessage>,
    observed_at: &str,
) -> Result<String> {
    let empty = r#"{"schema_version": "atomic_memory_v1", "memories": []}"#;
    let template = r#"{"schema_version": "atomic_memory_v1", "memories": [{"text": "...", "memory_type": "fact", "subject": {"name": "...", "source_speaker": "..."}, "predicate": "...", "object": {"name": "...", "type": "..."}, "modality": "asserted", "event_time": {"raw": "...", "normalized": "...", "precision": "..."}, "attributes": {}, "evidence": [{"message_id": "copy an exact message_id from the window", "quote": "copy an exact substring from that message span", "evidence_role": "primary"}], "model_confidence": 0.95}]}"#;
    Ok(format!(
        "Extract durable atomic memories from the candidate messages.\n\nRules:\n- Only candidate messages may create new memories. Context is for resolving references only.\n- Each memory must express one self-contained fact, preference, relationship, event, state, or procedure.\n- Preserve negation, plans, possibilities, conditions, names, numbers, and dates.\n- Do not add facts from world knowledge or from context alone.\n- Each memory needs at least one primary evidence item from a candidate message.\n- Evidence quote must be an exact quote from the referenced source message.\n- Return {empty} when nothing is durable.\n- Return one JSON object and no commentary.\n- Replace every \"...\" placeholder in the template below with source-grounded data.\n- subject MUST be an object, never a string.\n- object MUST be an object, string, or null.\n- event_time MUST be an object or null, never a string.\n- evidence MUST be a non-empty array of objects. message_id must be copied exactly\n  from a window header; quote must be an exact substring of that message span;\n  evidence_role must be primary or supporting. At least one primary item must cite\n  a candidate message, not context-only text.\n- model_confidence MUST be a number from 0.0 to 1.0, never words such as \"high\".\n- memory_type MUST be one of: fact, preference, relationship, event, state,\n  procedure, other. It cannot be \"planned\"; planned belongs in modality.\n- modality MUST be one of: asserted, negated, possible, planned, conditional, reported.\n\nRequired JSON shape:\n{template}\n\nHost observation time: {}\n\n{}\n",
        if observed_at.is_empty() { "unknown" } else { observed_at },
        render_window(window, messages_by_id)?
    ))
}

pub fn component_identity(component: &(impl MemoryExtractor + ?Sized)) -> Map<String, Value> {
    let mut value = Map::from_iter([
        ("implementation".into(), json!(component.implementation())),
        ("model".into(), json!(component.model())),
        ("prompt_version".into(), json!(component.prompt_version())),
    ]);
    if let Some(tokens) = component.max_output_tokens() {
        value.insert("max_output_tokens".into(), json!(tokens));
    }
    value
}
