use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::client::{OpenAiCompatibleClient, StructuredOutputSpec};
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
    fn compatibility_identity(&self) -> Option<Value> {
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
    context_window_tokens: Option<usize>,
    reasoning_reserve_tokens: usize,
}

impl LlmMemoryExtractor {
    pub fn new(client: OpenAiCompatibleClient, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            prompt_version: "extract_v3".into(),
            max_output_tokens: 1600,
            context_window_tokens: None,
            reasoning_reserve_tokens: 0,
        }
    }

    pub fn with_token_budget(
        mut self,
        max_output_tokens: usize,
        context_window_tokens: Option<usize>,
        reasoning_reserve_tokens: usize,
    ) -> Self {
        self.max_output_tokens = max_output_tokens;
        self.context_window_tokens = context_window_tokens;
        self.reasoning_reserve_tokens = reasoning_reserve_tokens;
        self
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
    fn compatibility_identity(&self) -> Option<Value> {
        serde_json::to_value(self.client.compatibility()).ok()
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
        let messages = self.messages_within_budget(window, messages, observed_at)?;
        let spec = extraction_output_spec();
        let mut result = self
            .client
            .chat_with_schema(
                &self.model,
                messages,
                self.max_output_tokens,
                Some(spec.clone()),
            )
            .await?;
        let mut parsed = parse_extraction_json(&result.content)
            .and_then(|payload| batch_from_payload(&window.id, &payload, &result.content));
        // json_repair_attempts is the number of times the model is asked to
        // repair a response that is not valid JSON (0 disables the repair
        // path). The server configuration currently caps it at 1 because a
        // second repair round rarely recovers a response the first could not.
        for attempt in 1..=self.client.compatibility().json_repair_attempts {
            if parsed.is_ok() {
                break;
            }
            let repaired = self
                .repair_json(&result.content, spec.clone(), attempt)
                .await?;
            result.usage = combine_usage(result.usage, &repaired.usage);
            result.content = repaired.content;
            parsed = parse_extraction_json(&result.content)
                .and_then(|payload| batch_from_payload(&window.id, &payload, &result.content));
        }
        let mut batch =
            parsed.map_err(|error| error.at_site("memory_pipeline.extract.parse_response"))?;
        batch.usage = result.usage;
        Ok(batch)
    }
}

