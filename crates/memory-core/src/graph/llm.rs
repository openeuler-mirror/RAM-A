use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::{MemoryError, MemoryResult};

use super::{
    GraphEvidenceSpan, GraphExtractionInput, GraphExtractionOutput, GraphExtractor,
    GraphTypeRegistry, GRAPH_FALLBACK_PREDICATE,
};

const LLM_MAX_ATTEMPTS: usize = 5;
const DEFAULT_LLM_TIMEOUT_SECS: u64 = 60;
const ERROR_BODY_PREVIEW_MAX_CHARS: usize = 1000;
const MAX_GRAPH_LLM_RESPONSE_BYTES: usize = 256 * 1024;

pub const GRAPH_EXTRACTION_PROMPT_VERSION: &str = "graph-extraction-prompt-v6";
pub const GRAPH_EXTRACTION_SCHEMA_VERSION: &str = "graph-extraction-candidates-v3";
pub const OPENAI_COMPATIBLE_CLIENT_NAME: &str = "openai-compatible";
pub const LLM_GRAPH_EXTRACTOR_NAME: &str = "llm-graph-extractor";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphLlmMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphLlmRequest {
    pub messages: Vec<GraphLlmMessage>,
    pub temperature: f32,
    pub response_format_json: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphLlmResponse {
    pub content: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

#[async_trait::async_trait]
pub trait GraphLlmClient: Send + Sync {
    fn client_name(&self) -> &str;
    fn model_name(&self) -> &str;
    async fn complete_json(&self, request: GraphLlmRequest) -> MemoryResult<GraphLlmResponse>;
}

pub struct OpenAiCompatibleGraphLlmClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    timeout: Option<Duration>,
}

impl OpenAiCompatibleGraphLlmClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::with_base_url(api_key, "https://openrouter.ai/api/v1", model)
    }

    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            timeout: Some(Duration::from_secs(DEFAULT_LLM_TIMEOUT_SECS)),
        }
    }

    pub fn with_timeout_ms(mut self, timeout_ms: Option<u64>) -> Self {
        self.timeout = timeout_ms.map(Duration::from_millis);
        self
    }

    fn chat_completions_url(&self) -> String {
        if self.base_url.ends_with("/chat/completions") {
            self.base_url.clone()
        } else {
            format!("{}/chat/completions", self.base_url)
        }
    }

    async fn complete_once(
        &self,
        request: &GraphLlmRequest,
    ) -> Result<GraphLlmResponse, LlmAttemptError> {
        let body = OpenAiChatRequest {
            model: &self.model,
            messages: &request.messages,
            temperature: request.temperature,
            response_format: request
                .response_format_json
                .then_some(OpenAiResponseFormat {
                    kind: "json_object",
                }),
        };

        let mut http_request = self
            .client
            .post(self.chat_completions_url())
            .bearer_auth(&self.api_key)
            .json(&body);
        if let Some(timeout) = self.timeout {
            http_request = http_request.timeout(timeout);
        }

        let mut response = http_request.send().await.map_err(|error| {
            LlmAttemptError::retryable(format!("graph LLM request failed: {error}"))
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(LlmAttemptError {
                retryable: is_retryable_status(status),
                message: format!("graph LLM API returned HTTP {status}"),
            });
        }
        let body_text = read_bounded_response_body(&mut response).await?;

        let body: OpenAiChatResponse = serde_json::from_str(&body_text).map_err(|error| {
            LlmAttemptError::retryable(format!("decode failed for graph LLM response: {error}"))
        })?;
        let content = body
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or_else(|| LlmAttemptError {
                retryable: true,
                message: "graph LLM response did not include message content".to_string(),
            })?;
        validate_json_content(&content)?;

        Ok(GraphLlmResponse {
            content,
            input_tokens: body.usage.as_ref().and_then(|usage| usage.prompt_tokens),
            output_tokens: body
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens),
        })
    }
}

