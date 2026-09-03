use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{PipelineError, Result};
use crate::extraction::ModelUsage;

#[derive(Clone)]
pub struct OpenAiCompatibleClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    max_retries: usize,
    compatibility: ChatCompatibilityOptions,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTokenParameter {
    #[default]
    MaxTokens,
    MaxCompletionTokens,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputMode {
    #[default]
    PromptOnly,
    JsonObject,
    JsonSchema,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ChatCompatibilityOptions {
    pub reasoning_effort: Option<String>,
    pub enable_thinking: Option<bool>,
    pub send_temperature: bool,
    pub temperature: f64,
    pub output_token_parameter: OutputTokenParameter,
    pub structured_output: StructuredOutputMode,
    pub reasoning_only_retry: bool,
    pub json_repair_attempts: usize,
}

impl Default for ChatCompatibilityOptions {
    fn default() -> Self {
        Self {
            reasoning_effort: None,
            enable_thinking: None,
            send_temperature: true,
            temperature: 0.0,
            output_token_parameter: OutputTokenParameter::MaxTokens,
            structured_output: StructuredOutputMode::PromptOnly,
            reasoning_only_retry: false,
            json_repair_attempts: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StructuredOutputSpec {
    pub name: &'static str,
    pub schema: Value,
}

#[derive(Debug)]
pub struct ChatResult {
    pub content: String,
    pub usage: ModelUsage,
}

impl OpenAiCompatibleClient {
    pub fn new(
        api_key: impl Into<String>,
        base_url: &str,
        timeout_seconds: u64,
        max_retries: usize,
    ) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(PipelineError::InvalidInput(
                "API key must not be empty".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|error| {
                PipelineError::Protocol(format!("cannot build HTTP client: {error}"))
            })?;
        Ok(Self {
            client,
            api_key,
            base_url: base_url.trim_end_matches('/').into(),
            max_retries,
            compatibility: ChatCompatibilityOptions::default(),
        })
    }

    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.compatibility.reasoning_effort = reasoning_effort
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        self
    }

    pub fn with_compatibility(mut self, compatibility: ChatCompatibilityOptions) -> Self {
        self.compatibility = compatibility;
        self
    }

    pub fn compatibility(&self) -> &ChatCompatibilityOptions {
        &self.compatibility
    }

    pub fn validate_context_budget(
        &self,
        messages: &[Value],
        max_output_tokens: usize,
        context_window_tokens: Option<usize>,
        reasoning_reserve_tokens: usize,
    ) -> Result<()> {
        let Some(context_window_tokens) = context_window_tokens else {
            return Ok(());
        };
        let prompt_tokens =
            crate::canonical::estimate_tokens(&Value::Array(messages.to_vec()).to_string());
        let required = prompt_tokens
            .saturating_add(max_output_tokens)
            .saturating_add(reasoning_reserve_tokens);
        if required > context_window_tokens {
            return Err(PipelineError::InvalidInput(format!(
                "context_budget_exceeded: estimated request requires {required} tokens but model window is {context_window_tokens}"
            )));
        }
        Ok(())
    }

    pub fn from_env(
        api_key_env: &str,
        base_url: &str,
        timeout_seconds: u64,
        max_retries: usize,
    ) -> Result<Self> {
        let api_key = std::env::var(api_key_env).map_err(|_| {
            PipelineError::InvalidInput(format!("missing API key env {api_key_env}"))
        })?;
        Self::new(api_key, base_url, timeout_seconds, max_retries)
    }

    pub async fn chat(
        &self,
        model: &str,
        messages: Vec<Value>,
        max_tokens: usize,
    ) -> Result<ChatResult> {
        self.chat_with_schema(model, messages, max_tokens, None)
            .await
    }

    pub async fn chat_with_schema(
        &self,
        model: &str,
        messages: Vec<Value>,
        max_tokens: usize,
        structured_output: Option<StructuredOutputSpec>,
    ) -> Result<ChatResult> {
        let semantic_attempts = if self.compatibility.reasoning_only_retry {
            2
        } else {
            1
        };
        for semantic_attempt in 0..semantic_attempts {
            let mut attempt_messages = messages.clone();
            if semantic_attempt > 0 {
                attempt_messages.insert(0, json!({
                    "role": "system",
                    "content": "Return the final answer in message.content now. Do not return reasoning without final content."
                }));
            }
            let payload = self.build_payload(
                model,
                attempt_messages,
                max_tokens,
                structured_output.as_ref(),
                semantic_attempt > 0,
            )?;
            let response = self.send_with_retries(model, &payload).await?;
            if !response.content.is_empty() {
                return Ok(ChatResult {
                    content: response.content,
                    usage: response.usage,
                });
            }
            if response.has_reasoning {
                tracing::warn!(
                    event = "ram_a.provider.reasoning_only",
                    provider_kind = "llm",
                    provider = "openai_compatible",
                    model,
                    operation = "chat_completion",
                    semantic_attempt = semantic_attempt + 1,
                    finish_reason = response.finish_reason.as_deref().unwrap_or("unknown"),
                    completion_tokens = response.usage.completion_tokens
                );
                if semantic_attempt + 1 < semantic_attempts {
                    continue;
                }
                return Err(PipelineError::Protocol(
                    "chat completion returned reasoning content without final content".into(),
                )
                .at_site("memory_pipeline.provider.chat_completion"));
            }
            return Err(
                PipelineError::Protocol("chat completion returned empty content".into())
                    .at_site("memory_pipeline.provider.chat_completion"),
            );
        }
        unreachable!("semantic attempt loop always returns")
    }

    fn build_payload(
        &self,
        model: &str,
        messages: Vec<Value>,
        max_tokens: usize,
        structured_output: Option<&StructuredOutputSpec>,
        correction: bool,
    ) -> Result<Value> {
        let mut payload = json!({"model": model, "messages": messages});
        if self.compatibility.send_temperature {
            payload["temperature"] = json!(self.compatibility.temperature);
        }
        match self.compatibility.output_token_parameter {
            OutputTokenParameter::MaxTokens => payload["max_tokens"] = json!(max_tokens),
            OutputTokenParameter::MaxCompletionTokens => {
                payload["max_completion_tokens"] = json!(max_tokens)
            }
        }
        if let Some(reasoning_effort) = &self.compatibility.reasoning_effort {
            payload["reasoning_effort"] = json!(if correction { "none" } else { reasoning_effort });
        }
        if let Some(enable_thinking) = self.compatibility.enable_thinking {
            payload["enable_thinking"] = json!(if correction { false } else { enable_thinking });
        }
        match self.compatibility.structured_output {
            StructuredOutputMode::PromptOnly => {}
            StructuredOutputMode::JsonObject => {
                payload["response_format"] = json!({"type": "json_object"});
            }
            StructuredOutputMode::JsonSchema => {
                let spec = structured_output.ok_or_else(|| {
                    PipelineError::InvalidInput(
                        "json_schema structured output requires a schema".into(),
                    )
                })?;
                payload["response_format"] = json!({
                    "type": "json_schema",
                    "json_schema": {"name": spec.name, "strict": true, "schema": spec.schema}
                });
            }
        }
        Ok(payload)
    }

    async fn send_with_retries(&self, model: &str, payload: &Value) -> Result<ParsedResponse> {
        let mut last_error = String::new();
        for attempt in 0..self.max_retries.max(1) {
            let started = Instant::now();
            match self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&payload)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    let body = match response.text().await {
                        Ok(body) => body,
                        Err(error) => {
                            last_error = format!("chat completion body read failed: {error}");
                            if attempt + 1 < self.max_retries.max(1) {
                                let backoff = Duration::from_secs((1u64 << attempt.min(6)).min(64));
                                log_retry(
                                    model,
                                    attempt,
                                    self.max_retries,
                                    backoff,
                                    "response_read",
                                );
                                tokio::time::sleep(backoff).await;
                            }
                            continue;
                        }
                    };
                    let raw: Value = match serde_json::from_str(&body) {
                        Ok(raw) => raw,
                        Err(error) => {
                            last_error = format!("chat completion returned invalid JSON: {error}");
                            if attempt + 1 < self.max_retries.max(1) {
                                let backoff = Duration::from_secs((1u64 << attempt.min(6)).min(64));
                                log_retry(
                                    model,
                                    attempt,
                                    self.max_retries,
                                    backoff,
                                    "invalid_json",
                                );
                                tokio::time::sleep(backoff).await;
                            }
                            continue;
                        }
                    };
                    let content = raw
                        .pointer("/choices/0/message/content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    let has_reasoning = raw
                        .pointer("/choices/0/message/reasoning_content")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty());
                    let prompt = raw
                        .pointer("/usage/prompt_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or_else(|| estimate_value_tokens(&payload["messages"]));
                    let completion = raw
                        .pointer("/usage/completion_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or_else(|| estimate_text_tokens(&content));
                    let total = raw
                        .pointer("/usage/total_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(prompt + completion);
                    return Ok(ParsedResponse {
                        content,
                        has_reasoning,
                        finish_reason: raw
                            .pointer("/choices/0/finish_reason")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        usage: ModelUsage {
                            latency_ms: started.elapsed().as_secs_f64() * 1000.0,
                            prompt_tokens: prompt,
                            completion_tokens: completion,
                            total_tokens: total,
                        },
                    });
                }
                Ok(response) => {
                    let status = response.status();
                    last_error = format!("HTTP {status}");
                    if !retryable(status) {
                        break;
                    }
                }
                Err(error) => {
                    last_error = error.to_string();
                }
            }
            if attempt + 1 < self.max_retries.max(1) {
                let backoff = Duration::from_secs((1u64 << attempt.min(6)).min(64));
                log_retry(
                    model,
                    attempt,
                    self.max_retries,
                    backoff,
                    llm_error_kind(&last_error),
                );
                tokio::time::sleep(backoff).await;
            }
        }
        tracing::error!(
            event = "ram_a.provider.failed",
            provider_kind = "llm",
            provider = "openai_compatible",
            model,
            operation = "chat_completion",
            attempts = self.max_retries.max(1),
            error_kind = llm_error_kind(&last_error)
        );
        Err(PipelineError::Protocol(format!(
            "chat completion failed after retries: {last_error}"
        ))
        .at_site("memory_pipeline.provider.chat_completion"))
    }
}

struct ParsedResponse {
    content: String,
    has_reasoning: bool,
    finish_reason: Option<String>,
    usage: ModelUsage,
}

fn log_retry(model: &str, attempt: usize, max_retries: usize, backoff: Duration, kind: &str) {
    tracing::warn!(
        event = "ram_a.provider.retry",
        provider_kind = "llm",
        provider = "openai_compatible",
        model,
        operation = "chat_completion",
        attempt = attempt + 1,
        max_attempts = max_retries.max(1),
        backoff_ms = backoff.as_millis() as u64,
        error_kind = kind
    );
}

fn llm_error_kind(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("reasoning content without final content") {
        "reasoning_only"
    } else if lower.contains("empty content") {
        "empty_content"
    } else if lower.contains("429") {
        "http_429"
    } else if lower.contains("503") {
        "http_503"
    } else if lower.contains("invalid json") {
        "invalid_json"
    } else if lower.contains("body read") {
        "response_read"
    } else if lower.contains("timed out") {
        "timeout"
    } else if lower.contains("http ") {
        "http_status"
    } else if lower.contains("connect") {
        "connect"
    } else {
        "request"
    }
}

fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn estimate_value_tokens(value: &Value) -> i64 {
    estimate_text_tokens(&value.to_string())
}
fn estimate_text_tokens(value: &str) -> i64 {
    ((value.chars().count() / 4).max(1)) as i64
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    #[test]
    fn rejects_empty_api_key() {
        assert!(OpenAiCompatibleClient::new("  ", "http://localhost", 1, 1).is_err());
    }

    #[test]
    fn model_failures_have_stable_safe_categories() {
        for (message, expected) in [
            ("chat completion returned empty content", "empty_content"),
            ("chat completion returned invalid JSON", "invalid_json"),
            ("chat completion body read failed", "response_read"),
            ("request timed out", "timeout"),
            ("HTTP 429 Too Many Requests", "http_429"),
            ("HTTP 400 Bad Request", "http_status"),
            ("connection refused", "connect"),
        ] {
            assert_eq!(llm_error_kind(message), expected);
        }
    }

    async fn complete_captured_chat_request(client: OpenAiCompatibleClient) {
        let result = client
            .chat("model", vec![json!({"role": "user", "content": "hi"})], 10)
            .await
            .unwrap();
        assert_eq!(result.content, "ok");
        assert_eq!(result.usage.total_tokens, 2);
    }

    fn request_capturing_server() -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line
                    .to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                {
                    content_length = value.parse().unwrap();
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();
            sender.send(serde_json::from_slice(&body).unwrap()).unwrap();
            let body = serde_json::json!({
                "choices": [{"message": {"content": "ok"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })
            .to_string();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        (format!("http://{address}"), receiver, server)
    }

    fn sequence_server(
        bodies: Vec<Value>,
    ) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            for body in bodies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line
                        .to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                    {
                        content_length = value.parse().unwrap();
                    }
                }
                let mut request_body = vec![0u8; content_length];
                reader.read_exact(&mut request_body).unwrap();
                sender
                    .send(serde_json::from_slice(&request_body).unwrap())
                    .unwrap();
                let body = body.to_string();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        (format!("http://{address}"), receiver, server)
    }

    #[tokio::test]
    async fn configured_reasoning_effort_is_sent() {
        let (base_url, request, server) = request_capturing_server();
        let client = OpenAiCompatibleClient::new("key", &base_url, 2, 1)
            .unwrap()
            .with_reasoning_effort(Some("none".to_string()));

        complete_captured_chat_request(client).await;

        assert_eq!(request.recv().unwrap()["reasoning_effort"], "none");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn unconfigured_reasoning_effort_is_omitted() {
        let (base_url, request, server) = request_capturing_server();
        let client = OpenAiCompatibleClient::new("key", &base_url, 2, 1).unwrap();

        complete_captured_chat_request(client).await;

        assert!(request.recv().unwrap().get("reasoning_effort").is_none());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn typed_compatibility_options_control_request_shape() {
        let (base_url, request, server) = request_capturing_server();
        let client = OpenAiCompatibleClient::new("key", &base_url, 2, 1)
            .unwrap()
            .with_compatibility(ChatCompatibilityOptions {
                enable_thinking: Some(false),
                send_temperature: false,
                output_token_parameter: OutputTokenParameter::MaxCompletionTokens,
                structured_output: StructuredOutputMode::JsonSchema,
                ..ChatCompatibilityOptions::default()
            });

        let result = client
            .chat_with_schema(
                "model",
                vec![json!({"role": "user", "content": "hi"})],
                10,
                Some(StructuredOutputSpec {
                    name: "test_output",
                    schema: json!({"type": "object"}),
                }),
            )
            .await
            .unwrap();

        assert_eq!(result.content, "ok");
        let request = request.recv().unwrap();
        assert_eq!(request["max_completion_tokens"], 10);
        assert!(request.get("max_tokens").is_none());
        assert!(request.get("temperature").is_none());
        assert_eq!(request["enable_thinking"], false);
        assert_eq!(request["response_format"]["type"], "json_schema");
        assert_eq!(
            request["response_format"]["json_schema"]["name"],
            "test_output"
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn reasoning_only_response_gets_one_corrective_retry() {
        let (base_url, requests, server) = sequence_server(vec![
            json!({
                "choices": [{"finish_reason": "length", "message": {"content": "", "reasoning_content": "private reasoning"}}],
                "usage": {"prompt_tokens": 2, "completion_tokens": 10, "total_tokens": 12}
            }),
            json!({
                "choices": [{"finish_reason": "stop", "message": {"content": "{\"ok\":true}", "reasoning_content": "private reasoning"}}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
            }),
        ]);
        let client = OpenAiCompatibleClient::new("key", &base_url, 2, 1)
            .unwrap()
            .with_compatibility(ChatCompatibilityOptions {
                reasoning_effort: Some("high".into()),
                reasoning_only_retry: true,
                ..ChatCompatibilityOptions::default()
            });

        let result = client
            .chat("model", vec![json!({"role": "user", "content": "hi"})], 64)
            .await
            .unwrap();

        assert_eq!(result.content, "{\"ok\":true}");
        let first = requests.recv().unwrap();
        let second = requests.recv().unwrap();
        assert_eq!(first["reasoning_effort"], "high");
        assert_eq!(second["reasoning_effort"], "none");
        assert!(second["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("final answer"));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn repeated_reasoning_only_response_fails_after_one_correction() {
        let (base_url, requests, server) = sequence_server(vec![
            json!({
                "choices": [{"finish_reason": "length", "message": {"content": "", "reasoning_content": "private reasoning"}}],
                "usage": {"prompt_tokens": 2, "completion_tokens": 10, "total_tokens": 12}
            }),
            json!({
                "choices": [{"finish_reason": "length", "message": {"content": "", "reasoning_content": "private reasoning again"}}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 10, "total_tokens": 13}
            }),
        ]);
        let client = OpenAiCompatibleClient::new("key", &base_url, 2, 1)
            .unwrap()
            .with_compatibility(ChatCompatibilityOptions {
                enable_thinking: Some(true),
                reasoning_only_retry: true,
                ..ChatCompatibilityOptions::default()
            });

        let error = client
            .chat("model", vec![json!({"role": "user", "content": "hi"})], 64)
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("reasoning content without final content"));
        let first = requests.recv().unwrap();
        let second = requests.recv().unwrap();
        assert_eq!(first["enable_thinking"], true);
        assert_eq!(second["enable_thinking"], false);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn content_with_reasoning_does_not_trigger_semantic_retry() {
        let (base_url, requests, server) = sequence_server(vec![json!({
            "choices": [{"finish_reason": "stop", "message": {"content": "{\"ok\":true}", "reasoning_content": "private reasoning"}}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 2, "total_tokens": 4}
        })]);
        let client = OpenAiCompatibleClient::new("key", &base_url, 2, 1)
            .unwrap()
            .with_compatibility(ChatCompatibilityOptions {
                reasoning_only_retry: true,
                ..ChatCompatibilityOptions::default()
            });

        let result = client
            .chat("model", vec![json!({"role": "user", "content": "hi"})], 64)
            .await
            .unwrap();

        assert_eq!(result.content, "{\"ok\":true}");
        assert_eq!(requests.recv().unwrap()["model"], "model");
        assert!(requests.try_recv().is_err());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn empty_content_without_reasoning_is_not_semantic_retryable() {
        let (base_url, requests, server) = sequence_server(vec![json!({
            "choices": [{"finish_reason": "stop", "message": {"content": ""}}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 0, "total_tokens": 2}
        })]);
        let client = OpenAiCompatibleClient::new("key", &base_url, 2, 1)
            .unwrap()
            .with_compatibility(ChatCompatibilityOptions {
                reasoning_only_retry: true,
                ..ChatCompatibilityOptions::default()
            });

        let error = client
            .chat("model", vec![json!({"role": "user", "content": "hi"})], 64)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("empty content"));
        assert_eq!(requests.recv().unwrap()["model"], "model");
        assert!(requests.try_recv().is_err());
        server.join().unwrap();
    }

    #[test]
    fn context_budget_is_checked_before_provider_call() {
        let client = OpenAiCompatibleClient::new("key", "http://localhost", 2, 1).unwrap();
        let messages = vec![json!({"role": "user", "content": "a deliberately long prompt"})];

        let error = client
            .validate_context_budget(&messages, 100, Some(32), 8)
            .unwrap_err();

        assert!(error.to_string().contains("context_budget_exceeded"));
        assert!(client
            .validate_context_budget(&messages, 100, None, 8)
            .is_ok());
    }

    #[tokio::test]
    async fn retries_invalid_success_json() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for body in [
                "not-json".to_owned(),
                serde_json::json!({
                    "choices": [{"message": {"content": "ok"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                })
                .to_string(),
            ] {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line
                        .to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                    {
                        content_length = value.parse().unwrap();
                    }
                }
                let mut request_body = vec![0u8; content_length];
                reader.read_exact(&mut request_body).unwrap();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let client =
            OpenAiCompatibleClient::new("key", &format!("http://{address}"), 2, 2).unwrap();

        let result = client
            .chat("model", vec![json!({"role": "user", "content": "hi"})], 10)
            .await
            .unwrap();

        assert_eq!(result.content, "ok");
        assert_eq!(result.usage.total_tokens, 2);
        server.join().unwrap();
    }
}