impl LlmMemoryExtractor {
    fn messages_within_budget(
        &self,
        window: &ExtractionWindow,
        messages_by_id: &HashMap<String, NormalizedMessage>,
        observed_at: &str,
    ) -> Result<Vec<Value>> {
        let mut prompt_window = window.clone();
        let mut trimmed_before = 0usize;
        let mut trimmed_after = 0usize;
        loop {
            let messages = extraction_prompt_messages(build_extraction_prompt(
                &prompt_window,
                messages_by_id,
                observed_at,
            )?);
            match self.client.validate_context_budget(
                &messages,
                self.max_output_tokens,
                self.context_window_tokens,
                self.reasoning_reserve_tokens,
            ) {
                Ok(()) => {
                    if trimmed_before > 0 || trimmed_after > 0 {
                        tracing::info!(
                            event = "ram_a.provider.context_trimmed",
                            stage = "extract",
                            trimmed_context_before = trimmed_before,
                            trimmed_context_after = trimmed_after,
                            remaining_context_before = prompt_window.context_before_refs.len(),
                            remaining_context_after = prompt_window.context_after_refs.len()
                        );
                    }
                    return Ok(messages);
                }
                Err(error)
                    if !prompt_window.context_after_refs.is_empty()
                        || !prompt_window.context_before_refs.is_empty() =>
                {
                    if prompt_window.context_after_refs.pop().is_some() {
                        trimmed_after += 1;
                    } else {
                        prompt_window.context_before_refs.remove(0);
                        trimmed_before += 1;
                    }
                    prompt_window.total_token_count =
                        prompt_window.candidate_token_count.saturating_add(
                            prompt_window
                                .context_before_refs
                                .iter()
                                .chain(prompt_window.context_after_refs.iter())
                                .map(|reference| crate::canonical::estimate_tokens(&reference.text))
                                .sum::<usize>(),
                        );
                    let _ = error;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn repair_json(
        &self,
        invalid_content: &str,
        spec: StructuredOutputSpec,
        attempt: usize,
    ) -> Result<crate::client::ChatResult> {
        tracing::warn!(
            event = "ram_a.provider.json_repair",
            stage = "extract",
            attempt
        );
        let messages = repair_messages(invalid_content, &spec.schema);
        self.client.validate_context_budget(
            &messages,
            self.max_output_tokens,
            self.context_window_tokens,
            self.reasoning_reserve_tokens,
        )?;
        self.client
            .chat_with_schema(&self.model, messages, self.max_output_tokens, Some(spec))
            .await
    }
}

fn extraction_prompt_messages(prompt: String) -> Vec<Value> {
    vec![
        json!({"role": "system", "content": "You are a source-faithful long-term-memory extractor. Output only the requested JSON object. Never invent evidence identifiers."}),
        json!({"role": "user", "content": prompt}),
    ]
}

pub fn extraction_output_spec() -> StructuredOutputSpec {
    StructuredOutputSpec {
        name: "atomic_memory_extraction",
        schema: json!({
            // Nested objects are given full properties/required definitions
            // so OpenAI-style strict json_schema validation accepts the
            // schema; only the fields the pipeline actually reads are pinned
            // and the rest is left to `validate_extraction` after parsing.
            "type": "object",
            "properties": {
                "schema_version": {"type": "string", "const": SCHEMA_VERSION},
                "memories": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": {"type": "string"},
                            "memory_type": {"type": "string"},
                            "subject": {
                                "type": "object",
                                "properties": {
                                    "name": {"type": "string"},
                                    "source_speaker": {"type": "string"}
                                },
                                "required": ["name"],
                                "additionalProperties": true
                            },
                            "predicate": {"type": "string"},
                            "object": {
                                "anyOf": [
                                    {
                                        "type": "object",
                                        "properties": {
                                            "name": {"type": "string"},
                                            "type": {"type": "string"}
                                        },
                                        "required": ["name"],
                                        "additionalProperties": true
                                    },
                                    {"type": "string"},
                                    {"type": "null"}
                                ]
                            },
                            "modality": {"type": "string"},
                            "event_time": {
                                "anyOf": [
                                    {
                                        "type": "object",
                                        "properties": {
                                            "raw": {"type": "string"},
                                            "normalized": {"type": "string"},
                                            "precision": {"type": "string"}
                                        },
                                        "additionalProperties": true
                                    },
                                    {"type": "null"}
                                ]
                            },
                            "attributes": {
                                "type": "object",
                                // Arbitrary key/value pairs; values are
                                // validated by `validate_extraction`.
                                "additionalProperties": true
                            },
                            "evidence": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "message_id": {"type": "string"},
                                        "quote": {"type": "string"},
                                        "evidence_role": {"type": "string"}
                                    },
                                    "required": ["message_id", "quote", "evidence_role"],
                                    "additionalProperties": false
                                }
                            },
                            "model_confidence": {"type": "number"}
                        },
                        "required": [
                            "text", "memory_type", "subject", "predicate",
                            "modality", "evidence", "model_confidence"
                        ],
                        "additionalProperties": true
                    }
                }
            },
            "required": ["schema_version", "memories"],
            "additionalProperties": false
        }),
    }
}

pub(crate) fn repair_messages(invalid_content: &str, schema: &Value) -> Vec<Value> {
    let bounded = invalid_content.chars().take(16_000).collect::<String>();
    vec![
        json!({"role": "system", "content": "Repair the supplied model output into valid JSON matching the schema. Preserve its facts exactly; do not add, infer, or remove facts. Return only the repaired JSON object."}),
        json!({"role": "user", "content": format!("Schema:\n{}\n\nInvalid output:\n{}", schema, bounded)}),
    ]
}

pub(crate) fn combine_usage(mut total: ModelUsage, added: &ModelUsage) -> ModelUsage {
    total.latency_ms += added.latency_ms;
    total.prompt_tokens += added.prompt_tokens;
    total.completion_tokens += added.completion_tokens;
    total.total_tokens += added.total_tokens;
    total
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
    let trimmed = content.trim();
    let text = strip_outer_code_fence(trimmed).unwrap_or(trimmed);
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

fn strip_outer_code_fence(text: &str) -> Option<&str> {
    if !text.starts_with("```") || !text.ends_with("```") || text.len() < 6 {
        return None;
    }
    let mut inner = text[3..text.len() - 3].trim();
    if inner
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("json"))
        && inner.get(4..).is_some_and(|rest| {
            rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with('{')
        })
    {
        inner = inner[4..].trim_start();
    }
    Some(inner.trim())
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
    if let Some(compatibility) = component.compatibility_identity() {
        value.insert("compatibility".into(), compatibility);
    }
    value
}
