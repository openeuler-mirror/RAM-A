use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::{MemoryError, MemoryResult};

use super::{GraphExtractionInput, GraphExtractionOutput, GraphExtractor, GraphTypeRegistry};

const LLM_MAX_ATTEMPTS: usize = 5;
const DEFAULT_LLM_TIMEOUT_SECS: u64 = 60;
const ERROR_BODY_PREVIEW_MAX_CHARS: usize = 1000;
const MAX_GRAPH_LLM_RESPONSE_BYTES: usize = 256 * 1024;

pub const GRAPH_EXTRACTION_PROMPT_VERSION: &str = "graph-extraction-prompt-v1";
pub const GRAPH_EXTRACTION_SCHEMA_VERSION: &str = "graph-extraction-candidates-v1";
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

fn build_graph_llm_request(
    input: &GraphExtractionInput,
    type_registry: &GraphTypeRegistry,
) -> GraphLlmRequest {
    let registry_json = serde_json::json!({
        "version": type_registry.version.clone(),
        "entity_types": type_registry.entity_types.clone(),
        "predicates": type_registry.predicates.clone(),
    });
    let record_json = serde_json::json!({
        "memory_space_id": input.memory_space_id.clone(),
        "memory_record_id": input.memory_record_id.clone(),
        "metadata": input.metadata.clone(),
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
                        "start_byte": 0,
                        "end_byte": 0
                    }
                ],
                "confidence": 0.0,
                "valid_from_ms": null,
                "valid_to_ms": null
            }
        ]
    });

    GraphLlmRequest {
        messages: vec![
            GraphLlmMessage {
                role: "system".to_string(),
                content: "Extract graph memory candidates from one memory record. Return only one valid JSON object. Do not include Markdown or explanatory text.".to_string(),
            },
            GraphLlmMessage {
                role: "user".to_string(),
                content: format!(
                    "Use only the registered entity types and predicates. Every fact must be grounded by evidence from the record text. Evidence start_byte/end_byte are byte offsets in the UTF-8 record text, not character indexes.\n\nType registry:\n{}\n\nMemory record:\n{}\n\nReturn JSON matching this shape. Omit a field only when it is optional. Empty entities/facts arrays are allowed when the record has no graph-relevant content.\n{}",
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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
}
