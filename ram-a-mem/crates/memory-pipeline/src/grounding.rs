use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client::{OpenAiCompatibleClient, StructuredOutputSpec};
use crate::error::{PipelineError, Result};
use crate::extraction::{combine_usage, parse_extraction_json, repair_messages, ModelUsage};
use crate::models::{AtomicMemory, ExtractionWindow, NormalizedMessage};

const STATUSES: [&str; 4] = [
    "SUPPORTED",
    "PARTIALLY_SUPPORTED",
    "UNSUPPORTED",
    "UNCERTAIN",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GroundingResult {
    pub memory_id: String,
    pub status: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GroundingBatch {
    pub window_id: String,
    pub results: Vec<GroundingResult>,
    pub usage: ModelUsage,
    pub raw_response: String,
}

#[async_trait]
pub trait GroundingVerifier: Send + Sync {
    fn model(&self) -> &str;
    fn prompt_version(&self) -> &str;
    fn implementation(&self) -> &'static str;
    fn max_output_tokens(&self) -> Option<usize> {
        None
    }
    fn compatibility_identity(&self) -> Option<Value> {
        None
    }
    async fn verify(
        &self,
        window: &ExtractionWindow,
        memories: &[AtomicMemory],
        messages_by_id: &HashMap<String, NormalizedMessage>,
    ) -> Result<GroundingBatch>;
}

pub struct StaticGroundingVerifier {
    responses: HashMap<String, Value>,
}

pub struct LlmGroundingVerifier {
    client: OpenAiCompatibleClient,
    model: String,
    prompt_version: String,
    max_output_tokens: usize,
    context_window_tokens: Option<usize>,
    reasoning_reserve_tokens: usize,
}

impl LlmGroundingVerifier {
    pub fn new(client: OpenAiCompatibleClient, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            prompt_version: "ground_v2".into(),
            max_output_tokens: 1000,
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
impl GroundingVerifier for LlmGroundingVerifier {
    fn model(&self) -> &str {
        &self.model
    }
    fn prompt_version(&self) -> &str {
        &self.prompt_version
    }
    fn implementation(&self) -> &'static str {
        "LLMGroundingVerifier"
    }
    fn max_output_tokens(&self) -> Option<usize> {
        Some(self.max_output_tokens)
    }
    fn compatibility_identity(&self) -> Option<Value> {
        serde_json::to_value(self.client.compatibility()).ok()
    }

    async fn verify(
        &self,
        window: &ExtractionWindow,
        memories: &[AtomicMemory],
        messages: &HashMap<String, NormalizedMessage>,
    ) -> Result<GroundingBatch> {
        if memories.is_empty() {
            return Ok(GroundingBatch {
                window_id: window.id.clone(),
                results: Vec::new(),
                usage: ModelUsage::default(),
                raw_response: String::new(),
            });
        }
        let prompt = build_grounding_prompt(memories, messages)?;
        let messages_payload = vec![
            serde_json::json!({"role": "system", "content": "You verify whether each candidate memory is fully supported by its quoted source evidence. Return only JSON."}),
            serde_json::json!({"role": "user", "content": prompt}),
        ];
        self.client.validate_context_budget(
            &messages_payload,
            self.max_output_tokens,
            self.context_window_tokens,
            self.reasoning_reserve_tokens,
        )?;
        let spec = grounding_output_spec();
        let mut result = self
            .client
            .chat_with_schema(
                &self.model,
                messages_payload,
                self.max_output_tokens,
                Some(spec.clone()),
            )
            .await?;
        let mut parsed = parse_extraction_json(&result.content)
            .and_then(|payload| parse_grounding_results(&payload, memories));
        // json_repair_attempts is the number of times the model is asked to
        // repair a response that is not valid JSON (0 disables the repair
        // path). The server configuration currently caps it at 1 because a
        // second repair round rarely recovers a response the first could not.
        for attempt in 1..=self.client.compatibility().json_repair_attempts {
            if parsed.is_ok() {
                break;
            }
            tracing::warn!(
                event = "ram_a.provider.json_repair",
                stage = "ground",
                attempt
            );
            let repair_payload = repair_messages(&result.content, &spec.schema);
            self.client.validate_context_budget(
                &repair_payload,
                self.max_output_tokens,
                self.context_window_tokens,
                self.reasoning_reserve_tokens,
            )?;
            let repaired = self
                .client
                .chat_with_schema(
                    &self.model,
                    repair_payload,
                    self.max_output_tokens,
                    Some(spec.clone()),
                )
                .await?;
            result.usage = combine_usage(result.usage, &repaired.usage);
            result.content = repaired.content;
            parsed = parse_extraction_json(&result.content)
                .and_then(|payload| parse_grounding_results(&payload, memories));
        }
        Ok(GroundingBatch {
            window_id: window.id.clone(),
            results: parsed
                .map_err(|error| {
                    PipelineError::Protocol(format!("invalid grounding JSON: {error}"))
                })
                .map_err(|error| error.at_site("memory_pipeline.ground.parse_response"))?,
            usage: result.usage,
            raw_response: result.content,
        })
    }
}

pub fn grounding_output_spec() -> StructuredOutputSpec {
    StructuredOutputSpec {
        name: "memory_grounding",
        // Fully closed (every object sets `additionalProperties: false` and
        // lists every property in `required`), so it is safe to request
        // strict json_schema validation.
        strict: true,
        schema: json!({
            "type": "object",
            "properties": {
                "results": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "memory_id": {"type": "string"},
                            "status": {"type": "string", "enum": STATUSES},
                            "reason": {"type": "string"}
                        },
                        "required": ["memory_id", "status", "reason"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["results"],
            "additionalProperties": false
        }),
    }
}

pub fn build_grounding_prompt(
    memories: &[AtomicMemory],
    messages: &HashMap<String, NormalizedMessage>,
) -> Result<String> {
    let mut candidates = Vec::new();
    for memory in memories {
        let mut evidence_values = Vec::new();
        for evidence in &memory.evidence {
            let source = messages.get(&evidence.message_id).ok_or_else(|| {
                PipelineError::InvalidInput(format!(
                    "grounding evidence references unknown message: {}",
                    evidence.message_id
                ))
            })?;
            evidence_values.push(serde_json::json!({
                "message_id": evidence.message_id, "quote": evidence.quote,
                "start_char": evidence.start_char, "end_char": evidence.end_char,
                "evidence_role": evidence.evidence_role, "source_role": source.role,
                "source_speaker": source.speaker, "source_timestamp": source.timestamp
            }));
        }
        candidates.push(serde_json::json!({
            "memory_id": memory.id, "claim": memory.canonical_content(),
            "observation_time": memory.observed_at, "evidence": evidence_values
        }));
    }
    Ok(format!("Classify every memory as SUPPORTED, PARTIALLY_SUPPORTED, UNSUPPORTED, or UNCERTAIN. SUPPORTED means every material part of the claim follows from the quoted evidence. Return one result per memory_id.\n\n{}\n\nOutput: {{\"results\":[{{\"memory_id\":\"...\",\"status\":\"SUPPORTED\",\"reason\":\"...\"}}]}}", serde_json::to_string(&serde_json::json!({"memories": candidates})).expect("prompt serializes")))
}

impl StaticGroundingVerifier {
    pub fn new(responses: HashMap<String, Value>) -> Self {
        Self { responses }
    }
}

#[async_trait]
impl GroundingVerifier for StaticGroundingVerifier {
    fn model(&self) -> &str {
        "static"
    }

    fn prompt_version(&self) -> &str {
        "static_v1"
    }

    fn implementation(&self) -> &'static str {
        "StaticGroundingVerifier"
    }

    async fn verify(
        &self,
        window: &ExtractionWindow,
        memories: &[AtomicMemory],
        _messages_by_id: &HashMap<String, NormalizedMessage>,
    ) -> Result<GroundingBatch> {
        let mut results = Vec::new();
        for memory in memories {
            let response = self.responses.get(&memory.id).ok_or_else(|| {
                PipelineError::Protocol(format!(
                    "missing static grounding for memory {}",
                    memory.id
                ))
            })?;
            let (status, reason) = if let Some(status) = response.as_str() {
                (status, "")
            } else {
                let object = response.as_object().ok_or_else(|| {
                    PipelineError::Protocol(
                        "static grounding response must be a string or object".into(),
                    )
                })?;
                (
                    object.get("status").and_then(Value::as_str).unwrap_or(""),
                    object.get("reason").and_then(Value::as_str).unwrap_or(""),
                )
            };
            validate_status(status)?;
            results.push(GroundingResult {
                memory_id: memory.id.clone(),
                status: status.into(),
                reason: reason.into(),
            });
        }
        Ok(GroundingBatch {
            window_id: window.id.clone(),
            results,
            usage: ModelUsage::default(),
            raw_response: String::new(),
        })
    }
}

pub fn parse_grounding_results(
    payload: &Value,
    memories: &[AtomicMemory],
) -> Result<Vec<GroundingResult>> {
    let raw = payload
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| PipelineError::Protocol("grounding results must be a list".into()))?;
    let expected = memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    let mut by_id = HashMap::new();
    for result in raw {
        let object = result.as_object().ok_or_else(|| {
            PipelineError::Protocol("each grounding result must be an object".into())
        })?;
        let id = object
            .get("memory_id")
            .and_then(Value::as_str)
            .ok_or_else(|| PipelineError::Protocol("grounding result fields are invalid".into()))?;
        let status = object
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| PipelineError::Protocol("grounding result fields are invalid".into()))?;
        if by_id.contains_key(id) {
            return Err(PipelineError::Protocol(format!(
                "duplicate grounding result for {id}"
            )));
        }
        if !expected.contains(&id) {
            return Err(PipelineError::Protocol(format!(
                "unexpected grounding result for {id}"
            )));
        }
        validate_status(status)?;
        by_id.insert(
            id.to_owned(),
            GroundingResult {
                memory_id: id.into(),
                status: status.into(),
                reason: object
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            },
        );
    }
    Ok(memories
        .iter()
        .map(|memory| {
            by_id.remove(&memory.id).unwrap_or_else(|| GroundingResult {
                memory_id: memory.id.clone(),
                status: "UNCERTAIN".into(),
                reason: "verifier omitted this memory_id".into(),
            })
        })
        .collect())
}

fn validate_status(status: &str) -> Result<()> {
    if !STATUSES.contains(&status) {
        return Err(PipelineError::Protocol(format!(
            "unknown grounding status: {status:?}"
        )));
    }
    Ok(())
}