#[async_trait::async_trait]
impl GraphLlmClient for OpenAiCompatibleGraphLlmClient {
    fn client_name(&self) -> &str {
        OPENAI_COMPATIBLE_CLIENT_NAME
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    async fn complete_json(&self, request: GraphLlmRequest) -> MemoryResult<GraphLlmResponse> {
        let mut last_error = None;
        for attempt in 1..=LLM_MAX_ATTEMPTS {
            match self.complete_once(&request).await {
                Ok(response) => return Ok(response),
                Err(error) if error.retryable && attempt < LLM_MAX_ATTEMPTS => {
                    let backoff = retry_backoff(attempt);
                    eprintln!(
                        "graph LLM attempt {attempt}/{LLM_MAX_ATTEMPTS} failed: {}; retrying in {}s",
                        error.summary(),
                        backoff.as_secs()
                    );
                    tokio::time::sleep(backoff).await;
                    last_error = Some(error.message);
                }
                Err(error) => {
                    return Err(MemoryError::Extraction {
                        message: error.message,
                    });
                }
            }
        }

        Err(MemoryError::Extraction {
            message: last_error
                .unwrap_or_else(|| "graph LLM request failed without an error".to_string()),
        })
    }
}

pub struct LlmGraphExtractor {
    client: Arc<dyn GraphLlmClient>,
    type_registry: GraphTypeRegistry,
    prompt_version: String,
    schema_version: String,
}

impl LlmGraphExtractor {
    pub fn new(client: Arc<dyn GraphLlmClient>, type_registry: GraphTypeRegistry) -> Self {
        Self {
            client,
            type_registry,
            prompt_version: GRAPH_EXTRACTION_PROMPT_VERSION.to_string(),
            schema_version: GRAPH_EXTRACTION_SCHEMA_VERSION.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl GraphExtractor for LlmGraphExtractor {
    fn extractor_name(&self) -> &str {
        LLM_GRAPH_EXTRACTOR_NAME
    }

    fn model_name(&self) -> &str {
        self.client.model_name()
    }

    fn prompt_version(&self) -> &str {
        &self.prompt_version
    }

    fn schema_version(&self) -> &str {
        &self.schema_version
    }

    async fn extract(&self, input: GraphExtractionInput) -> MemoryResult<GraphExtractionOutput> {
        let response = self
            .client
            .complete_json(build_graph_llm_request(&input, &self.type_registry))
            .await?;
        let mut output = parse_graph_llm_output_text(&response.content)?;
        repair_evidence_byte_offsets(&mut output, &input.text);
        normalize_llm_entity_types(&mut output, &self.type_registry);
        deduplicate_entity_local_ids(&mut output);
        drop_unmaterializable_facts(&mut output, &self.type_registry, &input.text);
        sanitize_llm_fact_times(&mut output, &input.text);
        drop_low_signal_facts(&mut output);
        uniquify_fact_local_ids(&mut output);
        deduplicate_semantic_facts(&mut output);
        if response.input_tokens.is_some() {
            output.input_tokens = response.input_tokens;
        }
        if response.output_tokens.is_some() {
            output.output_tokens = response.output_tokens;
        }
        Ok(output)
    }
}

pub fn parse_graph_extraction_output_text(text: &str) -> MemoryResult<GraphExtractionOutput> {
    let json_text = strip_json_fence(text);
    serde_json::from_str(json_text).map_err(|error| MemoryError::Extraction {
        message: format!("failed to parse graph extraction JSON: {error}"),
    })
}

#[derive(Deserialize)]
struct LlmGraphExtractionEnvelope {
    entities: Vec<serde_json::Value>,
    facts: Vec<serde_json::Value>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

fn parse_graph_llm_output_text(text: &str) -> MemoryResult<GraphExtractionOutput> {
    let json_text = strip_json_fence(text);
    let envelope: LlmGraphExtractionEnvelope =
        serde_json::from_str(json_text).map_err(|error| MemoryError::Extraction {
            message: format!("failed to parse graph extraction JSON: {error}"),
        })?;
    let entity_count = envelope.entities.len();
    let entities = envelope
        .entities
        .into_iter()
        .filter_map(|candidate| serde_json::from_value(candidate).ok())
        .collect::<Vec<_>>();
    let malformed_entities = entity_count - entities.len();
    if malformed_entities > 0 {
        eprintln!("graph LLM adapter dropped {malformed_entities} malformed entities");
    }
    let fact_count = envelope.facts.len();
    let facts = envelope
        .facts
        .into_iter()
        .filter_map(|candidate| serde_json::from_value(candidate).ok())
        .collect::<Vec<_>>();
    let malformed_facts = fact_count - facts.len();
    if malformed_facts > 0 {
        eprintln!("graph LLM adapter dropped {malformed_facts} malformed facts");
    }
    Ok(GraphExtractionOutput {
        entities,
        facts,
        input_tokens: envelope.input_tokens,
        output_tokens: envelope.output_tokens,
    })
}

fn repair_evidence_byte_offsets(output: &mut GraphExtractionOutput, record_text: &str) {
    for fact in &mut output.facts {
        for evidence in &mut fact.evidence {
            let Some(evidence_text) = evidence.text.as_deref() else {
                continue;
            };
            if evidence_byte_range_matches_record(evidence, record_text, evidence_text) {
                continue;
            }
            let Some(start) = record_text.find(evidence_text) else {
                continue;
            };
            evidence.start_byte = Some(start);
            evidence.end_byte = Some(start + evidence_text.len());
        }
    }
}

fn evidence_byte_range_matches_record(
    evidence: &GraphEvidenceSpan,
    record_text: &str,
    evidence_text: &str,
) -> bool {
    let (Some(start), Some(end)) = (evidence.start_byte, evidence.end_byte) else {
        return false;
    };
    start < end
        && end <= record_text.len()
        && record_text.is_char_boundary(start)
        && record_text.is_char_boundary(end)
        && &record_text[start..end] == evidence_text
}

fn normalize_llm_entity_types(
    output: &mut GraphExtractionOutput,
    type_registry: &GraphTypeRegistry,
) {
    for entity in &mut output.entities {
        entity.entity_type = normalize_llm_entity_type(&entity.entity_type, type_registry);
    }
}

fn normalize_llm_entity_type(raw: &str, type_registry: &GraphTypeRegistry) -> String {
    let normalized = raw.trim().replace([' ', '-'], "_").to_ascii_uppercase();
    if let Some(registered) = registered_entity_type(&normalized, type_registry) {
        return registered.to_string();
    }
    if let Some(alias) = entity_type_alias(&normalized) {
        if let Some(registered) = registered_entity_type(alias, type_registry) {
            return registered.to_string();
        }
    }
    fallback_entity_type(type_registry).to_string()
}

fn registered_entity_type<'a>(
    candidate: &str,
    type_registry: &'a GraphTypeRegistry,
) -> Option<&'a str> {
    type_registry
        .entity_types
        .iter()
        .find(|entity_type| entity_type.eq_ignore_ascii_case(candidate))
        .map(String::as_str)
}

fn entity_type_alias(entity_type: &str) -> Option<&'static str> {
    match entity_type {
        "FAMILY" | "HOUSEHOLD" | "TEAM" | "CLUB" | "COMMUNITY" | "CLASS" | "COHORT"
        | "SUPPORT_GROUP" | "SOCIAL_GROUP" => Some("GROUP"),
        "COMPANY" | "BUSINESS" | "AGENCY" | "SCHOOL" | "UNIVERSITY" | "INSTITUTION"
        | "NONPROFIT" => Some("ORGANIZATION"),
        "PLACE" | "CITY" | "COUNTRY" | "ADDRESS" | "VENUE" => Some("LOCATION"),
        "DATE" | "DATETIME" | "TIME_PERIOD" | "PERIOD" | "SCHEDULE" => Some("TIME"),
        "TASK" | "HOBBY" | "PROGRAM" | "MEETING" | "APPOINTMENT" => Some("ACTIVITY"),
        "TOPIC" | "SUBJECT" | "IDEA" | "ATTRIBUTE" | "STATUS" | "ROLE" => Some("CONCEPT"),
        _ => None,
    }
}

fn fallback_entity_type(type_registry: &GraphTypeRegistry) -> &str {
    registered_entity_type("CONCEPT", type_registry)
        .or_else(|| registered_entity_type("OBJECT", type_registry))
        .or_else(|| type_registry.entity_types.first().map(String::as_str))
        .unwrap_or("CONCEPT")
}

fn deduplicate_entity_local_ids(output: &mut GraphExtractionOutput) {
    let entities = std::mem::take(&mut output.entities);
    let mut groups: HashMap<String, Vec<super::ExtractedEntityCandidate>> = HashMap::new();
    let mut local_id_order = Vec::new();
    for entity in entities {
        if !groups.contains_key(&entity.local_id) {
            local_id_order.push(entity.local_id.clone());
        }
        groups
            .entry(entity.local_id.clone())
            .or_default()
            .push(entity);
    }

    let mut deduped = Vec::with_capacity(groups.len());
    let mut ambiguous = HashSet::new();
    let mut dropped = 0;
    for local_id in local_id_order {
        let candidates = groups
            .remove(&local_id)
            .expect("entity local id was inserted into its group");
        let first_identity = candidates.first().map(entity_identity);
        if candidates
            .iter()
            .any(|candidate| Some(entity_identity(candidate)) != first_identity)
        {
            ambiguous.insert(local_id);
            continue;
        }
        dropped += candidates.len().saturating_sub(1);
        deduped.push(
            candidates
                .into_iter()
                .max_by(|left, right| {
                    left.confidence
                        .unwrap_or(0.0)
                        .total_cmp(&right.confidence.unwrap_or(0.0))
                })
                .expect("entity group is non-empty"),
        );
    }

    if !ambiguous.is_empty() {
        let fact_count_before = output.facts.len();
        output.facts.retain(|fact| {
            !ambiguous.contains(&fact.subject_ref) && !ambiguous.contains(&fact.object_ref)
        });
        eprintln!(
            "graph LLM adapter dropped {} ambiguous entity local_ids and {} dependent facts",
            ambiguous.len(),
            fact_count_before - output.facts.len()
        );
    }
    if dropped > 0 {
        eprintln!("graph LLM adapter dropped {dropped} equivalent duplicate entity local_ids");
    }
    output.entities = deduped;
}

fn entity_identity(entity: &super::ExtractedEntityCandidate) -> (String, String) {
    (
        entity
            .name
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase(),
        entity.entity_type.to_ascii_uppercase(),
    )
}

fn drop_unmaterializable_facts(
    output: &mut GraphExtractionOutput,
    type_registry: &GraphTypeRegistry,
    record_text: &str,
) {
    let entity_ids = output
        .entities
        .iter()
        .map(|entity| entity.local_id.as_str())
        .collect::<HashSet<_>>();
    let fact_count_before = output.facts.len();
    let fallback_predicate_available = type_registry.predicate(GRAPH_FALLBACK_PREDICATE).is_some();
    output.facts.retain_mut(|fact| {
        fact.evidence
            .retain(|evidence| evidence_is_grounded(evidence, record_text));
        if fact.evidence.is_empty() {
            return false;
        }
        if !entity_ids.contains(fact.subject_ref.as_str())
            || !entity_ids.contains(fact.object_ref.as_str())
        {
            return false;
        }
        if type_registry.predicate(&fact.predicate).is_some() {
            return true;
        }
        if fallback_predicate_available {
            fact.predicate = GRAPH_FALLBACK_PREDICATE.to_string();
            return true;
        }
        false
    });
    let dropped = fact_count_before - output.facts.len();
    if dropped > 0 {
        eprintln!("graph LLM adapter dropped {dropped} unmaterializable facts");
    }
}

fn sanitize_llm_fact_times(output: &mut GraphExtractionOutput, record_text: &str) {
    for fact in &mut output.facts {
        let grounded_expression = fact
            .temporal_expression
            .as_deref()
            .map(str::trim)
            .filter(|expression| !expression.is_empty() && record_text.contains(expression));

        if fact.temporal_expression.is_some() && grounded_expression.is_none() {
            fact.temporal_expression = None;
            fact.valid_from_ms = None;
            fact.valid_to_ms = None;
            continue;
        }
        fact.temporal_expression = grounded_expression.map(ToOwned::to_owned);

        // Numeric fact times must come from a dedicated resolver. LLM calendar
        // arithmetic is not reliable enough to publish directly to graph consumers.
        fact.valid_from_ms = None;
        fact.valid_to_ms = None;
    }
}

fn drop_low_signal_facts(output: &mut GraphExtractionOutput) {
    // Defense in depth for prompt drift: the prompt should avoid these, but
    // weak conversational facts are more harmful than losing a bookkeeping edge.
    let fact_count_before = output.facts.len();
    output
        .facts
        .retain(|fact| !is_low_signal_conversational_fact(fact));
    let dropped = fact_count_before - output.facts.len();
    if dropped > 0 {
        eprintln!("graph LLM adapter dropped {dropped} low-signal facts");
    }
}

fn is_low_signal_conversational_fact(fact: &super::ExtractedFactCandidate) -> bool {
    if fact.subject_ref == fact.object_ref {
        return true;
    }
    if fact.predicate == "MENTIONED" {
        return true;
    }

    let normalized = normalize_fact_text_for_filtering(&fact.fact_text);
    [
        "mentioned",
        "is mentioned",
        "was mentioned",
        "are mentioned",
        "were mentioned",
        "communicates with",
        "communicating with",
        "is communicating with",
        "was communicating with",
        "talked to",
        "talks to",
        "is talking to",
        "thanked",
        "thanks",
        "thanking",
        "is thanked by",
        "was thanked by",
        "agrees with",
        "agreed with",
        "is in agreement with",
        "was in agreement with",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
}

fn uniquify_fact_local_ids(output: &mut GraphExtractionOutput) {
    let mut seen = HashSet::new();
    let mut repaired = 0usize;
    for fact in &mut output.facts {
        if seen.insert(fact.local_id.clone()) {
            continue;
        }
        let base = fact.local_id.clone();
        let mut suffix = 2usize;
        loop {
            let candidate = format!("{base}:{suffix}");
            if seen.insert(candidate.clone()) {
                fact.local_id = candidate;
                repaired += 1;
                break;
            }
            suffix += 1;
        }
    }
    if repaired > 0 {
        eprintln!("graph LLM adapter repaired {repaired} duplicate fact local_ids");
    }
}

fn deduplicate_semantic_facts(output: &mut GraphExtractionOutput) {
    let facts = std::mem::take(&mut output.facts);
    let mut deduped = Vec::with_capacity(facts.len());
    let mut seen = HashMap::new();
    for fact in facts {
        let key = (
            fact.subject_ref.clone(),
            fact.object_ref.clone(),
            normalize_fact_text_for_filtering(&fact.fact_text),
        );
        if let Some(existing_index) = seen.get(&key).copied() {
            if should_replace_fact(&deduped[existing_index], &fact) {
                deduped[existing_index] = fact;
            }
            continue;
        }
        seen.insert(key, deduped.len());
        deduped.push(fact);
    }
    output.facts = deduped;
}

fn should_replace_fact(
    existing: &super::ExtractedFactCandidate,
    candidate: &super::ExtractedFactCandidate,
) -> bool {
    let existing_priority = predicate_specificity(&existing.predicate);
    let candidate_priority = predicate_specificity(&candidate.predicate);
    if candidate_priority != existing_priority {
        return candidate_priority > existing_priority;
    }
    candidate.confidence.unwrap_or(0.0) > existing.confidence.unwrap_or(0.0)
}

fn predicate_specificity(predicate: &str) -> u8 {
    match predicate {
        "MENTIONED" => 0,
        GRAPH_FALLBACK_PREDICATE => 1,
        _ => 2,
    }
}

fn normalize_fact_text_for_filtering(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn evidence_is_grounded(evidence: &GraphEvidenceSpan, record_text: &str) -> bool {
    match evidence.text.as_deref() {
        Some(evidence_text) => {
            evidence_byte_range_matches_record(evidence, record_text, evidence_text)
        }
        None => {
            let (Some(start), Some(end)) = (evidence.start_byte, evidence.end_byte) else {
                return false;
            };
            start < end
                && end <= record_text.len()
                && record_text.is_char_boundary(start)
                && record_text.is_char_boundary(end)
        }
    }
}

fn build_graph_llm_request(
    input: &GraphExtractionInput,
    type_registry: &GraphTypeRegistry,
) -> GraphLlmRequest {
    let source_context = graph_source_context(&input.metadata);
    let registry_json = serde_json::json!({
        "version": type_registry.version.clone(),
        "entity_types": type_registry.entity_types.clone(),
        "predicates": type_registry.predicates.clone(),
    });
    let record_json = serde_json::json!({
        "memory_space_id": input.memory_space_id.clone(),
        "memory_record_id": input.memory_record_id.clone(),
        "metadata": input.metadata.clone(),
        "source_context": source_context,
        "context_record_ids": input.context_record_ids.clone(),
        "type_registry_version": input.type_registry_version.clone(),
        "text": input.text.clone(),
    });
    let output_schema = serde_json::json!({
        "entities": [
            {
                "local_id": "entity:<stable-local-name>",
                "name": "<entity surface name>",
                "entity_type": "<one registered entity type>",
                "confidence": 0.0
            }
        ],
        "facts": [
            {
                "local_id": "fact:<stable-local-name>",
                "subject_ref": "<entity local_id>",
                "predicate": "<one registered predicate>",
                "object_ref": "<entity local_id>",
                "fact_text": "<short grounded fact>",
                "evidence": [
                    {
                        "text": "<exact substring from record text>",
                        "start_byte": null,
                        "end_byte": null
                    }
                ],
                "confidence": 0.0,
                "temporal_expression": null
            }
        ]
    });
    let fallback_instruction = if type_registry.predicate(GRAPH_FALLBACK_PREDICATE).is_some() {
        "Use RELATED_TO when a grounded relationship does not fit a more specific predicate; do not invent predicate names."
    } else {
        "Do not invent predicate names."
    };

    GraphLlmRequest {
        messages: vec![
            GraphLlmMessage {
                role: "system".to_string(),
                content: "Extract graph memory candidates from one memory record. Return only one valid JSON object. Do not include Markdown or explanatory text.".to_string(),
            },
            GraphLlmMessage {
                role: "user".to_string(),
                content: format!(
                    "Use only the registered entity types and predicates. If an entity does not fit a registered type, use CONCEPT. Use GROUP for families, teams, clubs, classes, communities, and support groups when GROUP is registered. Prefer the most specific registered predicate. {fallback_instruction} Every fact must be grounded by evidence from the record text. Every fact subject_ref and object_ref must exactly match a local_id in entities; omit the fact if either endpoint is missing or uncertain. Use source_context only to ground who spoke, when the record was observed, and which session/turn the text came from; do not create facts from metadata alone. If source_context.speaker is present, use that person as the subject for first-person statements such as I, me, my, mine, we, our, and us. Extract grounded relationships, preferences, plans, identities, roles, locations, and stable attributes. Also extract a concrete activity or event that the speaker attended, visited, completed, or participated in when the record states it; a concrete event does not need to be recurring or durable. Use ATTENDED, PARTICIPATED_IN, or VISITED when they fit. For every temporal fact, set temporal_expression to the exact substring in the record text that supports its time, or null when the text contains no temporal expression. Do not return valid_from_ms or valid_to_ms: numeric time resolution is performed later by a trusted component. Do not extract conversational bookkeeping or low-value interaction facts such as X mentioned Y, X is communicating with Y, X thanked Y, or X agreed with Y unless the text establishes a relationship, preference, plan, event participation, identity, role, location, or stable attribute. Do not extract self-loop facts. Use one predicate for a fact; do not duplicate the same fact under multiple predicates. Use complete canonical names when the text provides them instead of nicknames or abbreviations. Evidence text must be an exact substring from the record text. Evidence start_byte/end_byte are byte offsets in the UTF-8 record text, not character indexes; set them to null if you are not certain.\n\nType registry:\n{}\n\nMemory record:\n{}\n\nReturn JSON matching this shape. Omit a field only when it is optional. Empty entities/facts arrays are allowed when the record has no graph-relevant content.\n{}",
                    pretty_json(&registry_json),
                    pretty_json(&record_json),
                    pretty_json(&output_schema)
                ),
            },
        ],
        temperature: 0.0,
        response_format_json: true,
    }
}

fn graph_source_context(metadata: &serde_json::Value) -> serde_json::Value {
    let mut context = serde_json::Map::new();
    copy_json_field(metadata, &mut context, "speaker");
    if !context.contains_key("speaker") {
        if let Some(name) = metadata
            .get("graph_source_entity")
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            context.insert(
                "speaker".to_string(),
                serde_json::Value::String(name.to_string()),
            );
        }
    }
    copy_json_field(metadata, &mut context, "session_id");
    copy_json_field(metadata, &mut context, "turn_index");
    copy_json_field(metadata, &mut context, "session_timestamp");
    copy_json_field(metadata, &mut context, "observed_at_ms");
    serde_json::Value::Object(context)
}

fn copy_json_field(
    metadata: &serde_json::Value,
    context: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) {
    if let Some(value) = metadata.get(field) {
        context.insert(field.to_string(), value.clone());
    }
}

#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: &'a [GraphLlmMessage],
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<OpenAiResponseFormat>,
}

#[derive(Serialize)]
struct OpenAiResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
}

struct LlmAttemptError {
    retryable: bool,
    message: String,
}

fn validate_json_content(content: &str) -> Result<(), LlmAttemptError> {
    let json_text = strip_json_fence(content);
    serde_json::from_str::<serde_json::Value>(json_text)
        .map(|_| ())
        .map_err(|error| {
            LlmAttemptError::retryable(format!("graph LLM returned invalid JSON: {error}"))
        })
}

async fn read_bounded_response_body(
    response: &mut reqwest::Response,
) -> Result<String, LlmAttemptError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        LlmAttemptError::retryable(format!("failed to read graph LLM response body: {error}"))
    })? {
        if body.len().saturating_add(chunk.len()) > MAX_GRAPH_LLM_RESPONSE_BYTES {
            return Err(LlmAttemptError {
                retryable: false,
                message: format!(
                    "graph LLM response body exceeds {MAX_GRAPH_LLM_RESPONSE_BYTES} byte limit"
                ),
            });
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|error| {
        LlmAttemptError::retryable(format!("graph LLM response body is not UTF-8: {error}"))
    })
}

impl LlmAttemptError {
    fn retryable(message: String) -> Self {
        Self {
            retryable: true,
            message,
        }
    }

    fn summary(&self) -> String {
        preview_body(&self.message)
    }
}

fn strip_json_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(after_ticks) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some(first_newline) = after_ticks.find('\n') else {
        return trimmed;
    };
    let language = after_ticks[..first_newline].trim();
    if !language.is_empty() && !language.eq_ignore_ascii_case("json") {
        return trimmed;
    }
    let body = &after_ticks[first_newline + 1..];
    let Some(end) = body.rfind("```") else {
        return trimmed;
    };
    body[..end].trim()
}

fn preview_body(body: &str) -> String {
    body.chars().take(ERROR_BODY_PREVIEW_MAX_CHARS).collect()
}

fn pretty_json(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn retry_backoff(attempt: usize) -> Duration {
    Duration::from_secs(1 << (attempt - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ExtractedEntityCandidate, ExtractedFactCandidate};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn conflicting_duplicate_entity_ids_drop_dependent_facts() {
        let mut output = GraphExtractionOutput {
            entities: vec![
                ExtractedEntityCandidate {
                    local_id: "entity:person".to_string(),
                    name: "Alice".to_string(),
                    entity_type: "PERSON".to_string(),
                    confidence: Some(0.9),
                },
                ExtractedEntityCandidate {
                    local_id: "entity:person".to_string(),
                    name: "Bob".to_string(),
                    entity_type: "PERSON".to_string(),
                    confidence: Some(0.8),
                },
                ExtractedEntityCandidate {
                    local_id: "entity:book".to_string(),
                    name: "Book club".to_string(),
                    entity_type: "GROUP".to_string(),
                    confidence: Some(0.8),
                },
            ],
            facts: vec![ExtractedFactCandidate {
                local_id: "fact:member".to_string(),
                subject_ref: "entity:person".to_string(),
                predicate: "MEMBER_OF".to_string(),
                object_ref: "entity:book".to_string(),
                fact_text: "The person is a member of the book club.".to_string(),
                evidence: Vec::new(),
                confidence: Some(0.8),
                temporal_expression: None,
                valid_from_ms: None,
                valid_to_ms: None,
            }],
            input_tokens: None,
            output_tokens: None,
        };

        deduplicate_entity_local_ids(&mut output);

        assert_eq!(output.entities.len(), 1);
        assert_eq!(output.entities[0].local_id, "entity:book");
        assert!(output.facts.is_empty());
    }

    #[test]
    fn equivalent_duplicate_entity_ids_keep_highest_confidence_candidate() {
        let mut output = GraphExtractionOutput {
            entities: vec![
                ExtractedEntityCandidate {
                    local_id: "entity:person".to_string(),
                    name: " Alice  Smith ".to_string(),
                    entity_type: "person".to_string(),
                    confidence: Some(0.4),
                },
                ExtractedEntityCandidate {
                    local_id: "entity:person".to_string(),
                    name: "Alice Smith".to_string(),
                    entity_type: "PERSON".to_string(),
                    confidence: Some(0.9),
                },
            ],
            facts: Vec::new(),
            input_tokens: None,
            output_tokens: None,
        };

        deduplicate_entity_local_ids(&mut output);

        assert_eq!(output.entities.len(), 1);
        assert_eq!(output.entities[0].name, "Alice Smith");
        assert_eq!(output.entities[0].confidence, Some(0.9));
    }

    #[test]
    fn openai_compatible_client_defaults_to_timeout_and_allows_override() {
        let client = OpenAiCompatibleGraphLlmClient::new("test-key", "test-model");
        assert_eq!(client.timeout, Some(Duration::from_secs(60)));

        let client = client.with_timeout_ms(Some(1_500));
        assert_eq!(client.timeout, Some(Duration::from_millis(1_500)));

        let client = client.with_timeout_ms(None);
        assert_eq!(client.timeout, None);
    }

    #[test]
    fn chat_completions_url_does_not_append_endpoint_twice() {
        let client = OpenAiCompatibleGraphLlmClient::with_base_url(
            "test-key",
            "https://example.com/v1/chat/completions",
            "test-model",
        );

        assert_eq!(
            client.chat_completions_url(),
            "https://example.com/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn openai_compatible_client_retries_truncated_json_content() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let server_attempts = attempts.clone();
        let server = tokio::spawn(async move {
            for content in [r#"{"entities":["#, r#"{"entities":[],"facts":[]}"#] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 8192];
                let _ = socket.read(&mut request).await.unwrap();
                server_attempts.fetch_add(1, Ordering::SeqCst);
                let body = serde_json::json!({
                    "choices": [{"message": {"content": content}}],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 4}
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let client = OpenAiCompatibleGraphLlmClient::with_base_url(
            "test-key",
            format!("http://{address}/v1"),
            "test-model",
        );
        let response = client
            .complete_json(GraphLlmRequest {
                messages: vec![GraphLlmMessage {
                    role: "user".to_string(),
                    content: "extract".to_string(),
                }],
                temperature: 0.0,
                response_format_json: true,
            })
            .await
            .unwrap();

        server.await.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(response.content, r#"{"entities":[],"facts":[]}"#);
        assert_eq!(response.input_tokens, Some(3));
        assert_eq!(response.output_tokens, Some(4));
    }

    #[tokio::test]
    async fn openai_compatible_client_rejects_oversized_success_body_without_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let body = "x".repeat(256 * 1024 + 1);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let client = OpenAiCompatibleGraphLlmClient::with_base_url(
            "test-key",
            format!("http://{address}/v1"),
            "test-model",
        );

        let error = client
            .complete_once(&GraphLlmRequest {
                messages: Vec::new(),
                temperature: 0.0,
                response_format_json: true,
            })
            .await
            .unwrap_err();

        server.await.unwrap();
        assert!(!error.retryable);
        assert_eq!(
            error.message,
            "graph LLM response body exceeds 262144 byte limit"
        );
    }

    #[tokio::test]
    async fn openai_compatible_client_does_not_expose_error_response_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let body = "private-memory-marker";
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let client = OpenAiCompatibleGraphLlmClient::with_base_url(
            "test-key",
            format!("http://{address}/v1"),
            "test-model",
        );

        let error = client
            .complete_once(&GraphLlmRequest {
                messages: Vec::new(),
                temperature: 0.0,
                response_format_json: true,
            })
            .await
            .unwrap_err();

        server.await.unwrap();
        assert!(!error.message.contains("private-memory-marker"));
        assert_eq!(error.message, "graph LLM API returned HTTP 400 Bad Request");
    }

    #[test]
    fn strip_json_fence_handles_common_model_formats() {
        assert_eq!(
            strip_json_fence(" {\"entities\":[],\"facts\":[]} "),
            "{\"entities\":[],\"facts\":[]}"
        );
        assert_eq!(
            strip_json_fence("```json\n{\"ok\":true}\n```"),
            "{\"ok\":true}"
        );
        assert_eq!(
            strip_json_fence("```JSON\n{\"ok\":true}\n```"),
            "{\"ok\":true}"
        );
        assert_eq!(strip_json_fence("```\n{\"ok\":true}\n```"), "{\"ok\":true}");
        assert_eq!(
            strip_json_fence("```python\n{\"ok\":true}\n```"),
            "```python\n{\"ok\":true}\n```"
        );
        assert_eq!(
            strip_json_fence("```json\n{\"ok\":true}"),
            "```json\n{\"ok\":true}"
        );
    }

    #[test]
    fn extraction_prompt_requires_concrete_one_off_events_without_admitting_chat_noise() {
        let request = build_graph_llm_request(
            &GraphExtractionInput {
                memory_space_id: "space-1".to_string(),
                memory_record_id: "record-1".to_string(),
                text: "I went to a poetry reading last Friday.".to_string(),
                metadata: serde_json::json!({"speaker": "Alice"}),
                context_record_ids: Vec::new(),
                type_registry_version: "graph-type-registry-v2".to_string(),
            },
            &GraphTypeRegistry::default(),
        );
        let prompt = &request.messages[1].content;

        assert!(prompt.contains("a concrete event does not need to be recurring or durable"));
        assert!(prompt.contains("Use ATTENDED, PARTICIPATED_IN, or VISITED"));
        assert!(prompt.contains("Do not extract conversational bookkeeping"));
    }

    #[test]
    fn graph_source_context_uses_declared_source_entity_when_speaker_is_absent() {
        let context = graph_source_context(&serde_json::json!({
            "graph_source_entity": {
                "name": "Alice",
                "entity_type": "PERSON"
            }
        }));

        assert_eq!(context["speaker"], "Alice");
    }
}
